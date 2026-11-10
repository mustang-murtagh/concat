// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! The jobs, and what can be done to one.
//!
//! A [`Studio`] is what a transport holds - the MCP server has one for as
//! long as it runs - and every tool an agent has is a method here.
//!
//! **Preparing** runs on a thread of the studio's own, one per job, so the
//! call that started it returns at once: fetching, transcribing and
//! listening take minutes for an hour of recording, and nothing that asks
//! over a wire should wait that long on one answer. The thread keeps the
//! job in memory as it goes and writes it to the job's folder at every
//! stage; [`Studio::wait`] watches it. Transcription takes turns - whisper
//! wants the whole machine - so a second job started meanwhile fetches its
//! recording and then queues.
//!
//! **Rendering** needs the editor, and the editor is the caller's
//! ([`Engine`]), so it happens inside [`Studio::render`]: clip by clip,
//! shape by shape, a project is built and handed to the engine's exporter,
//! and the call watches the export until its time is up. An export outlives
//! the call that began it; the next call picks up where the last left off,
//! from what the job's folder says. A render is therefore safe to ask for
//! again at any point: what is done is not done twice.
//!
//! Everything else - reading the transcript, changing the plan - is a job
//! read from its folder, changed and written back.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use concat_host::AppDirs;
use concat_speech::transcribe::Transcriber;
use serde_json::{Value, json};

use crate::compose::{self, Brief};
use crate::framing::{self, Choice, Look, Placement, Subject};
use crate::moments;
use crate::plan::{
    self, Clip, Folder, Job, Origin, Recording, RenderState, SourceType, Stage, Verdict,
};
use crate::settings::{ClipRules, Format, Settings};
use crate::sound::{self, Quiet, Tightened};
use crate::source::{self, Source};
use crate::transcript::{self, Transcript};
use crate::{Engine, Error, clock};

/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(250);
/// The shortest clip that can be planned, in seconds.
const SHORTEST_CLIP: f64 = 1.0;
/// The longest, in seconds: ten minutes is no longer a short.
const LONGEST_CLIP: f64 = 600.0;

/// A job being prepared in this process.
struct Preparing {
    /// The job as it stands.
    job: Mutex<Job>,
    /// Set to stop it.
    cancel: AtomicBool,
    /// Set while whisper is running for it, so a cancel can stop whisper.
    transcribing: AtomicBool,
    /// Set when its thread has ended.
    finished: AtomicBool,
}

/// What a job is started with.
#[derive(Clone, Debug, Default)]
pub struct StartOptions {
    /// A file's path or a link.
    pub source: String,
    /// What the recording is, when its maker says.
    pub source_type: SourceType,
    /// A job to carry on with, instead of beginning a new one: its
    /// recording and transcript are reused, and only what is missing is
    /// done.
    pub job: Option<String>,
    /// The shortest suggested clip, in seconds; the settings' when absent.
    pub min_seconds: Option<f64>,
    /// The longest.
    pub max_seconds: Option<f64>,
    /// The most suggestions.
    pub max_clips: Option<usize>,
}

/// A clip as somebody asks for it.
#[derive(Clone, Debug, Default)]
pub struct ClipSpec {
    /// An existing clip to change; a new one is made when absent.
    pub id: Option<String>,
    /// Where it starts, in seconds of the recording.
    pub start: Option<f64>,
    /// Where it ends.
    pub end: Option<f64>,
    /// What to call it; taken from its opening words when absent.
    pub title: Option<String>,
    /// How its picture is framed.
    pub framing: Option<Choice>,
    /// Where across the picture a crop centres, 0 to 1. `Some(None)`
    /// leaves it to be seen again.
    pub focus_x: Option<Option<f64>>,
    /// Whether it is approved; approved when absent, since a clip
    /// somebody gave the times for is a clip they want.
    pub approved: Option<bool>,
    /// Keep the times exactly as given, instead of moving them onto the
    /// nearest words.
    pub exact: bool,
}

/// What a render is asked for.
#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    /// The clips to render; every approved clip when absent.
    pub clips: Option<Vec<String>>,
    /// The formats to render them in; the enabled ones when absent.
    pub formats: Option<Vec<String>>,
    /// Render again what is already rendered.
    pub again: bool,
    /// How long this call may watch, in seconds.
    pub timeout: f64,
}

/// A frame of a clip, as a picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// The picture, a PNG in base64.
    pub png: String,
    /// Its width in pixels.
    pub width: u64,
    /// Its height in pixels.
    pub height: u64,
    /// The moment of the clip it shows, in seconds.
    pub time: f64,
    /// How the clip was framed, and anything left out.
    pub note: String,
}

/// The jobs of one workspace.
pub struct Studio {
    dirs: AppDirs,
    transcriber: Arc<Transcriber>,
    /// Whose turn it is to transcribe.
    turn: Arc<Mutex<()>>,
    preparing: Mutex<HashMap<String, Arc<Preparing>>>,
}

impl Studio {
    /// A studio over Concat's own folders: the settings in its config
    /// folder, the speech models in its data folder.
    pub fn new(dirs: AppDirs) -> Studio {
        Studio {
            dirs,
            transcriber: Arc::new(Transcriber::new()),
            turn: Arc::new(Mutex::new(())),
            preparing: Mutex::new(HashMap::new()),
        }
    }

    /// The settings in force.
    pub fn settings(&self) -> Result<Settings, Error> {
        Settings::load(&self.dirs)
    }

    /// Where the settings file is.
    pub fn settings_path(&self) -> PathBuf {
        Settings::path(&self.dirs)
    }

    /// Merges `patch` over the settings and saves them.
    pub fn update_settings(&self, patch: &Value) -> Result<Settings, Error> {
        let settings = self.settings()?.patched(patch)?;
        settings.save(&self.dirs)?;
        Ok(settings)
    }

    fn workspace(&self) -> Result<PathBuf, Error> {
        self.settings()?.workspace_dir()
    }

    fn folder(&self, id: &str) -> Result<Folder, Error> {
        if !plan::valid_id(id) {
            return Err(Error::NotFound(format!("there is no job called {id:?}")));
        }
        Ok(Folder::of(&self.workspace()?, id))
    }

    /// The job being prepared here under `id`, if it still is.
    fn live(&self, id: &str) -> Option<Arc<Preparing>> {
        let mut preparing = self.preparing.lock().ok()?;
        if preparing
            .get(id)
            .is_some_and(|job| job.finished.load(Ordering::Relaxed))
        {
            preparing.remove(id);
        }
        preparing.get(id).cloned()
    }

    /// A job as it stands: from the thread preparing it, or from its
    /// folder. A job its folder says is being prepared, with no thread
    /// here doing so, was cut short by a restart, and says so.
    pub fn status(&self, id: &str) -> Result<Job, Error> {
        if let Some(live) = self.live(id) {
            return live
                .job
                .lock()
                .map(|job| job.clone())
                .map_err(|_| Error::Failed("the job's state was lost".to_owned()));
        }
        let mut job = self.folder(id)?.load()?;
        if job.stage.working() {
            job.stage = Stage::Failed;
            job.progress = None;
            job.error = Some(
                "preparing was interrupted before it finished; start it again with this job's id to carry on from where it stopped".to_owned(),
            );
        }
        Ok(job)
    }

    /// Every job in the workspace, newest first.
    pub fn list(&self) -> Result<Vec<Job>, Error> {
        let workspace = self.workspace()?;
        let Ok(entries) = std::fs::read_dir(&workspace) else {
            return Ok(Vec::new());
        };
        let mut jobs: Vec<Job> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| plan::valid_id(name))
            .filter_map(|name| self.status(&name).ok())
            .collect();
        jobs.sort_by(|a, b| b.created.cmp(&a.created).then(a.id.cmp(&b.id)));
        Ok(jobs)
    }

    /// A job that is prepared, for something that needs its plan.
    fn ready(&self, id: &str) -> Result<(Folder, Job), Error> {
        let job = self.status(id)?;
        match job.stage {
            Stage::Ready => Ok((self.folder(id)?, job)),
            Stage::Failed | Stage::Cancelled => Err(Error::Failed(format!(
                "job {id} was not prepared: {}",
                job.error.as_deref().unwrap_or("it was cancelled")
            ))),
            _ => Err(Error::Failed(format!(
                "job {id} is still being prepared ({}); wait for it first",
                job.note
            ))),
        }
    }

    /// Begins preparing a job and returns it at once; [`Studio::wait`]
    /// follows it. With `options.job`, carries on with that job instead.
    pub fn start(&self, options: StartOptions) -> Result<Job, Error> {
        let settings = self.settings()?;
        let workspace = settings.workspace_dir()?;
        let mut rules = settings.clips.clone();
        if let Some(min) = options.min_seconds {
            rules.min_seconds = min;
        }
        if let Some(max) = options.max_seconds {
            rules.max_seconds = max;
        }
        if let Some(count) = options.max_clips {
            rules.max_clips = count;
        }
        check_rules(&rules)?;

        let job = match &options.job {
            Some(id) => {
                if self.live(id).is_some() {
                    return Err(Error::Failed(format!("job {id} is already being prepared")));
                }
                let mut job = self.folder(id)?.load()?;
                job.stage = Stage::Queued;
                job.error = None;
                job.progress = None;
                job.note = "waiting to start".to_owned();
                job
            }
            None => {
                let source = source::read(&options.source)?;
                let named = match &source {
                    Source::File(path) => path.to_string_lossy().into_owned(),
                    Source::Link(link) => link.clone(),
                };
                let stem = source::stem(&source);
                // The time makes two jobs on one recording two jobs.
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|since| since.as_nanos())
                    .unwrap_or_default();
                let mut id = format!("{stem}-{}", plan::tag(&format!("{named} {nanos}")));
                let mut salt = 0u32;
                while Folder::of(&workspace, &id).0.exists() {
                    salt += 1;
                    id = format!("{stem}-{}", plan::tag(&format!("{named} {nanos} {salt}")));
                }
                Job::begin(id, named, options.source_type)
            }
        };
        let folder = Folder::of(&workspace, &job.id);
        std::fs::create_dir_all(&folder.0).map_err(|error| {
            Error::Failed(format!("could not make {}: {error}", folder.0.display()))
        })?;
        folder.save(&job)?;

        let preparing = Arc::new(Preparing {
            job: Mutex::new(job.clone()),
            cancel: AtomicBool::new(false),
            transcribing: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        });
        if let Ok(mut all) = self.preparing.lock() {
            all.insert(job.id.clone(), Arc::clone(&preparing));
        }
        let worker = Worker {
            dirs: self.dirs.clone(),
            transcriber: Arc::clone(&self.transcriber),
            turn: Arc::clone(&self.turn),
            preparing: Arc::clone(&preparing),
            folder,
            settings,
            rules,
        };
        std::thread::Builder::new()
            .name(format!("shorts {}", job.id))
            .spawn(move || worker.run())
            .map_err(|error| Error::Failed(format!("could not start preparing: {error}")))?;
        Ok(job)
    }

    /// Watches a job until it is prepared, has failed, `timeout` has
    /// passed or `cancelled` is set, telling `progress` (done, of a
    /// hundred, and a sentence) as it moves. Returns the job as it then
    /// stands; one still working when the time is up can be waited on
    /// again.
    pub fn wait(
        &self,
        id: &str,
        timeout: Duration,
        cancelled: &AtomicBool,
        progress: Option<&dyn Fn(f64, f64, String)>,
    ) -> Result<Job, Error> {
        let until = Instant::now() + timeout;
        let mut reported = -1.0;
        loop {
            let job = self.status(id)?;
            if let Some(progress) = progress {
                let overall = overall(&job);
                if overall > reported {
                    reported = overall;
                    progress(overall, 100.0, job.note.clone());
                }
            }
            if !job.stage.working() || Instant::now() >= until || cancelled.load(Ordering::Relaxed)
            {
                return Ok(job);
            }
            std::thread::sleep(POLL);
        }
    }

    /// Stops whatever a job is doing: preparing, and any export of its
    /// clips the engine has running.
    pub fn cancel(&self, engine: &mut dyn Engine, id: &str) -> Result<Job, Error> {
        if let Some(live) = self.live(id) {
            live.cancel.store(true, Ordering::Relaxed);
            if live.transcribing.load(Ordering::Relaxed) {
                self.transcriber.cancel();
                self.transcriber.cancel_download();
            }
            // Its thread notices within a moment and writes the job.
            let until = Instant::now() + Duration::from_secs(10);
            while !live.finished.load(Ordering::Relaxed) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            return self.status(id);
        }
        let folder = self.folder(id)?;
        let mut job = folder.load()?;
        let mut changed = false;
        for render in &mut job.renders {
            if render.state != RenderState::Exporting {
                continue;
            }
            if let Some(export) = render.export.take() {
                let _ = compose::call(engine, json!({ "method": "export.cancel", "job": export }));
            }
            if let Some(project) = &render.project {
                let _ = compose::call(
                    engine,
                    json!({ "method": "project.close", "path": project, "save": false }),
                );
            }
            render.state = RenderState::Cancelled;
            changed = true;
        }
        if changed {
            folder.save(&job)?;
        }
        self.status(id)
    }

    /// What was said in a job's recording between `from` and `to` seconds,
    /// a sentence a line with its time; at most `limit` characters, with
    /// where the next page starts when there is more.
    pub fn transcript(
        &self,
        id: &str,
        from: f64,
        to: f64,
        limit: usize,
    ) -> Result<(String, Option<f64>, Job), Error> {
        let (folder, job) = self.ready(id)?;
        let transcript = Transcript::load(&folder.transcript()).unwrap_or_default();
        let (text, next) = transcript.page(from, to, limit.max(200));
        Ok((text, next, job))
    }

    /// Suggests moments afresh, by `rules`. Clips somebody chose or
    /// approved are kept, and so are rejected ones, so nothing over them
    /// is suggested again; the other suggestions are replaced.
    pub fn suggest(&self, id: &str, rules: &ClipRules) -> Result<Job, Error> {
        check_rules(rules)?;
        let (folder, mut job) = self.ready(id)?;
        let settings = self.settings()?;
        let transcript = Transcript::load(&folder.transcript()).unwrap_or_default();
        let quiet = Quiet::load(&folder.quiet()).unwrap_or_default();
        let standing: Vec<Clip> = job
            .clips
            .iter()
            .filter(|clip| clip.origin == Origin::Chosen || clip.verdict != Verdict::Proposed)
            .cloned()
            .collect();
        let dropped: Vec<String> = job
            .clips
            .iter()
            .filter(|clip| !standing.iter().any(|kept| kept.id == clip.id))
            .map(|clip| clip.id.clone())
            .collect();
        job.renders.retain(|render| !dropped.contains(&render.clip));
        job.clips = standing;
        // More than asked for, so that those overlapping a standing clip
        // can be passed over and the count still met.
        let generous = ClipRules {
            max_clips: rules.max_clips + job.clips.len(),
            ..rules.clone()
        };
        let mut added = 0;
        for moment in moments::suggest(&transcript, &quiet, &generous, &settings.tighten) {
            if added >= rules.max_clips {
                break;
            }
            let overlaps = job
                .clips
                .iter()
                .any(|clip| moment.start < clip.end && clip.start < moment.end);
            if overlaps {
                continue;
            }
            let id = job.mint_clip();
            job.clips.push(Clip {
                id,
                title: moment.title,
                start: moment.start,
                end: moment.end,
                length: moment.length,
                verdict: Verdict::Proposed,
                origin: Origin::Suggested,
                hook: moment.hook,
                reason: moment.reason,
                score: Some(moment.score),
                framing: Choice::Auto,
                focus_x: None,
                look: None,
            });
            added += 1;
        }
        job.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        folder.save(&job)?;
        Ok(job)
    }

    /// Adds or changes clips by their times. With `replace`, the plan
    /// becomes exactly these.
    pub fn set_clips(&self, id: &str, specs: Vec<ClipSpec>, replace: bool) -> Result<Job, Error> {
        if specs.is_empty() && !replace {
            return Err(Error::Invalid("no clips were given".to_owned()));
        }
        let (folder, mut job) = self.ready(id)?;
        let settings = self.settings()?;
        let recording = job
            .recording
            .clone()
            .ok_or_else(|| Error::Failed("the job has no recording".to_owned()))?;
        let transcript = Transcript::load(&folder.transcript()).unwrap_or_default();
        let quiet = Quiet::load(&folder.quiet()).unwrap_or_default();
        if replace {
            let named: Vec<&str> = specs.iter().filter_map(|spec| spec.id.as_deref()).collect();
            job.clips.retain(|clip| named.contains(&clip.id.as_str()));
            let kept: Vec<String> = job.clips.iter().map(|clip| clip.id.clone()).collect();
            job.renders.retain(|render| kept.contains(&render.clip));
        }
        for (number, spec) in specs.into_iter().enumerate() {
            let existing = match &spec.id {
                Some(id) => Some(job.clip(id).cloned().ok_or_else(|| {
                    Error::NotFound(format!("there is no clip {id} in this job"))
                })?),
                None => None,
            };
            let start = spec.start.or(existing.as_ref().map(|clip| clip.start));
            let end = spec.end.or(existing.as_ref().map(|clip| clip.end));
            let (Some(mut start), Some(mut end)) = (start, end) else {
                return Err(Error::Invalid(format!(
                    "clip {} needs a start and an end",
                    number + 1
                )));
            };
            let moved = spec.start.is_some() || spec.end.is_some();
            if moved {
                if !(start.is_finite() && end.is_finite() && start >= 0.0) {
                    return Err(Error::Invalid(format!(
                        "clip {}: its times are seconds from the start of the recording",
                        number + 1
                    )));
                }
                if start >= recording.duration {
                    return Err(Error::Invalid(format!(
                        "clip {} starts at {}, but the recording ends at {}",
                        number + 1,
                        clock(start),
                        clock(recording.duration)
                    )));
                }
                end = end.min(recording.duration);
                if !spec.exact {
                    (start, end) = transcript.snap(start, end);
                    end = end.min(recording.duration);
                }
                if end - start < SHORTEST_CLIP {
                    return Err(Error::Invalid(format!(
                        "clip {} would run {:.1} s; the shortest is {SHORTEST_CLIP} s",
                        number + 1,
                        end - start
                    )));
                }
                if end - start > LONGEST_CLIP {
                    return Err(Error::Invalid(format!(
                        "clip {} would run {}; the longest is {}",
                        number + 1,
                        clock(end - start),
                        clock(LONGEST_CLIP)
                    )));
                }
            }
            let (title, hook) = moments::describe(&transcript, start, end);
            let length = Tightened::of(start, end, &quiet, &settings.tighten).length();
            let given_title = spec
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(str::to_owned);
            match existing {
                Some(old) => {
                    let framing = spec.framing.unwrap_or(old.framing);
                    let focus_x = spec.focus_x.unwrap_or(old.focus_x);
                    let reframed = framing != old.framing || focus_x != old.focus_x;
                    let retitled = given_title
                        .as_ref()
                        .is_some_and(|title| *title != old.title);
                    let clip_id = old.id.clone();
                    let Some(clip) = job.clip_mut(&clip_id) else {
                        continue;
                    };
                    if moved {
                        clip.start = start;
                        clip.end = end;
                        clip.length = length;
                        clip.hook = hook;
                        // What was seen was seen of the old stretch.
                        clip.look = None;
                        if given_title.is_none() && old.origin == Origin::Suggested {
                            clip.title = title;
                        }
                        // No longer the tool's suggestion.
                        clip.origin = Origin::Chosen;
                        clip.reason = String::new();
                        clip.score = None;
                    }
                    if let Some(title) = given_title {
                        clip.title = title;
                    }
                    clip.framing = framing;
                    clip.focus_x = focus_x;
                    if let Some(approved) = spec.approved {
                        clip.verdict = if approved {
                            Verdict::Approved
                        } else {
                            Verdict::Proposed
                        };
                    }
                    // What was rendered of it is not this clip any more.
                    if moved || reframed || retitled {
                        job.renders.retain(|render| render.clip != clip_id);
                    }
                }
                None => {
                    let id = job.mint_clip();
                    job.clips.push(Clip {
                        id,
                        title: given_title.unwrap_or(title),
                        start,
                        end,
                        length,
                        verdict: if spec.approved.unwrap_or(true) {
                            Verdict::Approved
                        } else {
                            Verdict::Proposed
                        },
                        origin: Origin::Chosen,
                        hook,
                        reason: String::new(),
                        score: None,
                        framing: spec.framing.unwrap_or_default(),
                        focus_x: spec.focus_x.flatten(),
                        look: None,
                    });
                }
            }
        }
        for clip in &job.clips {
            if clip
                .focus_x
                .is_some_and(|focus| !(0.0..=1.0).contains(&focus))
            {
                return Err(Error::Invalid(format!(
                    "clip {}: focusX is 0 (left) to 1 (right)",
                    clip.id
                )));
            }
        }
        job.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        folder.save(&job)?;
        Ok(job)
    }

    /// Approves, rejects and removes clips by id. `approve_all` approves
    /// every clip still proposed.
    pub fn review(
        &self,
        id: &str,
        approve: &[String],
        approve_all: bool,
        reject: &[String],
        remove: &[String],
    ) -> Result<Job, Error> {
        let (folder, mut job) = self.ready(id)?;
        for named in approve.iter().chain(reject).chain(remove) {
            if job.clip(named).is_none() {
                return Err(Error::NotFound(format!(
                    "there is no clip {named} in this job; its clips are {}",
                    job.clips
                        .iter()
                        .map(|clip| clip.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
        for clip in &mut job.clips {
            if approve.contains(&clip.id) || (approve_all && clip.verdict == Verdict::Proposed) {
                clip.verdict = Verdict::Approved;
            }
            if reject.contains(&clip.id) {
                clip.verdict = Verdict::Rejected;
            }
        }
        job.clips.retain(|clip| !remove.contains(&clip.id));
        job.renders.retain(|render| !remove.contains(&render.clip));
        folder.save(&job)?;
        Ok(job)
    }

    /// Everything a clip's build needs that is read from its job's folder.
    fn material(&self, folder: &Folder, job: &Job) -> Result<Material, Error> {
        let media = job
            .media
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| Error::Failed("the job has no recording".to_owned()))?;
        if !media.is_file() {
            return Err(Error::NotFound(format!(
                "the recording is no longer at {}",
                media.display()
            )));
        }
        let recording = job
            .recording
            .clone()
            .ok_or_else(|| Error::Failed("the job has no recording".to_owned()))?;
        Ok(Material {
            media,
            recording,
            transcript: Transcript::load(&folder.transcript()).unwrap_or_default(),
            quiet: Quiet::load(&folder.quiet()).unwrap_or_default(),
        })
    }

    /// Builds the project for one clip in one format, looking at the
    /// clip's picture first if nobody has. Returns what was built and how
    /// it was framed.
    fn build(
        &self,
        engine: &mut dyn Engine,
        folder: &Folder,
        job: &mut Job,
        material: &Material,
        settings: &Settings,
        clip_id: &str,
        format: &Format,
        name: &str,
        cancelled: &AtomicBool,
    ) -> Result<(compose::Built, String), Error> {
        let clip = job
            .clip(clip_id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("there is no clip {clip_id} in this job")))?;
        let tightened = Tightened::of(clip.start, clip.end, &material.quiet, &settings.tighten);
        let (from, to) = match (tightened.kept.first(), tightened.kept.last()) {
            (Some(first), Some(last)) => (first.0, last.1),
            _ => (clip.start, clip.end),
        };
        let picture = (material.recording.width, material.recording.height);
        let frame = (format.width, format.height);
        // What the job's maker said it is settles what the clip's own
        // choice leaves open.
        let choice = match (clip.framing, job.source_type) {
            (Choice::Auto, SourceType::TalkingHead) => Choice::Crop,
            (Choice::Auto, SourceType::Screen) => Choice::Fit,
            (choice, _) => choice,
        };
        let mut notes: Vec<String> = Vec::new();
        let same_shape = matches!(
            framing::place(picture, frame, Choice::Fit, settings.reframe.mode, None),
            Placement::Fill { .. }
        );
        let mut look = clip.look;
        if look.is_none() && !same_shape && choice != Choice::Fit {
            match framing::look(&material.media, from, to, cancelled) {
                Ok(seen) => {
                    look = Some(seen);
                    if let Some(clip) = job.clip_mut(clip_id) {
                        clip.look = Some(seen);
                    }
                    folder.save(job)?;
                }
                Err(error) => notes.push(format!(
                    "the picture could not be looked at ({error}), so it was framed without knowing where the speaker is"
                )),
            }
        }
        // A centre chosen by hand is where the person is, whatever was seen.
        let seen = match (clip.focus_x, look) {
            (Some(focus_x), seen) => Some(Look {
                subject: Subject::Person,
                focus_x,
                focus_y: seen.map_or(0.35, |look| look.focus_y),
                covers: seen.map_or(0.0, |look| look.covers),
            }),
            (None, seen) => seen,
        };
        let placement =
            framing::place(picture, frame, choice, settings.reframe.mode, seen.as_ref());
        notes.insert(
            0,
            match (placement, same_shape, seen.map(|look| look.subject)) {
                (_, true, _) => "the picture already has this shape".to_owned(),
                (Placement::Fit, _, Some(Subject::Screen)) => {
                    "shown whole over a blurred copy: nobody fills the picture, so it was taken for a screen recording".to_owned()
                }
                (Placement::Fit, _, Some(Subject::Group)) => {
                    "shown whole over a blurred copy: there are people across the width of the picture".to_owned()
                }
                (Placement::Fit, _, _) => "shown whole over a blurred copy".to_owned(),
                (Placement::Fill { .. }, _, _) if clip.focus_x.is_some() => {
                    "cropped in, centred where it was set by hand".to_owned()
                }
                (Placement::Fill { .. }, _, Some(Subject::Person)) => {
                    "cropped in on the speaker".to_owned()
                }
                (Placement::Fill { .. }, _, _) => "cropped to the middle of the picture".to_owned(),
            },
        );
        let words = material.transcript.between(from, to);
        let location = folder.projects();
        let built = compose::build(
            engine,
            &Brief {
                settings,
                format,
                media: &material.media,
                recording: &material.recording,
                tightened: &tightened,
                words,
                placement,
                location: &location,
                name,
            },
        )?;
        notes.extend(built.notes.iter().cloned());
        let note = notes.join("; ");
        Ok((built, note))
    }

    /// Renders a job's approved clips in every format asked for, watching
    /// until all are done, `options.timeout` has passed or `cancelled` is
    /// set. Returns the job as it then stands; call again to carry on.
    pub fn render(
        &self,
        engine: &mut dyn Engine,
        id: &str,
        options: &RenderOptions,
        cancelled: &AtomicBool,
        progress: Option<&dyn Fn(f64, f64, String)>,
    ) -> Result<Job, Error> {
        let (folder, mut job) = self.ready(id)?;
        let settings = self.settings()?;
        let material = self.material(&folder, &job)?;

        let formats: Vec<Format> = match &options.formats {
            Some(ids) => ids
                .iter()
                .map(|id| {
                    settings.format(id).cloned().ok_or_else(|| {
                        Error::NotFound(format!(
                            "there is no format {id:?}; the formats are {}",
                            settings
                                .formats
                                .iter()
                                .map(|format| format.id.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })
                })
                .collect::<Result<_, _>>()?,
            None => settings
                .formats
                .iter()
                .filter(|format| format.enabled)
                .cloned()
                .collect(),
        };
        if formats.is_empty() {
            return Err(Error::Invalid(
                "no format is enabled in the settings, and none was asked for".to_owned(),
            ));
        }
        let clips: Vec<String> = match &options.clips {
            Some(ids) => {
                for id in ids {
                    let clip = job.clip(id).ok_or_else(|| {
                        Error::NotFound(format!("there is no clip {id} in this job"))
                    })?;
                    if clip.verdict != Verdict::Approved {
                        return Err(Error::Invalid(format!(
                            "clip {id} is not approved; approve it first"
                        )));
                    }
                }
                ids.clone()
            }
            None => job
                .clips
                .iter()
                .filter(|clip| clip.verdict == Verdict::Approved)
                .map(|clip| clip.id.clone())
                .collect(),
        };
        if clips.is_empty() {
            return Err(Error::Invalid(
                "no clip is approved yet; approve the ones to make, or give clips by their times"
                    .to_owned(),
            ));
        }

        // The queue, in the plan's order. What is done stays done unless
        // asked for again or its file has gone; what failed is tried again.
        let mut queue: Vec<(String, String)> = Vec::new();
        for clip in &clips {
            for format in &formats {
                let render = job.render_mut(clip, &format.id);
                let file_there = render
                    .output
                    .as_deref()
                    .is_some_and(|output| Path::new(output).is_file());
                match render.state {
                    RenderState::Done if file_there && !options.again => {}
                    RenderState::Exporting => queue.push((clip.clone(), format.id.clone())),
                    _ => {
                        render.state = RenderState::Queued;
                        render.error = None;
                        render.export = None;
                        queue.push((clip.clone(), format.id.clone()));
                    }
                }
            }
        }
        // An export already running goes first: the engine runs one at a
        // time, and nothing else can start until it ends.
        queue.sort_by_key(|(clip, format)| {
            job.render(clip, format)
                .is_none_or(|render| render.state != RenderState::Exporting)
        });
        folder.save(&job)?;

        let total = queue.len().max(1) as f64;
        let until = Instant::now() + Duration::from_secs_f64(options.timeout.max(0.0));
        let mut reported = -1.0;
        let mut report = |done: usize, fraction: f64, message: String| {
            if let Some(progress) = progress {
                let overall =
                    ((done as f64 + fraction.clamp(0.0, 1.0)) / total * 1000.0).round() / 10.0;
                if overall > reported {
                    reported = overall;
                    progress(overall.min(100.0), 100.0, message);
                }
            }
        };

        for (done, (clip_id, format_id)) in queue.iter().enumerate() {
            let Some(format) = formats.iter().find(|format| format.id == *format_id) else {
                continue;
            };
            let title = job
                .clip(clip_id)
                .map(|clip| clip.title.clone())
                .unwrap_or_default();
            let label = format!("{title} ({})", format.label);

            // Build and start the export, unless one is under way.
            let exporting = job.render(clip_id, format_id).is_some_and(|render| {
                render.state == RenderState::Exporting && render.export.is_some()
            });
            if !exporting {
                if Instant::now() >= until || cancelled.load(Ordering::Relaxed) {
                    return Ok(job);
                }
                report(done, 0.0, format!("building {label}"));
                // The project a render of this clip in this shape left
                // before is replaced by the new one: it is this tool's own,
                // in the job's projects folder, and what it held is about
                // to be rebuilt.
                if let Some(stale) = job
                    .render(clip_id, format_id)
                    .and_then(|render| render.project.clone())
                {
                    discard_project(engine, &folder, &stale);
                }
                let name = free_name(&folder.projects(), &format!("{clip_id}-{format_id}"));
                let output = output_path(&folder, &job, clip_id, &title, format_id);
                let started = self
                    .build(
                        engine, &folder, &mut job, &material, &settings, clip_id, format, &name,
                        cancelled,
                    )
                    .and_then(|(built, note)| {
                        if let Some(parent) = output.parent() {
                            std::fs::create_dir_all(parent).map_err(|error| {
                                Error::Failed(format!("could not make {}: {error}", parent.display()))
                            })?;
                        }
                        let export = compose::call(
                            engine,
                            json!({
                                "method": "export.run",
                                "path": built.project,
                                "output": output.to_string_lossy(),
                                "codec": settings.export.codec,
                                "crf": settings.export.crf,
                                "preset": settings.export.preset,
                            }),
                        );
                        match export {
                            Ok(started) => Ok((built, note, started)),
                            Err(error) => {
                                let _ = compose::call(
                                    engine,
                                    json!({ "method": "project.close", "path": built.project, "save": false }),
                                );
                                Err(error)
                            }
                        }
                    });
                let render = job.render_mut(clip_id, format_id);
                match started {
                    Ok((built, note, started)) => {
                        render.state = RenderState::Exporting;
                        render.project = Some(built.project);
                        render.output = Some(output.to_string_lossy().into_owned());
                        render.export = started["job"].as_str().map(str::to_owned);
                        render.note = note;
                        render.seconds = Some((built.total * 10.0).round() / 10.0);
                        render.error = None;
                    }
                    Err(error) => {
                        render.state = RenderState::Failed;
                        render.error = Some(error.message().to_owned());
                        render.export = None;
                    }
                }
                folder.save(&job)?;
                if job
                    .render(clip_id, format_id)
                    .is_some_and(|render| render.state == RenderState::Failed)
                {
                    continue;
                }
            }

            // Watch the export.
            loop {
                let (export, project) = {
                    let render = job.render_mut(clip_id, format_id);
                    (
                        render.export.clone().unwrap_or_default(),
                        render.project.clone(),
                    )
                };
                let status =
                    compose::call(engine, json!({ "method": "export.status", "job": export }))
                        .and_then(|status| {
                            // An engine restarted since names its exports
                            // from the start again: a job of this name that
                            // is rendering some other project is not this
                            // one.
                            let same = match (&project, status["path"].as_str()) {
                                (Some(project), Some(path)) => project == path,
                                _ => true,
                            };
                            if same {
                                Ok(status)
                            } else {
                                Err(Error::Failed("the engine was restarted".to_owned()))
                            }
                        });
                let ended = match &status {
                    Ok(status) => {
                        let state = status["state"].as_str().unwrap_or_default();
                        if state == "running" {
                            let stage = status["stage"].as_str().unwrap_or("starting");
                            // The frame loop is nearly all of an export.
                            let fraction = match stage {
                                "rendering" | "rendering in software" | "video" => {
                                    0.05 + status["fraction"].as_f64().unwrap_or(0.0) * 0.85
                                }
                                "mixing audio" | "audio" => 0.92,
                                "muxing" | "mux" => 0.97,
                                _ => 0.02,
                            };
                            report(done, fraction, format!("{stage}: {label}"));
                            None
                        } else if state == "done" {
                            Some(Ok(()))
                        } else if state == "cancelled" {
                            Some(Err((
                                RenderState::Cancelled,
                                "the export was cancelled".to_owned(),
                            )))
                        } else {
                            Some(Err((
                                RenderState::Failed,
                                status["error"]["message"]
                                    .as_str()
                                    .unwrap_or("the export failed")
                                    .to_owned(),
                            )))
                        }
                    }
                    // The engine does not know the export: it was
                    // restarted since. The clip goes back in the queue for
                    // the next call.
                    Err(error) => Some(Err((
                        RenderState::Queued,
                        format!("the export was lost ({error}); it will be rendered again"),
                    ))),
                };
                if let Some(outcome) = ended {
                    if let Some(project) = &project {
                        let _ = compose::call(
                            engine,
                            json!({ "method": "project.close", "path": project, "save": false }),
                        );
                    }
                    let render = job.render_mut(clip_id, format_id);
                    render.export = None;
                    match outcome {
                        Ok(()) => {
                            render.state = RenderState::Done;
                            render.error = None;
                        }
                        Err((state, why)) => {
                            render.state = state;
                            render.error = Some(why);
                        }
                    }
                    folder.save(&job)?;
                    report(done + 1, 0.0, format!("finished {label}"));
                    break;
                }
                if Instant::now() >= until || cancelled.load(Ordering::Relaxed) {
                    return Ok(job);
                }
                std::thread::sleep(POLL);
            }
        }
        Ok(job)
    }

    /// One frame of a clip as it will render in a format: its project is
    /// built, a frame composited `at` seconds into it (a third of the way
    /// through when absent) and the project put away again.
    pub fn preview(
        &self,
        engine: &mut dyn Engine,
        id: &str,
        clip_id: &str,
        format_id: &str,
        at: Option<f64>,
        cancelled: &AtomicBool,
    ) -> Result<Frame, Error> {
        let (folder, mut job) = self.ready(id)?;
        let settings = self.settings()?;
        let material = self.material(&folder, &job)?;
        let format = settings
            .format(format_id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("there is no format {format_id:?}")))?;
        let name = free_name(
            &folder.projects(),
            &format!("preview-{clip_id}-{format_id}"),
        );
        let (built, note) = self.build(
            engine, &folder, &mut job, &material, &settings, clip_id, &format, &name, cancelled,
        )?;
        let time = at
            .unwrap_or(built.content / 3.0)
            .clamp(0.0, (built.total - 0.05).max(0.0));
        // Within 1280 pixels, for a picture an agent can look at.
        let scale = (1280.0 / f64::from(format.width.max(format.height))).min(1.0);
        let even = |side: u32| (((f64::from(side) * scale) / 2.0).round() as u32 * 2).max(2);
        let picture = compose::call(
            engine,
            json!({
                "method": "preview.frame",
                "path": built.project,
                "time": time,
                "width": even(format.width),
                "height": even(format.height),
            }),
        );
        // A preview's project is scratch: made a moment ago, by this, in
        // the job's own projects folder.
        discard_project(engine, &folder, &built.project);
        let picture = picture?;
        let png = picture["png"]
            .as_str()
            .ok_or_else(|| Error::Failed("the engine returned no picture".to_owned()))?
            .to_owned();
        Ok(Frame {
            png,
            width: picture["width"].as_u64().unwrap_or_default(),
            height: picture["height"].as_u64().unwrap_or_default(),
            time,
            note,
        })
    }
}

/// What a build reads from the job's folder.
struct Material {
    media: PathBuf,
    recording: Recording,
    transcript: Transcript,
    quiet: Quiet,
}

/// Clip rules that make sense, or what is wrong with them.
fn check_rules(rules: &ClipRules) -> Result<(), Error> {
    if !(rules.min_seconds.is_finite() && rules.min_seconds >= 3.0) {
        return Err(Error::Invalid(
            "the shortest clip is at least 3 seconds".to_owned(),
        ));
    }
    if !(rules.max_seconds.is_finite() && rules.max_seconds <= LONGEST_CLIP) {
        return Err(Error::Invalid(format!(
            "the longest clip is at most {LONGEST_CLIP} seconds"
        )));
    }
    if rules.max_seconds < rules.min_seconds {
        return Err(Error::Invalid(
            "the longest clip is not shorter than the shortest".to_owned(),
        ));
    }
    if !(1..=50).contains(&rules.max_clips) {
        return Err(Error::Invalid("the number of clips is 1 to 50".to_owned()));
    }
    Ok(())
}

/// How far through preparing a job is, of a hundred: each stage a share,
/// and the stage's own progress within it.
fn overall(job: &Job) -> f64 {
    let (from, to) = match job.stage {
        Stage::Queued => (0.0, 1.0),
        Stage::Fetching => (1.0, 25.0),
        Stage::Transcribing => (25.0, 88.0),
        Stage::Listening => (88.0, 97.0),
        Stage::Suggesting => (97.0, 99.0),
        Stage::Ready | Stage::Failed | Stage::Cancelled => return 100.0,
    };
    let within = job.progress.unwrap_or(0.0).clamp(0.0, 1.0);
    ((from + (to - from) * within) * 10.0).round() / 10.0
}

/// `name`, or `name-2`, `name-3`..., whichever is not yet a folder in
/// `location`: a project is never built over another.
fn free_name(location: &Path, name: &str) -> String {
    if !location.join(name).exists() {
        return name.to_owned();
    }
    (2..)
        .map(|revision| format!("{name}-{revision}"))
        .find(|candidate| !location.join(candidate).exists())
        .unwrap_or_else(|| name.to_owned())
}

/// Closes and removes a project this tool built, when it is where this
/// tool builds them: directly inside the job's projects folder. Anything
/// else is left alone.
fn discard_project(engine: &mut dyn Engine, folder: &Folder, project: &str) {
    let path = Path::new(project);
    if path.parent() != Some(folder.projects().as_path()) || !path.is_dir() {
        return;
    }
    let _ = compose::call(
        engine,
        json!({ "method": "project.close", "path": project, "save": false }),
    );
    if let Err(error) = std::fs::remove_dir_all(path) {
        log::warn!("shorts: could not remove {}: {error}", path.display());
    }
}

/// Where a clip's file in a format is written: `clips/03-its-title-vertical.mp4`.
fn output_path(folder: &Folder, job: &Job, clip_id: &str, title: &str, format: &str) -> PathBuf {
    // Numbered by the clip's place in the plan, so the files sort as the
    // recording runs.
    let place = job
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .map_or(0, |at| at + 1);
    folder.clips().join(format!(
        "{place:02}-{}-{clip_id}-{format}.mp4",
        source::slug(title, 48)
    ))
}

/// The thread that prepares one job.
struct Worker {
    dirs: AppDirs,
    transcriber: Arc<Transcriber>,
    turn: Arc<Mutex<()>>,
    preparing: Arc<Preparing>,
    folder: Folder,
    settings: Settings,
    rules: ClipRules,
}

impl Worker {
    /// Changes the job, in memory; `save` also writes it to its folder.
    fn update(&self, save: bool, change: impl FnOnce(&mut Job)) {
        let Ok(mut job) = self.preparing.job.lock() else {
            return;
        };
        change(&mut job);
        if save && let Err(error) = self.folder.save(&job) {
            log::warn!("shorts: {error}");
        }
    }

    fn stage(&self, stage: Stage, note: &str) {
        self.update(true, |job| {
            job.stage = stage;
            job.note = note.to_owned();
            job.progress = None;
        });
    }

    fn progress(&self, fraction: Option<f64>, note: &str) {
        self.update(false, |job| {
            job.progress = fraction;
            job.note = note.to_owned();
        });
    }

    fn cancelled(&self) -> bool {
        self.preparing.cancel.load(Ordering::Relaxed)
    }

    fn run(self) {
        let outcome = self.prepare();
        let cancelled = self.cancelled();
        self.update(true, |job| {
            job.progress = None;
            match outcome {
                Ok(()) => {
                    job.stage = Stage::Ready;
                    job.error = None;
                    job.note = match job.clips.len() {
                        0 => "prepared; no moments were suggested".to_owned(),
                        1 => "prepared; 1 clip in the plan".to_owned(),
                        count => format!("prepared; {count} clips in the plan"),
                    };
                }
                Err(_) if cancelled => {
                    job.stage = Stage::Cancelled;
                    job.note = "cancelled".to_owned();
                    job.error = None;
                }
                Err(error) => {
                    job.stage = Stage::Failed;
                    job.note = "failed".to_owned();
                    job.error = Some(error.message().to_owned());
                }
            }
        });
        self.preparing.finished.store(true, Ordering::Relaxed);
    }

    fn stop_if_cancelled(&self) -> Result<(), Error> {
        if self.cancelled() {
            Err(Error::Failed("cancelled".to_owned()))
        } else {
            Ok(())
        }
    }

    fn prepare(&self) -> Result<(), Error> {
        let (source, known_media) = {
            let job = self
                .preparing
                .job
                .lock()
                .map_err(|_| Error::Failed("the job's state was lost".to_owned()))?;
            (job.source.clone(), job.media.clone())
        };

        // The recording, as a file here.
        let media = match source::read(&source)? {
            Source::File(path) => path,
            Source::Link(link) => {
                match known_media.map(PathBuf::from).filter(|path| path.is_file()) {
                    Some(path) => path,
                    None => {
                        self.stage(Stage::Fetching, "fetching the recording");
                        source::fetch(
                            &link,
                            &self.folder.0,
                            &self.settings.download,
                            &self.preparing.cancel,
                            &mut |fraction, note| self.progress(fraction, note),
                        )?
                    }
                }
            }
        };
        self.stop_if_cancelled()?;

        // What it is.
        let info = concat_media::probe(&media).map_err(|error| {
            Error::Failed(format!(
                "{} does not read as a video: {error}",
                media.display()
            ))
        })?;
        let Some(video) = &info.video else {
            return Err(Error::Invalid(format!(
                "{} has sound but no picture; a short needs a video",
                media.display()
            )));
        };
        let duration = info
            .duration
            .map(|duration| duration.as_f64())
            .unwrap_or(0.0);
        if duration < SHORTEST_CLIP {
            return Err(Error::Invalid(format!(
                "{} is {duration:.1} s long; there is nothing to cut",
                media.display()
            )));
        }
        let recording = Recording {
            duration,
            width: video.width,
            height: video.height,
            fps: {
                let fps = video.frame_rate.fps().as_f64();
                if fps.is_finite() && fps > 0.0 {
                    fps
                } else {
                    30.0
                }
            },
        };
        self.update(true, |job| {
            job.media = Some(media.to_string_lossy().into_owned());
            job.recording = Some(recording.clone());
        });

        // What is said in it. A transcript from a run before is kept when
        // the same model made it.
        let model = self.settings.transcription.model.clone();
        let kept = Transcript::load(&self.folder.transcript()).filter(|transcript| {
            transcript.model == model && (transcript.duration - duration).abs() < 1.0
        });
        let transcript = match kept {
            Some(transcript) => transcript,
            None if info.audio.is_none() => Transcript {
                model: model.clone(),
                duration,
                words: Vec::new(),
            },
            None => {
                self.stage(Stage::Queued, "waiting for another transcription to finish");
                let turn = self
                    .turn
                    .lock()
                    .map_err(|_| Error::Failed("the transcriber was lost".to_owned()))?;
                self.stop_if_cancelled()?;
                self.stage(Stage::Transcribing, "getting the speech model ready");
                self.preparing.transcribing.store(true, Ordering::Relaxed);
                let outcome = transcript::ensure_model(
                    &self.dirs,
                    &self.transcriber,
                    &model,
                    &mut |fraction, note| {
                        // The download is a small first part of the stage.
                        self.progress(fraction.map(|fraction| fraction * 0.05), note);
                    },
                )
                .and_then(|()| {
                    let preparing = Arc::clone(&self.preparing);
                    transcript::transcribe(
                        &self.dirs,
                        &self.transcriber,
                        &media,
                        duration,
                        &model,
                        &self.preparing.cancel,
                        move |fraction, note| {
                            if let Ok(mut job) = preparing.job.lock() {
                                job.progress = fraction.map(|fraction| 0.05 + fraction * 0.95);
                                job.note = note.to_owned();
                            }
                        },
                    )
                });
                self.preparing.transcribing.store(false, Ordering::Relaxed);
                drop(turn);
                let transcript = outcome?;
                transcript.save(&self.folder.transcript())?;
                transcript
            }
        };
        self.update(true, |job| job.words = transcript.words.len());
        self.stop_if_cancelled()?;

        // Where it goes quiet.
        let quiet = match Quiet::load(&self.folder.quiet()) {
            Some(quiet) => quiet,
            None if info.audio.is_none() => Quiet::default(),
            None => {
                self.stage(Stage::Listening, "listening for pauses");
                let levels = sound::levels(
                    &media,
                    duration,
                    &self.preparing.cancel,
                    &mut |fraction, note| {
                        self.progress(fraction, note);
                    },
                )?;
                let quiet = sound::quiet(&levels);
                quiet.save(&self.folder.quiet())?;
                quiet
            }
        };
        self.stop_if_cancelled()?;

        // What is worth cutting: suggested once, and never over a plan
        // somebody has already worked on.
        let planned = self
            .preparing
            .job
            .lock()
            .map(|job| !job.clips.is_empty())
            .unwrap_or(false);
        if !planned {
            self.stage(Stage::Suggesting, "picking moments");
            let moments =
                moments::suggest(&transcript, &quiet, &self.rules, &self.settings.tighten);
            self.update(true, |job| {
                for moment in moments {
                    let id = job.mint_clip();
                    job.clips.push(Clip {
                        id,
                        title: moment.title,
                        start: moment.start,
                        end: moment.end,
                        length: moment.length,
                        verdict: Verdict::Proposed,
                        origin: Origin::Suggested,
                        hook: moment.hook,
                        reason: moment.reason,
                        score: Some(moment.score),
                        framing: Choice::Auto,
                        focus_x: None,
                        look: None,
                    });
                }
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_runs_through_the_stages() {
        let mut job = Job::begin("a".to_owned(), "/a.mp4".to_owned(), SourceType::Auto);
        assert_eq!(overall(&job), 0.0);
        job.stage = Stage::Transcribing;
        job.progress = Some(0.5);
        assert_eq!(overall(&job), 56.5);
        job.stage = Stage::Listening;
        job.progress = None;
        assert_eq!(overall(&job), 88.0);
        job.stage = Stage::Ready;
        assert_eq!(overall(&job), 100.0);
    }

    #[test]
    fn a_project_is_never_built_over_another() {
        let folder = tempfile::tempdir().expect("a scratch folder");
        assert_eq!(free_name(folder.path(), "c1-vertical"), "c1-vertical");
        std::fs::create_dir(folder.path().join("c1-vertical")).expect("makes");
        assert_eq!(free_name(folder.path(), "c1-vertical"), "c1-vertical-2");
        std::fs::create_dir(folder.path().join("c1-vertical-2")).expect("makes");
        assert_eq!(free_name(folder.path(), "c1-vertical"), "c1-vertical-3");
    }

    #[test]
    fn rules_out_of_range_are_refused() {
        let rules = |min: f64, max: f64, clips: usize| ClipRules {
            min_seconds: min,
            max_seconds: max,
            max_clips: clips,
        };
        assert!(check_rules(&rules(60.0, 90.0, 6)).is_ok());
        assert!(check_rules(&rules(1.0, 90.0, 6)).is_err());
        assert!(check_rules(&rules(90.0, 60.0, 6)).is_err());
        assert!(check_rules(&rules(60.0, 9000.0, 6)).is_err());
        assert!(check_rules(&rules(60.0, 90.0, 0)).is_err());
    }

    #[test]
    fn a_file_is_named_for_its_place_its_title_and_its_shape() {
        let workspace = tempfile::tempdir().expect("a scratch folder");
        let folder = Folder::of(workspace.path(), "talk-1a2b");
        let mut job = Job::begin(
            "talk-1a2b".to_owned(),
            "/a.mp4".to_owned(),
            SourceType::Auto,
        );
        for (id, start) in [("c4", 10.0), ("c2", 90.0)] {
            job.clips.push(Clip {
                id: id.to_owned(),
                title: String::new(),
                start,
                end: start + 60.0,
                length: 60.0,
                verdict: Verdict::Approved,
                origin: Origin::Chosen,
                hook: String::new(),
                reason: String::new(),
                score: None,
                framing: Choice::Auto,
                focus_x: None,
                look: None,
            });
        }
        let path = output_path(&folder, &job, "c2", "Why most prompts fail!", "vertical");
        assert_eq!(
            path,
            folder
                .clips()
                .join("02-why-most-prompts-fail-c2-vertical.mp4")
        );
    }
}
