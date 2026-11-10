// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! The shorts tools: a long recording into short clips, from a chat.
//!
//! These are the one family of tools here that is not a single API
//! request. Each is a method of [`concat_shorts::Studio`], which prepares a
//! recording on a thread of its own and renders its clips through the same
//! backend every other tool uses - so a clip is still built from the
//! engine's edit commands and exported by its exporter, and a job runs the
//! same headless or through the window's server.
//!
//! The shape an agent meets is a job with a plan: start one, wait for it,
//! read the plan (and the transcript, to pick moments of its own), change
//! and approve it with the person, render. Every reply is the job as it
//! then stands, with a `next` line saying what would usually follow, so an
//! agent that has forgotten the order is told it by the tool it just
//! called.

use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use concat_api::{ApiError, AppDirs, ErrorCode, Request};
use concat_shorts::framing::Choice;
use concat_shorts::plan::{Job, RenderState, SourceType, Stage, Verdict};
use concat_shorts::settings::ClipRules;
use concat_shorts::studio::{ClipSpec, RenderOptions, StartOptions};
use concat_shorts::{Engine, Error, Studio, clock, parse_time};
use serde_json::{Map, Value, json};

use crate::backend::Backend;
use crate::tools::{Action, Outcome, Tool};

/// How long a wait or a render watches when told nothing, and at most.
const WAIT_DEFAULT: f64 = 60.0;
const WAIT_LONGEST: f64 = 600.0;
/// How much transcript one call returns when told nothing, and at most.
const PAGE_DEFAULT: usize = 24_000;
const PAGE_LONGEST: usize = 80_000;

/// Which shorts tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Begin preparing a recording.
    Start,
    /// Watch a job being prepared.
    Wait,
    /// A job as it stands, or every job.
    Status,
    /// What was said.
    Transcript,
    /// Add or change clips by their times.
    Clips,
    /// Suggest moments afresh.
    Suggest,
    /// Approve, reject, remove.
    Review,
    /// One frame of a clip as it will render.
    Preview,
    /// Render the approved clips.
    Render,
    /// Stop what a job is doing.
    Cancel,
    /// Read the settings.
    Settings,
    /// Change the settings.
    SettingsUpdate,
}

const fn tool(
    name: &'static str,
    title: &'static str,
    description: &'static str,
    kind: Kind,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
) -> Tool {
    Tool {
        name,
        title,
        description,
        read_only,
        destructive,
        idempotent,
        action: Action::Shorts(kind),
    }
}

/// The shorts tools, in the order they are usually called.
pub fn catalogue() -> Vec<Tool> {
    vec![
        tool(
            "shorts_start",
            "Start a shorts job",
            "Begins turning a long recording into short clips. `source` is a video file's full path on this machine, or a link (a direct video link, a Google Drive or Dropbox share link, or a page yt-dlp can fetch such as YouTube, Zoom or Vimeo). Returns at once with a `job` id while the recording is fetched, transcribed on this machine and its best moments suggested; follow with `shorts_wait`. `sourceType` says what the recording is when the person has said (\"talkingHead\" is cropped in on the speaker, \"screen\" is shown whole over a blurred copy, \"meeting\" and \"mixed\" are decided clip by clip, which is also the default \"auto\"). `minSeconds`/`maxSeconds`/`maxClips` change the length and number of suggested clips for this job (60-90 seconds and 6 unless the settings say otherwise). To carry on a job that was interrupted, pass its `job` id instead of a source.",
            Kind::Start,
            false,
            false,
            false,
        ),
        tool(
            "shorts_wait",
            "Wait for a shorts job",
            "Waits for a job to finish being prepared, up to `timeoutSeconds` (60 by default, 600 at most), reporting progress, and returns the job. An hour of recording takes several minutes to transcribe, so a job is often still working when the time is up: call again. Once `stage` is \"ready\" the reply holds the plan - the suggested clips, each with an id, a title, its start and end, why it was suggested and whether it is approved.",
            Kind::Wait,
            true,
            false,
            true,
        ),
        tool(
            "shorts_status",
            "Shorts job status",
            "With `job`: that job as it stands now - its stage, its plan, and every render with its state and the file it wrote. Without: every job in the workspace, newest first, in brief. Read-only and immediate.",
            Kind::Status,
            true,
            false,
            true,
        ),
        tool(
            "shorts_transcript",
            "Read a job's transcript",
            "What was said in a job's recording, a sentence a line, each led by its time as [m:ss]. Read it to pick moments yourself, to check where a suggested clip starts and ends, or to answer a question about the recording. `from` and `to` (seconds, or \"m:ss\") narrow it; a long transcript comes a page at a time, and `nextFrom` is where the next page starts.",
            Kind::Transcript,
            true,
            false,
            true,
        ),
        tool(
            "shorts_clips",
            "Add or change clips",
            "Puts clips in a job's plan by their times - the person's own timestamps, or moments you picked from the transcript. Each entry of `clips` has `start` and `end` (seconds, or \"m:ss\" / \"h:mm:ss\"), and optionally a `title`, `framing` (\"auto\", \"crop\" to fill the frame, \"fit\" to show the whole picture over a blurred copy) and `focusX` (where across the picture a crop centres, 0 left to 1 right). With an existing clip's `id`, that clip is changed instead and only the fields given change. Times are moved onto the nearest word boundaries so a clip never starts or ends mid-word, unless `exact` is true. A new clip is approved unless `approved` is false. `replace: true` makes the plan exactly these clips.",
            Kind::Clips,
            false,
            false,
            false,
        ),
        tool(
            "shorts_suggest",
            "Suggest moments again",
            "Suggests moments afresh with a different number or length: `count`, `minSeconds`, `maxSeconds`. Clips already approved, rejected or given by their times are kept, and nothing overlapping them is suggested; the other suggestions are replaced.",
            Kind::Suggest,
            false,
            true,
            false,
        ),
        tool(
            "shorts_review",
            "Approve or reject clips",
            "Records the person's decision on a job's clips: `approve` (a list of clip ids, or \"all\" for every clip still proposed), `reject` (kept in the plan so they are not suggested again) and `remove` (taken out of the plan). Only approved clips render. Show the plan to the person and ask before approving on their behalf.",
            Kind::Review,
            false,
            true,
            true,
        ),
        tool(
            "shorts_preview",
            "Preview a clip",
            "One frame of a clip as it will render in a format, as an image to look at: framing, captions, logo and all. `clip` is a clip id, `format` a format id (\"vertical\", \"landscape\" or \"square\" unless the settings say otherwise), `time` seconds into the clip (a third of the way through when absent; a time past the footage shows the end card). Use it to check a crop or the look of the captions before rendering everything. Works on a clip that is not approved yet.",
            Kind::Preview,
            true,
            false,
            true,
        ),
        tool(
            "shorts_render",
            "Render the approved clips",
            "Renders a job's approved clips in every enabled format - each cut to its moment, its pauses tightened, framed, captioned, branded, its sound cleaned and levelled - and writes an .mp4 per clip per format into the job's `clips` folder. Watches for up to `timeoutSeconds` (60 by default, 600 at most) with progress and returns the job; rendering takes a while, so when `rendersLeft` is not 0, call it again and it carries on where it left off without redoing what is done. `clips` and `formats` narrow what is rendered; `again: true` renders again what already has a file (after a settings change, say). Each render also leaves a Concat project the person can open in the Concat window to finish by hand.",
            Kind::Render,
            false,
            false,
            true,
        ),
        tool(
            "shorts_cancel",
            "Cancel a shorts job",
            "Stops what a job is doing: its preparation, or the export of its clips. What was already rendered is kept.",
            Kind::Cancel,
            false,
            true,
            true,
        ),
        tool(
            "shorts_settings",
            "Read the shorts settings",
            "The settings every short is made with, and the file they are kept in: the brand kit (colours, fonts, logo, end card), caption style, clip length, pause tightening, sound cleanup, frame formats, reframing and export quality.",
            Kind::Settings,
            true,
            false,
            true,
        ),
        tool(
            "shorts_settings_update",
            "Change the shorts settings",
            "Changes settings: `patch` is an object holding only the fields to change, nested as `shorts_settings` shows them, e.g. {\"brand\": {\"logo\": {\"file\": \"/Users/me/brand/logo.png\"}}} or {\"clips\": {\"maxSeconds\": 75}}. A field set to null goes back to its default. Lists (`formats`, `brand.fonts`) are replaced whole. A colour is a name from `brand.colors` or a #rrggbb. A wrong field or value is refused with its name and nothing changes. Applies to renders from now on; use `shorts_render` with `again: true` to redo clips already rendered.",
            Kind::SettingsUpdate,
            false,
            false,
            true,
        ),
    ]
}

/// A time as a schema: seconds, or a clock string.
fn time(description: &str) -> Value {
    json!({
        "type": ["number", "string"],
        "description": format!("{description} Seconds as a number, or \"m:ss\" / \"h:mm:ss\"."),
    })
}

fn job_field() -> Value {
    json!({ "type": "string", "description": "The job's id, as shorts_start returned it." })
}

fn timeout_field() -> Value {
    json!({
        "type": "number",
        "minimum": 0,
        "maximum": WAIT_LONGEST,
        "default": WAIT_DEFAULT,
        "description": "How long to watch, in seconds.",
    })
}

fn ids(description: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": description })
}

/// The schema a shorts tool's arguments follow.
pub fn schema(kind: Kind) -> Value {
    let object = |properties: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    };
    match kind {
        Kind::Start => object(
            json!({
                "source": { "type": "string", "description": "A video file's full path on this machine, or an http(s) link to a recording." },
                "sourceType": {
                    "type": "string",
                    "enum": ["auto", "talkingHead", "screen", "meeting", "mixed"],
                    "default": "auto",
                    "description": "What the recording is, when the person has said.",
                },
                "job": { "type": "string", "description": "An interrupted job to carry on with, instead of a new source." },
                "minSeconds": { "type": "number", "minimum": 3, "maximum": 600, "description": "The shortest suggested clip." },
                "maxSeconds": { "type": "number", "minimum": 3, "maximum": 600, "description": "The longest suggested clip." },
                "maxClips": { "type": "integer", "minimum": 1, "maximum": 50, "description": "The most clips suggested." },
            }),
            &[],
        ),
        Kind::Wait => object(
            json!({ "job": job_field(), "timeoutSeconds": timeout_field() }),
            &["job"],
        ),
        Kind::Status => object(
            json!({ "job": { "type": "string", "description": "A job's id; every job when absent." } }),
            &[],
        ),
        Kind::Transcript => object(
            json!({
                "job": job_field(),
                "from": time("Where to start reading."),
                "to": time("Where to stop."),
                "maxChars": {
                    "type": "integer",
                    "minimum": 200,
                    "maximum": PAGE_LONGEST,
                    "default": PAGE_DEFAULT,
                    "description": "The most text to return in one call.",
                },
            }),
            &["job"],
        ),
        Kind::Clips => object(
            json!({
                "job": job_field(),
                "clips": {
                    "type": "array",
                    "description": "The clips to add or change.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "An existing clip to change; a new clip is made when absent." },
                            "start": time("Where the clip starts in the recording."),
                            "end": time("Where it ends."),
                            "title": { "type": "string", "description": "A few words to call it by; taken from its opening words when absent." },
                            "framing": { "type": "string", "enum": ["auto", "crop", "fit"], "description": "How its picture is put in a frame of another shape." },
                            "focusX": { "type": ["number", "null"], "minimum": 0, "maximum": 1, "description": "Where across the picture a crop centres, 0 (left) to 1 (right); null to find the speaker again." },
                            "approved": { "type": "boolean", "description": "Whether it is approved to render." },
                            "exact": { "type": "boolean", "default": false, "description": "Keep the times exactly as given instead of moving them onto word boundaries." },
                        },
                        "additionalProperties": false,
                    },
                },
                "replace": { "type": "boolean", "default": false, "description": "Make the plan exactly these clips." },
            }),
            &["job", "clips"],
        ),
        Kind::Suggest => object(
            json!({
                "job": job_field(),
                "count": { "type": "integer", "minimum": 1, "maximum": 50, "description": "How many moments to suggest." },
                "minSeconds": { "type": "number", "minimum": 3, "maximum": 600, "description": "The shortest suggested clip." },
                "maxSeconds": { "type": "number", "minimum": 3, "maximum": 600, "description": "The longest suggested clip." },
            }),
            &["job"],
        ),
        Kind::Review => object(
            json!({
                "job": job_field(),
                "approve": {
                    "description": "Clip ids to approve, or \"all\" for every clip still proposed.",
                    "anyOf": [
                        { "type": "array", "items": { "type": "string" } },
                        { "type": "string", "enum": ["all"] },
                    ],
                },
                "reject": ids("Clip ids to reject."),
                "remove": ids("Clip ids to take out of the plan."),
            }),
            &["job"],
        ),
        Kind::Preview => object(
            json!({
                "job": job_field(),
                "clip": { "type": "string", "description": "The clip's id." },
                "format": { "type": "string", "description": "The format's id, e.g. \"vertical\"." },
                "time": { "type": "number", "minimum": 0, "description": "Seconds into the clip." },
            }),
            &["job", "clip", "format"],
        ),
        Kind::Render => object(
            json!({
                "job": job_field(),
                "clips": ids("Clip ids to render; every approved clip when absent."),
                "formats": ids("Format ids to render in; the enabled formats when absent."),
                "again": { "type": "boolean", "default": false, "description": "Render again what already has a file." },
                "timeoutSeconds": timeout_field(),
            }),
            &["job"],
        ),
        Kind::Cancel => object(json!({ "job": job_field() }), &["job"]),
        Kind::Settings => object(json!({}), &[]),
        Kind::SettingsUpdate => object(
            json!({
                "patch": { "type": "object", "description": "The fields to change, nested as shorts_settings shows them." },
            }),
            &["patch"],
        ),
    }
}

/// A backend as the engine a studio renders through.
struct Through<'a>(&'a mut dyn Backend);

impl Engine for Through<'_> {
    fn call(&mut self, request: Request) -> Result<Value, ApiError> {
        self.0.call(request)
    }
}

fn refusal(error: Error) -> ApiError {
    match error {
        Error::Invalid(message) => ApiError::invalid(message),
        Error::NotFound(message) => ApiError::new(ErrorCode::NotFound, message),
        Error::Failed(message) => ApiError::failed(message),
    }
}

/// A tool's arguments, taken field by field so that what is left over is
/// what nobody asked for.
struct Arguments(Map<String, Value>);

impl Arguments {
    fn of(arguments: Value) -> Result<Arguments, ApiError> {
        match arguments {
            Value::Object(fields) => Ok(Arguments(fields)),
            Value::Null => Ok(Arguments(Map::new())),
            _ => Err(ApiError::invalid("the arguments are an object")),
        }
    }

    fn take(&mut self, key: &str) -> Option<Value> {
        self.0.remove(key).filter(|value| !value.is_null())
    }

    fn text(&mut self, key: &str) -> Result<Option<String>, ApiError> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(ApiError::invalid(format!("invalid arguments: `{key}` is a string"))),
        }
    }

    fn required_text(&mut self, key: &str) -> Result<String, ApiError> {
        self.text(key)?
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| ApiError::invalid(format!("invalid arguments: missing field `{key}`")))
    }

    fn number(&mut self, key: &str) -> Result<Option<f64>, ApiError> {
        match self.take(key) {
            None => Ok(None),
            Some(value) => value
                .as_f64()
                .filter(|number| number.is_finite())
                .map(Some)
                .ok_or_else(|| ApiError::invalid(format!("invalid arguments: `{key}` is a number"))),
        }
    }

    fn count(&mut self, key: &str) -> Result<Option<usize>, ApiError> {
        match self.take(key) {
            None => Ok(None),
            Some(value) => value
                .as_u64()
                .map(|count| Some(count as usize))
                .ok_or_else(|| {
                    ApiError::invalid(format!("invalid arguments: `{key}` is a whole number"))
                }),
        }
    }

    fn flag(&mut self, key: &str) -> Result<Option<bool>, ApiError> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::Bool(flag)) => Ok(Some(flag)),
            Some(_) => Err(ApiError::invalid(format!(
                "invalid arguments: `{key}` is true or false"
            ))),
        }
    }

    fn time(&mut self, key: &str) -> Result<Option<f64>, ApiError> {
        match self.take(key) {
            None => Ok(None),
            Some(value) => parse_time(&value).map(Some).ok_or_else(|| {
                ApiError::invalid(format!(
                    "invalid arguments: `{key}` is a time - seconds as a number, or \"m:ss\" - not {value}"
                ))
            }),
        }
    }

    fn list(&mut self, key: &str) -> Result<Option<Vec<String>>, ApiError> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::Array(items)) => items
                .into_iter()
                .map(|item| match item {
                    Value::String(text) => Ok(text),
                    _ => Err(ApiError::invalid(format!(
                        "invalid arguments: `{key}` is a list of ids"
                    ))),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(ApiError::invalid(format!(
                "invalid arguments: `{key}` is a list of ids"
            ))),
        }
    }

    fn timeout(&mut self) -> Result<f64, ApiError> {
        Ok(self
            .number("timeoutSeconds")?
            .unwrap_or(WAIT_DEFAULT)
            .clamp(0.0, WAIT_LONGEST))
    }

    /// Nothing may be left: a field nobody took is a field misspelt.
    fn done(self) -> Result<(), ApiError> {
        match self.0.keys().next() {
            Some(unknown) => Err(ApiError::invalid(format!(
                "invalid arguments: unknown field `{unknown}`"
            ))),
            None => Ok(()),
        }
    }
}

fn source_type(word: &str) -> Result<SourceType, ApiError> {
    serde_json::from_value(json!(word)).map_err(|_| {
        ApiError::invalid(
            "invalid arguments: `sourceType` is auto, talkingHead, screen, meeting or mixed",
        )
    })
}

fn framing(word: &str) -> Result<Choice, ApiError> {
    serde_json::from_value(json!(word))
        .map_err(|_| ApiError::invalid("invalid arguments: `framing` is auto, crop or fit"))
}

/// One clip of `shorts_clips`, read.
fn clip_spec(value: Value, number: usize) -> Result<ClipSpec, ApiError> {
    let Value::Object(fields) = value else {
        return Err(ApiError::invalid(format!(
            "invalid arguments: clip {number} is an object with a start and an end"
        )));
    };
    // `focusX: null` means something - find the speaker again - so it is
    // read before the nulls are dropped.
    let focus_x = match fields.get("focusX") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(value) => Some(Some(value.as_f64().ok_or_else(|| {
            ApiError::invalid(format!("invalid arguments: clip {number}: `focusX` is a number or null"))
        })?)),
    };
    let mut fields = Arguments(fields);
    fields.0.remove("focusX");
    let spec = ClipSpec {
        id: fields.text("id")?,
        start: fields.time("start")?,
        end: fields.time("end")?,
        title: fields.text("title")?,
        framing: fields.text("framing")?.as_deref().map(framing).transpose()?,
        focus_x,
        approved: fields.flag("approved")?,
        exact: fields.flag("exact")?.unwrap_or(false),
    };
    fields.done()?;
    Ok(spec)
}

/// A number of seconds to one decimal place: what a person needs of one.
fn tenth(seconds: f64) -> f64 {
    (seconds * 10.0).round() / 10.0
}

/// What would usually follow, for an agent to read.
fn next(job: &Job) -> String {
    match job.stage {
        Stage::Failed => {
            return "Preparing failed; `error` says why. Fix that and call shorts_start with this job's id to try again.".to_owned();
        }
        Stage::Cancelled => {
            return "Preparing was cancelled. Call shorts_start with this job's id to carry on.".to_owned();
        }
        stage if stage.working() => {
            return "Still being prepared: call shorts_wait with this job's id.".to_owned();
        }
        _ => {}
    }
    let approved: Vec<&str> = job
        .clips
        .iter()
        .filter(|clip| clip.verdict == Verdict::Approved)
        .map(|clip| clip.id.as_str())
        .collect();
    let proposed = job
        .clips
        .iter()
        .filter(|clip| clip.verdict == Verdict::Proposed)
        .count();
    let exporting = job
        .renders
        .iter()
        .any(|render| render.state == RenderState::Exporting);
    let rendered = |clip: &str| {
        job.renders
            .iter()
            .any(|render| render.clip == clip && render.state == RenderState::Done)
    };
    if exporting {
        return "A render is under way: call shorts_render again to carry on watching it.".to_owned();
    }
    if job.clips.is_empty() {
        return "No moments were suggested. Read shorts_transcript and add clips by their times with shorts_clips, or try shorts_suggest with a shorter minSeconds.".to_owned();
    }
    if approved.is_empty() {
        return format!(
            "Show the {proposed} proposed clip(s) to the person - title, times, length and why - and ask which to make. Record their answer with shorts_review; move or add clips with shorts_clips (shorts_transcript shows what was said); shorts_preview shows how one will look. Then shorts_render."
        );
    }
    if approved.iter().any(|clip| !rendered(clip)) {
        let waiting = if proposed > 0 {
            format!(" {proposed} clip(s) are still only proposed.")
        } else {
            String::new()
        };
        return format!(
            "{} clip(s) approved: call shorts_render to make them.{waiting}",
            approved.len()
        );
    }
    "The approved clips are rendered; each render's `output` is its file and `project` a Concat project that opens in the Concat window for changes by hand. Tell the person where the files are.".to_owned()
}

/// A job as an agent reads it.
fn view(job: &Job, folder: Option<&std::path::Path>) -> Value {
    let clips: Vec<Value> = job
        .clips
        .iter()
        .map(|clip| {
            let renders: Vec<Value> = job
                .renders
                .iter()
                .filter(|render| render.clip == clip.id)
                .map(|render| {
                    let mut entry = json!({ "format": render.format, "state": render.state });
                    if let Some(output) = &render.output {
                        entry["output"] = json!(output);
                    }
                    if let Some(project) = &render.project {
                        entry["project"] = json!(project);
                    }
                    if let Some(seconds) = render.seconds {
                        entry["seconds"] = json!(seconds);
                    }
                    if !render.note.is_empty() {
                        entry["note"] = json!(render.note);
                    }
                    if let Some(error) = &render.error {
                        entry["error"] = json!(error);
                    }
                    entry
                })
                .collect();
            let mut entry = json!({
                "id": clip.id,
                "title": clip.title,
                "verdict": clip.verdict,
                "start": tenth(clip.start),
                "end": tenth(clip.end),
                "from": clock(clip.start),
                "to": clock(clip.end),
                "seconds": tenth(clip.length),
                "origin": clip.origin,
                "hook": clip.hook,
            });
            if !clip.reason.is_empty() {
                entry["why"] = json!(clip.reason);
            }
            if let Some(score) = clip.score {
                entry["score"] = json!(score);
            }
            if clip.framing != Choice::Auto {
                entry["framing"] = json!(clip.framing);
            }
            if let Some(focus) = clip.focus_x {
                entry["focusX"] = json!(focus);
            }
            if let Some(look) = &clip.look {
                entry["seen"] = json!(look.subject);
            }
            if !renders.is_empty() {
                entry["renders"] = json!(renders);
            }
            entry
        })
        .collect();
    let left = job
        .renders
        .iter()
        .filter(|render| matches!(render.state, RenderState::Queued | RenderState::Exporting))
        .count();
    let mut out = json!({
        "job": job.id,
        "stage": job.stage,
        "note": job.note,
        "source": job.source,
        "sourceType": job.source_type,
        "words": job.words,
        "clips": clips,
        "rendersLeft": left,
        "next": next(job),
    });
    if let Some(progress) = job.progress {
        out["progress"] = json!((progress * 100.0).round() / 100.0);
    }
    if let Some(error) = &job.error {
        out["error"] = json!(error);
    }
    if let Some(recording) = &job.recording {
        out["recording"] = json!({
            "length": clock(recording.duration),
            "seconds": tenth(recording.duration),
            "width": recording.width,
            "height": recording.height,
            "fps": (recording.fps * 100.0).round() / 100.0,
        });
    }
    if let Some(folder) = folder {
        out["folder"] = json!(folder.to_string_lossy());
    }
    out
}

/// A job in a line of a list.
fn brief(job: &Job) -> Value {
    let done = job
        .renders
        .iter()
        .filter(|render| render.state == RenderState::Done)
        .count();
    json!({
        "job": job.id,
        "stage": job.stage,
        "source": job.source,
        "length": job.recording.as_ref().map(|recording| clock(recording.duration)),
        "clips": job.clips.len(),
        "approved": job.clips.iter().filter(|clip| clip.verdict == Verdict::Approved).count(),
        "rendered": done,
    })
}

/// The studio behind the shorts tools, made the first time one is called.
#[derive(Default)]
pub struct Shorts {
    studio: OnceLock<Result<Studio, String>>,
}

impl Shorts {
    /// The shorts tools over this machine's Concat folders, found when
    /// first needed.
    pub fn new() -> Shorts {
        Shorts::default()
    }

    fn studio(&self) -> Result<&Studio, ApiError> {
        self.studio
            .get_or_init(|| AppDirs::locate().map(Studio::new))
            .as_ref()
            .map_err(|error| ApiError::failed(error.clone()))
    }

    /// The job's folder, for a reply to name.
    fn folder(&self, studio: &Studio, job: &str) -> Option<std::path::PathBuf> {
        Some(studio.settings().ok()?.workspace_dir().ok()?.join(job))
    }

    fn reply(&self, studio: &Studio, job: &Job) -> Outcome {
        Outcome::json(view(job, self.folder(studio, &job.id).as_deref()))
    }

    /// Runs one shorts tool.
    pub fn run(
        &self,
        kind: Kind,
        arguments: Value,
        backend: &mut dyn Backend,
        cancelled: &AtomicBool,
        progress: Option<&dyn Fn(f64, f64, String)>,
    ) -> Result<Outcome, ApiError> {
        let studio = self.studio()?;
        let mut arguments = Arguments::of(arguments)?;
        match kind {
            Kind::Start => {
                let job = arguments.text("job")?;
                let source = arguments.text("source")?.unwrap_or_default();
                if job.is_none() && source.trim().is_empty() {
                    return Err(ApiError::invalid(
                        "invalid arguments: give `source` (a video file's path or a link), or `job` to carry on an interrupted one",
                    ));
                }
                let options = StartOptions {
                    source,
                    source_type: arguments
                        .text("sourceType")?
                        .as_deref()
                        .map(source_type)
                        .transpose()?
                        .unwrap_or_default(),
                    job,
                    min_seconds: arguments.number("minSeconds")?,
                    max_seconds: arguments.number("maxSeconds")?,
                    max_clips: arguments.count("maxClips")?,
                };
                arguments.done()?;
                let job = studio.start(options).map_err(refusal)?;
                Ok(self.reply(studio, &job))
            }
            Kind::Wait => {
                let id = arguments.required_text("job")?;
                let timeout = arguments.timeout()?;
                arguments.done()?;
                let job = studio
                    .wait(&id, Duration::from_secs_f64(timeout), cancelled, progress)
                    .map_err(refusal)?;
                Ok(self.reply(studio, &job))
            }
            Kind::Status => {
                let id = arguments.text("job")?;
                arguments.done()?;
                match id {
                    Some(id) => {
                        let job = studio.status(&id).map_err(refusal)?;
                        Ok(self.reply(studio, &job))
                    }
                    None => {
                        let jobs = studio.list().map_err(refusal)?;
                        let workspace = studio
                            .settings()
                            .and_then(|settings| settings.workspace_dir())
                            .map_err(refusal)?;
                        Ok(Outcome::json(json!({
                            "workspace": workspace.to_string_lossy(),
                            "jobs": jobs.iter().map(brief).collect::<Vec<_>>(),
                        })))
                    }
                }
            }
            Kind::Transcript => {
                let id = arguments.required_text("job")?;
                let from = arguments.time("from")?.unwrap_or(0.0);
                let to = arguments.time("to")?.unwrap_or(f64::MAX);
                let limit = arguments
                    .count("maxChars")?
                    .unwrap_or(PAGE_DEFAULT)
                    .clamp(200, PAGE_LONGEST);
                arguments.done()?;
                let (text, next_from, job) =
                    studio.transcript(&id, from, to, limit).map_err(refusal)?;
                let mut out = json!({
                    "job": job.id,
                    "words": job.words,
                    "length": job.recording.as_ref().map(|recording| clock(recording.duration)),
                    "transcript": if job.words == 0 {
                        "(no speech was heard in this recording)".to_owned()
                    } else {
                        text
                    },
                });
                if let Some(next_from) = next_from {
                    out["nextFrom"] = json!(tenth(next_from));
                    out["next"] = json!("There is more: call again with `from` set to nextFrom.");
                }
                Ok(Outcome::json(out))
            }
            Kind::Clips => {
                let id = arguments.required_text("job")?;
                let clips = match arguments.take("clips") {
                    Some(Value::Array(clips)) => clips,
                    _ => {
                        return Err(ApiError::invalid(
                            "invalid arguments: `clips` is a list of {start, end}",
                        ));
                    }
                };
                let replace = arguments.flag("replace")?.unwrap_or(false);
                arguments.done()?;
                let specs = clips
                    .into_iter()
                    .enumerate()
                    .map(|(index, clip)| clip_spec(clip, index + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                let job = studio.set_clips(&id, specs, replace).map_err(refusal)?;
                Ok(self.reply(studio, &job))
            }
            Kind::Suggest => {
                let id = arguments.required_text("job")?;
                let settings = studio.settings().map_err(refusal)?;
                let rules = ClipRules {
                    max_clips: arguments.count("count")?.unwrap_or(settings.clips.max_clips),
                    min_seconds: arguments
                        .number("minSeconds")?
                        .unwrap_or(settings.clips.min_seconds),
                    max_seconds: arguments
                        .number("maxSeconds")?
                        .unwrap_or(settings.clips.max_seconds),
                };
                arguments.done()?;
                let job = studio.suggest(&id, &rules).map_err(refusal)?;
                Ok(self.reply(studio, &job))
            }
            Kind::Review => {
                let id = arguments.required_text("job")?;
                let (approve, all) = match arguments.0.get("approve") {
                    Some(Value::String(word)) if word == "all" => {
                        arguments.0.remove("approve");
                        (Vec::new(), true)
                    }
                    _ => (arguments.list("approve")?.unwrap_or_default(), false),
                };
                let reject = arguments.list("reject")?.unwrap_or_default();
                let remove = arguments.list("remove")?.unwrap_or_default();
                arguments.done()?;
                if approve.is_empty() && !all && reject.is_empty() && remove.is_empty() {
                    return Err(ApiError::invalid(
                        "invalid arguments: give clip ids to `approve`, `reject` or `remove`",
                    ));
                }
                let job = studio
                    .review(&id, &approve, all, &reject, &remove)
                    .map_err(refusal)?;
                Ok(self.reply(studio, &job))
            }
            Kind::Preview => {
                let id = arguments.required_text("job")?;
                let clip = arguments.required_text("clip")?;
                let format = arguments.required_text("format")?;
                let at = arguments.number("time")?;
                arguments.done()?;
                let frame = studio
                    .preview(&mut Through(backend), &id, &clip, &format, at, cancelled)
                    .map_err(refusal)?;
                let text = format!(
                    "Clip {clip} in {format} at {} s, {}×{}. {}.",
                    tenth(frame.time),
                    frame.width,
                    frame.height,
                    frame.note
                );
                Ok(Outcome {
                    content: vec![
                        json!({ "type": "image", "data": frame.png, "mimeType": "image/png" }),
                        json!({ "type": "text", "text": text }),
                    ],
                    structured: Some(json!({
                        "job": id,
                        "clip": clip,
                        "format": format,
                        "time": tenth(frame.time),
                        "width": frame.width,
                        "height": frame.height,
                        "note": frame.note,
                    })),
                    is_error: false,
                })
            }
            Kind::Render => {
                let id = arguments.required_text("job")?;
                let options = RenderOptions {
                    clips: arguments.list("clips")?,
                    formats: arguments.list("formats")?,
                    again: arguments.flag("again")?.unwrap_or(false),
                    timeout: arguments.timeout()?,
                };
                arguments.done()?;
                let job = studio
                    .render(&mut Through(backend), &id, &options, cancelled, progress)
                    .map_err(refusal)?;
                Ok(self.reply(studio, &job))
            }
            Kind::Cancel => {
                let id = arguments.required_text("job")?;
                arguments.done()?;
                let job = studio.cancel(&mut Through(backend), &id).map_err(refusal)?;
                Ok(self.reply(studio, &job))
            }
            Kind::Settings => {
                arguments.done()?;
                let settings = studio.settings().map_err(refusal)?;
                Ok(Outcome::json(json!({
                    "file": studio.settings_path().to_string_lossy(),
                    "workspace": settings.workspace_dir().map_err(refusal)?.to_string_lossy(),
                    "settings": settings,
                })))
            }
            Kind::SettingsUpdate => {
                let patch = arguments.take("patch").ok_or_else(|| {
                    ApiError::invalid("invalid arguments: missing field `patch`")
                })?;
                arguments.done()?;
                let settings = studio.update_settings(&patch).map_err(refusal)?;
                Ok(Outcome::json(json!({
                    "file": studio.settings_path().to_string_lossy(),
                    "settings": settings,
                })))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_shorts::plan::{Clip, Origin};

    fn job_with(verdicts: &[Verdict]) -> Job {
        let mut job = Job::begin("talk-1a2b".to_owned(), "/v/talk.mp4".to_owned(), SourceType::Auto);
        job.stage = Stage::Ready;
        for verdict in verdicts {
            let id = job.mint_clip();
            job.clips.push(Clip {
                id,
                title: "A moment".to_owned(),
                start: 61.26,
                end: 130.0,
                length: 64.44,
                verdict: *verdict,
                origin: Origin::Suggested,
                hook: "A moment.".to_owned(),
                reason: "Opens on a question.".to_owned(),
                score: Some(0.7),
                framing: Choice::Auto,
                focus_x: None,
                look: None,
            });
        }
        job
    }

    #[test]
    fn a_reply_says_what_would_usually_follow() {
        let mut job = job_with(&[]);
        job.stage = Stage::Transcribing;
        assert!(next(&job).contains("shorts_wait"));
        job.stage = Stage::Failed;
        assert!(next(&job).contains("shorts_start"));
        assert!(next(&job_with(&[])).contains("shorts_transcript"));
        assert!(next(&job_with(&[Verdict::Proposed])).contains("shorts_review"));
        let mut approved = job_with(&[Verdict::Approved, Verdict::Proposed]);
        assert!(next(&approved).contains("shorts_render"));
        approved.render_mut("c1", "vertical").state = RenderState::Exporting;
        assert!(next(&approved).contains("again"));
        approved.render_mut("c1", "vertical").state = RenderState::Done;
        assert!(next(&approved).contains("rendered"));
    }

    #[test]
    fn a_job_reads_as_a_plan() {
        let mut job = job_with(&[Verdict::Approved]);
        let render = job.render_mut("c1", "vertical");
        render.state = RenderState::Done;
        render.output = Some("/w/talk-1a2b/clips/01-a-moment-c1-vertical.mp4".to_owned());
        job.render_mut("c1", "square");
        let seen = view(&job, Some(std::path::Path::new("/w/talk-1a2b")));
        assert_eq!(seen["job"], "talk-1a2b");
        assert_eq!(seen["stage"], "ready");
        assert_eq!(seen["folder"], "/w/talk-1a2b");
        assert_eq!(seen["rendersLeft"], 1);
        let clip = &seen["clips"][0];
        assert_eq!(clip["id"], "c1");
        assert_eq!(clip["verdict"], "approved");
        assert_eq!(clip["start"], 61.3);
        assert_eq!(clip["from"], "1:01");
        assert_eq!(clip["to"], "2:10");
        assert_eq!(clip["seconds"], 64.4);
        assert_eq!(clip["why"], "Opens on a question.");
        assert_eq!(clip["renders"][0]["state"], "done");
        assert_eq!(clip["renders"][1]["state"], "queued");
        // Nothing chosen by hand, so nothing said about it.
        assert!(clip.get("framing").is_none() && clip.get("focusX").is_none());
    }

    #[test]
    fn a_clip_is_read_with_its_times_as_a_person_writes_them() {
        let spec = clip_spec(
            json!({ "start": "1:05", "end": 130.5, "framing": "crop", "focusX": 0.3, "approved": false }),
            1,
        )
        .expect("a clip");
        assert_eq!(spec.start, Some(65.0));
        assert_eq!(spec.end, Some(130.5));
        assert_eq!(spec.framing, Some(Choice::Crop));
        assert_eq!(spec.focus_x, Some(Some(0.3)));
        assert_eq!(spec.approved, Some(false));
        assert!(!spec.exact);
        // null puts the focus back to be found.
        let cleared = clip_spec(json!({ "id": "c2", "focusX": null }), 1).expect("a clip");
        assert_eq!(cleared.focus_x, Some(None));
        assert_eq!(cleared.start, None);
        for wrong in [
            json!({ "start": "soon", "end": 10 }),
            json!({ "start": 1, "end": 10, "framing": "zoom" }),
            json!({ "start": 1, "end": 10, "colour": "red" }),
            json!("1:05-2:10"),
        ] {
            assert!(clip_spec(wrong.clone(), 1).is_err(), "{wrong}");
        }
    }

    #[test]
    fn arguments_nobody_asked_for_are_refused_by_name() {
        let mut arguments = Arguments::of(json!({ "job": "a", "jobb": "b" })).expect("an object");
        assert_eq!(arguments.required_text("job").expect("a job"), "a");
        let error = arguments.done().expect_err("an unknown field");
        assert!(error.message.contains("jobb"), "{}", error.message);
        let mut missing = Arguments::of(Value::Null).expect("no arguments");
        assert!(missing.required_text("job").is_err());
        let mut long = Arguments::of(json!({ "timeoutSeconds": 99999 })).expect("an object");
        assert_eq!(long.timeout().expect("a timeout"), WAIT_LONGEST);
    }
}
