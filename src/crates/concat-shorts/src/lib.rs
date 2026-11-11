// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! Long recordings into short clips.
//!
//! A talk, a screen recording, a meeting or a folder's worth of a webinar
//! goes in - a file, or a link to one - and what comes out is a handful of
//! clips of a minute or so each, in every shape of frame a feed wants,
//! with the pauses tightened, the speaker framed, captions that follow the
//! words, the sound cleaned and levelled, and the brand on them.
//!
//! The work is a job ([`plan::Job`]) in two halves:
//!
//! 1. **Preparing** ([`studio::Studio::start`]), on a thread of its own and
//!    with nothing of the editor in it: the recording is fetched if it is a
//!    link ([`source`]), probed, transcribed word by word ([`transcript`]),
//!    its pauses found ([`sound`]) and its best moments suggested
//!    ([`moments`]). What it leaves is a plan: clips with a start, an end,
//!    a title and a reason, each proposed until someone approves it. The
//!    plan is the review step - the moments can be the tool's, an agent's
//!    reading of the transcript, or timestamps a person already has.
//! 2. **Rendering** ([`studio::Studio::render`]): each approved clip, in
//!    each frame shape, becomes a Concat project built from the engine's
//!    own edit commands ([`compose`]) - the recording placed and cut to its
//!    kept stretches, framed by what is in the picture ([`framing`]),
//!    captioned ([`captions`]), branded and cleaned ([`settings`]) - and is
//!    exported by the engine's exporter. The projects stay in the job's
//!    folder, so any clip can be opened in the Concat window and finished
//!    by hand.
//!
//! Nothing here renders a pixel or decides what an edit means: every
//! change to a project is a [`concat_api::Request`] handed to an
//! [`Engine`], the same requests the window, the JSON-RPC API and the MCP
//! tools make. That is what lets a job run through a headless engine or
//! through the open window's server alike.

pub mod captions;
pub mod compose;
pub mod framing;
pub mod moments;
pub mod plan;
pub mod settings;
pub mod sound;
pub mod source;
pub mod studio;
pub mod transcript;

use concat_api::{ApiError, Request};
use serde_json::Value;

pub use settings::Settings;
pub use studio::Studio;

/// What requests to the editor go through: the API in this process, or a
/// connection to the window's.
pub trait Engine {
    /// Runs one request; its result as the JSON the API replies with.
    fn call(&mut self, request: Request) -> Result<Value, ApiError>;
}

/// Why something was not done.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// What was asked for makes no sense as asked: a time past the end of
    /// the recording, a setting out of range. Asking differently works.
    Invalid(String),
    /// What was named is not there: a job, a clip, a format.
    NotFound(String),
    /// It was asked for properly and could not be done.
    Failed(String),
}

impl Error {
    /// The sentence, whichever kind it is.
    pub fn message(&self) -> &str {
        match self {
            Error::Invalid(message) | Error::NotFound(message) | Error::Failed(message) => message,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(self.message())
    }
}

impl std::error::Error for Error {}

impl From<ApiError> for Error {
    fn from(error: ApiError) -> Self {
        Error::Failed(error.message)
    }
}

/// Seconds as a person reads them: `1:05`, `12:40`, `1:02:03`.
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// A time as a person writes one: seconds as a number, or `m:ss`,
/// `h:mm:ss` and their decimals. `None` for anything else, and for a time
/// before zero.
pub fn parse_time(value: &Value) -> Option<f64> {
    if let Some(seconds) = value.as_f64() {
        return (seconds.is_finite() && seconds >= 0.0).then_some(seconds);
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut seconds = 0.0;
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let plain = !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_digit() || (last && c == '.'));
        if !plain {
            return None;
        }
        let number: f64 = part.parse().ok()?;
        // Minutes and seconds under an hour or a minute run to 59.
        if index > 0 && number >= 60.0 {
            return None;
        }
        seconds = seconds * 60.0 + number;
    }
    seconds.is_finite().then_some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn times_are_read_as_people_write_them() {
        assert_eq!(parse_time(&json!(75.5)), Some(75.5));
        assert_eq!(parse_time(&json!("75.5")), Some(75.5));
        assert_eq!(parse_time(&json!("1:15")), Some(75.0));
        assert_eq!(parse_time(&json!("01:02:03.5")), Some(3723.5));
        assert_eq!(parse_time(&json!(" 12:40 ")), Some(760.0));
        assert_eq!(parse_time(&json!("1:75")), None);
        assert_eq!(parse_time(&json!("soon")), None);
        assert_eq!(parse_time(&json!(-1)), None);
        assert_eq!(parse_time(&json!("")), None);
        assert_eq!(parse_time(&json!("1:2:3:4")), None);
    }

    #[test]
    fn times_are_written_as_people_read_them() {
        assert_eq!(clock(5.0), "0:05");
        assert_eq!(clock(65.4), "1:05");
        assert_eq!(clock(3723.0), "1:02:03");
    }
}
