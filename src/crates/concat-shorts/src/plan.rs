// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! A job: one recording, the clips planned from it, and how far each has
//! got.
//!
//! A job is a folder in the workspace, and `job.json` in it is everything
//! here, rewritten whole whenever something changes. Beside it sit the
//! recording itself when it was fetched from a link (`source.*`), what was
//! said (`transcript.json`), where it goes quiet (`quiet.json`), the
//! Concat project behind each rendered clip (`projects/`) and the finished
//! files (`clips/`). Nothing lives only in memory: a server that is
//! restarted reads the folder and carries on, and a person can look at
//! any of it.
//!
//! The plan is the list of clips. A clip is proposed until it is approved,
//! and only approved clips render - whether the tool suggested it, an
//! agent picked it from the transcript, or a person gave its times.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::framing::{Choice, Look};
use crate::settings::write_json;

/// What the recording is, when its maker says so instead of leaving it to
/// be seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SourceType {
    /// Look at each clip and decide.
    #[default]
    Auto,
    /// Somebody talking to a camera: cropped in on.
    TalkingHead,
    /// A screen recording: shown whole.
    Screen,
    /// A meeting: cropped in on a speaker who fills the picture, shown
    /// whole when it is a grid of faces.
    Meeting,
    /// Different things in different places: each clip decided on its own.
    Mixed,
}

/// Where a job has got to in being prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    /// Waiting for another job's transcription to finish.
    Queued,
    /// Downloading the recording.
    Fetching,
    /// Turning its speech into words.
    Transcribing,
    /// Finding its pauses.
    Listening,
    /// Picking moments.
    Suggesting,
    /// Prepared: the plan can be read, changed and rendered.
    Ready,
    /// Preparing stopped on an error; `error` says which.
    Failed,
    /// Preparing was cancelled.
    Cancelled,
}

impl Stage {
    /// Whether preparing is still under way.
    pub fn working(self) -> bool {
        !matches!(self, Stage::Ready | Stage::Failed | Stage::Cancelled)
    }
}

/// What the recording is, measured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    /// Its length in seconds.
    pub duration: f64,
    /// Its picture's width in pixels, as shown (after any rotation).
    pub width: u32,
    /// Its picture's height.
    pub height: u32,
    /// Its frames a second.
    pub fps: f64,
}

/// Whether a clip is to be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    /// Suggested, waiting on a yes or a no.
    Proposed,
    /// To be rendered.
    Approved,
    /// Not wanted; kept so it is not suggested again.
    Rejected,
}

/// Who picked a clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Origin {
    /// The tool's own suggestion.
    Suggested,
    /// A person or an agent, by its times.
    Chosen,
}

/// One planned clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    /// Its name within the job: `c1`, `c2`...
    pub id: String,
    /// A few words to call it by; files are named with it.
    pub title: String,
    /// Where it starts, in seconds of the recording.
    pub start: f64,
    /// Where it ends.
    pub end: f64,
    /// How long it runs once its pauses are tightened.
    pub length: f64,
    /// Whether it is to be made.
    pub verdict: Verdict,
    /// Who picked it.
    pub origin: Origin,
    /// Its opening line.
    pub hook: String,
    /// Why it was suggested, when it was.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// How it scored, when it was suggested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// How its picture is framed, when that is chosen by hand.
    #[serde(default)]
    pub framing: Choice,
    /// Where across the picture a crop centres, 0 (left) to 1 (right),
    /// when that is chosen by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_x: Option<f64>,
    /// What was seen in its picture, once it has been looked at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<Look>,
}

/// How far one clip in one frame shape has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RenderState {
    /// Waiting its turn.
    Queued,
    /// Its project is built and the engine is exporting it.
    Exporting,
    /// Its file is written.
    Done,
    /// It stopped on an error; `error` says which.
    Failed,
    /// It was cancelled.
    Cancelled,
}

/// One clip in one frame shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Render {
    /// The clip's id.
    pub clip: String,
    /// The format's id.
    pub format: String,
    /// How far it has got.
    pub state: RenderState,
    /// The Concat project it was built as: a folder that opens in the
    /// window for finishing by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The file it is written to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// The engine's name for its export, while one is running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<String>,
    /// How its picture was framed, and anything left out and why.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// What stopped it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How long the finished file runs, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
}

/// One recording and everything planned from it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// Its name: its folder's name in the workspace.
    pub id: String,
    /// What it was pointed at: a path or a link, as given.
    pub source: String,
    /// When it was started, in seconds since 1970.
    pub created: u64,
    /// Where preparing has got to.
    pub stage: Stage,
    /// What is happening now, for a person to read.
    #[serde(default)]
    pub note: String,
    /// How far through the current stage, 0 to 1, when that is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    /// What stopped preparing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The recording as a file on this machine, once there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
    /// What the recording is, measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording: Option<Recording>,
    /// What the recording is, said.
    #[serde(default)]
    pub source_type: SourceType,
    /// How many words were heard in it.
    #[serde(default)]
    pub words: usize,
    /// The plan.
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// Every clip and format asked to render so far.
    #[serde(default)]
    pub renders: Vec<Render>,
    /// The number the next clip's id takes.
    #[serde(default = "one")]
    pub next_clip: u32,
}

fn one() -> u32 {
    1
}

/// Seconds since 1970, now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// Four hex digits from `text`: enough to tell two jobs on one recording
/// apart, short enough to say aloud.
pub fn tag(text: &str) -> String {
    // FNV-1a.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{:04x}", (hash ^ (hash >> 16)) & 0xffff)
}

impl Job {
    /// A job just begun on `source`.
    pub fn begin(id: String, source: String, source_type: SourceType) -> Job {
        Job {
            id,
            source,
            created: now(),
            stage: Stage::Queued,
            note: "waiting to start".to_owned(),
            progress: None,
            error: None,
            media: None,
            recording: None,
            source_type,
            words: 0,
            clips: Vec::new(),
            renders: Vec::new(),
            next_clip: 1,
        }
    }

    /// The clip called `id`.
    pub fn clip(&self, id: &str) -> Option<&Clip> {
        self.clips.iter().find(|clip| clip.id == id)
    }

    /// The clip called `id`, to change.
    pub fn clip_mut(&mut self, id: &str) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|clip| clip.id == id)
    }

    /// A fresh clip id.
    pub fn mint_clip(&mut self) -> String {
        let id = format!("c{}", self.next_clip);
        self.next_clip += 1;
        id
    }

    /// The render of `clip` in `format`, if one was ever asked for.
    pub fn render(&self, clip: &str, format: &str) -> Option<&Render> {
        self.renders
            .iter()
            .find(|render| render.clip == clip && render.format == format)
    }

    /// The render of `clip` in `format`, to change; made, queued, when
    /// there is none yet.
    pub fn render_mut(&mut self, clip: &str, format: &str) -> &mut Render {
        let at = match self
            .renders
            .iter()
            .position(|render| render.clip == clip && render.format == format)
        {
            Some(at) => at,
            None => {
                self.renders.push(Render {
                    clip: clip.to_owned(),
                    format: format.to_owned(),
                    state: RenderState::Queued,
                    project: None,
                    output: None,
                    export: None,
                    note: String::new(),
                    error: None,
                    seconds: None,
                });
                self.renders.len() - 1
            }
        };
        &mut self.renders[at]
    }
}

/// A job's folder and the files in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder(pub PathBuf);

impl Folder {
    /// The folder of job `id` in `workspace`.
    pub fn of(workspace: &Path, id: &str) -> Folder {
        Folder(workspace.join(id))
    }

    /// `job.json`: the job itself.
    pub fn job(&self) -> PathBuf {
        self.0.join("job.json")
    }

    /// `transcript.json`: what was said.
    pub fn transcript(&self) -> PathBuf {
        self.0.join("transcript.json")
    }

    /// `quiet.json`: where it goes quiet.
    pub fn quiet(&self) -> PathBuf {
        self.0.join("quiet.json")
    }

    /// `projects/`: the Concat project behind each rendered clip.
    pub fn projects(&self) -> PathBuf {
        self.0.join("projects")
    }

    /// `clips/`: the finished files.
    pub fn clips(&self) -> PathBuf {
        self.0.join("clips")
    }

    /// Reads the job; `NotFound` when the folder holds none.
    pub fn load(&self) -> Result<Job, Error> {
        let path = self.job();
        let text = std::fs::read_to_string(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound(format!("there is no job at {}", self.0.display()))
            } else {
                Error::Failed(format!("could not read {}: {error}", path.display()))
            }
        })?;
        serde_json::from_str(&text)
            .map_err(|error| Error::Failed(format!("{} does not read: {error}", path.display())))
    }

    /// Writes the job, whole or not at all.
    pub fn save(&self, job: &Job) -> Result<(), Error> {
        write_json(&self.job(), job)
    }
}

/// A job id is a folder name of this tool's own making, and nothing else:
/// lowercase letters, digits and hyphens. Anything else is refused before
/// it is joined to a path, so no id can name a folder outside the
/// workspace.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_is_read_back_from_its_folder() {
        let workspace = tempfile::tempdir().expect("a scratch folder");
        let folder = Folder::of(workspace.path(), "talk-1a2b");
        assert!(matches!(folder.load(), Err(Error::NotFound(_))));
        let mut job = Job::begin(
            "talk-1a2b".to_owned(),
            "/videos/talk.mp4".to_owned(),
            SourceType::TalkingHead,
        );
        let id = job.mint_clip();
        assert_eq!(id, "c1");
        job.clips.push(Clip {
            id,
            title: "Start with why".to_owned(),
            start: 12.0,
            end: 80.0,
            length: 64.5,
            verdict: Verdict::Proposed,
            origin: Origin::Suggested,
            hook: "Start with why.".to_owned(),
            reason: "Opens on a strong line.".to_owned(),
            score: Some(0.71),
            framing: Choice::Auto,
            focus_x: None,
            look: None,
        });
        job.render_mut("c1", "vertical").state = RenderState::Done;
        folder.save(&job).expect("saves");
        let back = folder.load().expect("loads");
        assert_eq!(back, job);
        assert_eq!(
            back.render("c1", "vertical").map(|render| render.state),
            Some(RenderState::Done)
        );
        assert_eq!(back.render("c1", "square"), None);
        assert_eq!(job.mint_clip(), "c2");
    }

    #[test]
    fn a_job_id_cannot_leave_the_workspace() {
        assert!(valid_id("team-webinar-3f2a"));
        for id in ["", "../etc", "a/b", "a\\b", "A", "-x", ".", "a b"] {
            assert!(!valid_id(id), "{id:?}");
        }
    }

    #[test]
    fn a_tag_is_four_hex_digits_and_follows_its_text() {
        let tag_a = tag("/videos/talk.mp4 1");
        assert_eq!(tag_a.len(), 4);
        assert!(tag_a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(tag_a, tag("/videos/talk.mp4 1"));
        assert_ne!(tag_a, tag("/videos/talk.mp4 2"));
    }
}
