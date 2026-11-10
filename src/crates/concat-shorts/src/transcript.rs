// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! What was said in a recording, and when: every word with its start and
//! end in seconds of the file.
//!
//! The words are whisper's, heard by Concat's own transcriber
//! ([`concat_speech::transcribe`]) on this machine; the model is
//! downloaded the first time it is asked for. Everything downstream reads
//! this one list: the moments worth cutting are found in its sentences,
//! a clip's edges are moved onto its word boundaries, and captions are
//! its words shown as they are said.
//!
//! A transcript is written beside its job as `transcript.json`, so a
//! recording is transcribed once however many times its clips are
//! re-planned or re-rendered.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_host::AppDirs;
use concat_speech::JobStage;
use concat_speech::transcribe::{TranscribeRequest, Transcriber};
use serde::{Deserialize, Serialize};

use crate::{Error, clock};

/// A pause between two words this long ends a sentence whatever the
/// punctuation says: whisper leaves full stops out of fast speech, and
/// nobody pauses a second and a half mid-sentence.
const SENTENCE_PAUSE: f64 = 1.5;
/// The most words in one sentence; a run with no stop in it is broken
/// here, so no "sentence" is longer than a clip.
const SENTENCE_WORDS: usize = 48;

/// One word, in seconds of the recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// When it starts.
    pub start: f64,
    /// When it ends.
    pub end: f64,
    /// The word, punctuation attached.
    pub text: String,
}

/// Everything said in a recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Transcript {
    /// The whisper model that heard it.
    pub model: String,
    /// The recording's length in seconds.
    pub duration: f64,
    /// Every word, in order.
    pub words: Vec<Word>,
}

/// A run of words that reads as one sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct Sentence {
    /// The index of its first word.
    pub first: usize,
    /// The index of its last word.
    pub last: usize,
    /// When its first word starts.
    pub start: f64,
    /// When its last word ends.
    pub end: f64,
    /// Its words, joined.
    pub text: String,
}

impl Sentence {
    /// How many words it has.
    pub fn len(&self) -> usize {
        self.last + 1 - self.first
    }

    /// A sentence always has a word.
    pub fn is_empty(&self) -> bool {
        false
    }
}

/// Whether a word closes a sentence: it ends in a stop, and is not one of
/// the abbreviations that end in one without closing anything.
fn closes(word: &str) -> bool {
    let trimmed = word.trim_end_matches(['"', '\'', ')', ']', '”', '’']);
    let Some(last) = trimmed.chars().last() else {
        return false;
    };
    if !matches!(last, '.' | '?' | '!' | '…') {
        return false;
    }
    if last != '.' {
        return true;
    }
    let lower = trimmed.to_lowercase();
    const ABBREVIATIONS: [&str; 10] = [
        "mr.", "mrs.", "ms.", "dr.", "prof.", "st.", "vs.", "no.", "inc.", "ltd.",
    ];
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return false;
    }
    let body = lower.trim_end_matches('.');
    // Stops inside the word ("p.m.", "e.g.", "U.S.") are an abbreviation's.
    if body.contains('.') {
        return false;
    }
    // An initial ("J.") or a list number ("1.") closes nothing.
    !(body.chars().count() == 1 && body.chars().all(char::is_alphanumeric))
}

impl Transcript {
    /// The words as sentences: closed by a stop, by a long pause, or by
    /// running too long without either.
    pub fn sentences(&self) -> Vec<Sentence> {
        let mut out = Vec::new();
        let mut first = 0usize;
        for index in 0..self.words.len() {
            let word = &self.words[index];
            let pause = self
                .words
                .get(index + 1)
                .is_none_or(|next| next.start - word.end >= SENTENCE_PAUSE);
            let long = index + 1 - first >= SENTENCE_WORDS;
            if closes(&word.text) || pause || long {
                out.push(self.sentence(first, index));
                first = index + 1;
            }
        }
        out
    }

    fn sentence(&self, first: usize, last: usize) -> Sentence {
        Sentence {
            first,
            last,
            start: self.words[first].start,
            end: self.words[last].end,
            text: self.text_of(first, last),
        }
    }

    /// The words `first..=last`, joined by spaces.
    pub fn text_of(&self, first: usize, last: usize) -> String {
        self.words
            .get(first..=last.min(self.words.len().saturating_sub(1)))
            .unwrap_or_default()
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The words said between two moments: those whose middle falls
    /// inside, so a word cut by an edge belongs to the side most of it is
    /// on.
    pub fn between(&self, start: f64, end: f64) -> &[Word] {
        let middle = |word: &Word| (word.start + word.end) / 2.0;
        let from = self.words.partition_point(|word| middle(word) < start);
        let to = self.words.partition_point(|word| middle(word) <= end);
        &self.words[from..to.max(from)]
    }

    /// A clip's edges moved onto the words nearest them: `start` to just
    /// before the word that begins closest to it, `end` to just after the
    /// word that ends closest to it, when there is one within a second.
    /// "Just" is a little air, and never into the neighbouring word. So a
    /// time read off a player, or picked from the transcript's sentence
    /// times, cuts between words and not through one.
    pub fn snap(&self, start: f64, end: f64) -> (f64, f64) {
        const REACH: f64 = 1.0;
        const LEAD: f64 = 0.15;
        const TAIL: f64 = 0.25;
        let nearest = |at: f64, edge: fn(&Word) -> f64| {
            self.words
                .iter()
                .enumerate()
                .map(|(index, word)| (index, (edge(word) - at).abs()))
                .filter(|(_, distance)| *distance <= REACH)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(index, _)| index)
        };
        let mut snapped_start = start;
        if let Some(index) = nearest(start, |word| word.start) {
            let word = &self.words[index];
            let floor = match index.checked_sub(1).map(|before| &self.words[before]) {
                Some(before) => (before.end + word.start) / 2.0,
                None => 0.0,
            };
            snapped_start = (word.start - LEAD).max(floor.min(word.start)).max(0.0);
        }
        let mut snapped_end = end;
        if let Some(index) = nearest(end, |word| word.end) {
            let word = &self.words[index];
            let ceiling = match self.words.get(index + 1) {
                Some(after) => (word.end + after.start) / 2.0,
                None => self.duration.max(word.end),
            };
            snapped_end = (word.end + TAIL).min(ceiling.max(word.end));
        }
        if snapped_end - snapped_start < 0.5 {
            return (start, end);
        }
        (snapped_start, snapped_end)
    }

    /// The transcript as an agent reads it: a sentence a line, each led by
    /// when it starts, from `from` seconds on. At most `limit` characters;
    /// when there is more, the second value is where the next page starts.
    pub fn page(&self, from: f64, to: f64, limit: usize) -> (String, Option<f64>) {
        let mut text = String::new();
        for sentence in self.sentences() {
            if sentence.end < from {
                continue;
            }
            if sentence.start > to {
                break;
            }
            let line = format!("[{}] {}\n", clock(sentence.start.floor()), sentence.text);
            if !text.is_empty() && text.len() + line.len() > limit {
                return (text, Some(sentence.start));
            }
            text.push_str(&line);
        }
        (text, None)
    }

    /// Reads a transcript written by [`Transcript::save`]; `None` when
    /// there is none, or it does not read.
    pub fn load(path: &Path) -> Option<Transcript> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Writes the transcript to `path`.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        // Compact: an hour of words is tens of thousands of entries, and
        // nobody reads this file by eye.
        let text = serde_json::to_string(self)
            .map_err(|error| Error::Failed(format!("the transcript did not encode: {error}")))?;
        let partial = path.with_extension(format!("part-{}", std::process::id()));
        std::fs::write(&partial, text)
            .and_then(|()| std::fs::rename(&partial, path))
            .map_err(|error| Error::Failed(format!("could not write {}: {error}", path.display())))
    }
}

/// Makes sure whisper model `model` is on disk, downloading it if it is
/// not. `progress` is told how far the download is.
pub fn ensure_model(
    dirs: &AppDirs,
    transcriber: &Transcriber,
    model: &str,
    progress: &mut dyn FnMut(Option<f64>, &str),
) -> Result<(), Error> {
    let status = Transcriber::status(dirs).map_err(Error::Failed)?;
    let Some(known) = status.models.iter().find(|known| known.id == model) else {
        return Err(Error::Invalid(format!(
            "there is no transcription model called {model:?}"
        )));
    };
    if known.downloaded {
        return Ok(());
    }
    let megabytes = known.size_bytes as f64 / 1_048_576.0;
    progress(
        Some(0.0),
        &format!("downloading the {model} speech model ({megabytes:.0} MB, once)"),
    );
    transcriber
        .download_model(dirs, model, |download| {
            let fraction = (download.total > 0)
                .then(|| (download.received as f64 / download.total as f64).min(1.0));
            progress(
                fraction,
                &format!(
                    "downloading the {model} speech model, {:.0} of {megabytes:.0} MB",
                    download.received as f64 / 1_048_576.0
                ),
            );
        })
        .map_err(|error| {
            Error::Failed(format!(
                "the {model} speech model could not be downloaded: {error}"
            ))
        })
}

/// Transcribes the whole of `media`, `duration` seconds long, with whisper
/// model `model`. Blocks for the run - minutes for an hour of talk -
/// telling `progress` how far along it is; `cancel` is looked at as it
/// goes, and the transcriber's own cancel stops whisper itself.
pub fn transcribe(
    dirs: &AppDirs,
    transcriber: &Transcriber,
    media: &Path,
    duration: f64,
    model: &str,
    cancel: &AtomicBool,
    progress: impl FnMut(Option<f64>, &str) + Send + 'static,
) -> Result<Transcript, Error> {
    let request = TranscribeRequest {
        path: media.to_string_lossy().into_owned(),
        audio_stream: None,
        source_start: 0.0,
        window: duration,
        model_id: model.to_owned(),
        language: None,
    };
    let mut progress = progress;
    let lines = transcriber
        .transcribe_words(dirs, &request, move |stage| match stage {
            JobStage::Reading => progress(None, "reading the sound"),
            JobStage::Loading => progress(None, "loading the speech model"),
            JobStage::Working { done, .. } => {
                let done = f64::from(done).clamp(0.0, 1.0);
                progress(Some(done), &format!("transcribing, {:.0}%", done * 100.0));
            }
        })
        .map_err(Error::Failed)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Failed("transcription cancelled".to_owned()));
    }
    let mut words: Vec<Word> = Vec::new();
    for line in lines {
        for word in line.words {
            let text = word.text.trim();
            if text.is_empty() {
                continue;
            }
            // In order and inside the recording, whatever came back.
            let floor = words.last().map_or(0.0, |last: &Word| last.start);
            let start = word.start.clamp(floor, duration.max(floor));
            let end = word.end.clamp(start, duration.max(start));
            words.push(Word {
                start,
                end,
                text: text.to_owned(),
            });
        }
    }
    Ok(Transcript {
        model: model.to_owned(),
        duration,
        words,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A transcript of `text`, a word every `pace` seconds with a gap of
    /// `gap` after each word ending a sentence, starting at `start`.
    pub(crate) fn spoken(text: &str, start: f64, pace: f64, gap: f64) -> Transcript {
        let mut words = Vec::new();
        let mut at = start;
        for word in text.split_whitespace() {
            words.push(Word {
                start: at,
                end: at + pace * 0.8,
                text: word.to_owned(),
            });
            at += pace;
            if closes(word) {
                at += gap;
            }
        }
        Transcript {
            model: "test".to_owned(),
            duration: at + 1.0,
            words,
        }
    }

    #[test]
    fn sentences_close_on_stops_and_not_on_abbreviations() {
        let transcript = spoken(
            "Dr. Lee met J. Smith at 3 p.m. today. Did it work? Yes! It did",
            0.0,
            0.4,
            0.2,
        );
        let sentences: Vec<String> = transcript
            .sentences()
            .into_iter()
            .map(|sentence| sentence.text)
            .collect();
        assert_eq!(
            sentences,
            [
                "Dr. Lee met J. Smith at 3 p.m. today.",
                "Did it work?",
                "Yes!",
                "It did"
            ]
        );
    }

    #[test]
    fn a_long_pause_or_a_long_run_closes_a_sentence() {
        let mut transcript = spoken("one two three four", 0.0, 0.5, 0.0);
        transcript.words[2].start += 3.0;
        transcript.words[2].end += 3.0;
        transcript.words[3].start += 3.0;
        transcript.words[3].end += 3.0;
        let sentences = transcript.sentences();
        assert_eq!(sentences.len(), 2);
        assert_eq!(sentences[0].text, "one two");
        assert_eq!(sentences[1].first, 2);

        let run = spoken(&"word ".repeat(100), 0.0, 0.3, 0.0);
        let sentences = run.sentences();
        assert_eq!(sentences.len(), 3);
        assert_eq!(sentences[0].len(), SENTENCE_WORDS);
    }

    #[test]
    fn the_words_between_two_moments_are_those_mostly_inside() {
        let transcript = spoken("zero one two three four", 0.0, 1.0, 0.0);
        let texts = |start: f64, end: f64| -> Vec<&str> {
            transcript
                .between(start, end)
                .iter()
                .map(|word| word.text.as_str())
                .collect()
        };
        assert_eq!(texts(0.0, 10.0), ["zero", "one", "two", "three", "four"]);
        // "one" runs 1.0-1.8: a window from 1.3 has most of it.
        assert_eq!(texts(1.3, 3.5), ["one", "two", "three"]);
        assert_eq!(texts(1.5, 2.2), Vec::<&str>::new());
        assert!(texts(9.0, 12.0).is_empty());
    }

    #[test]
    fn edges_are_moved_onto_the_nearest_words() {
        // Words at 10.0-10.8, 11.0-11.8 ... 19.0-19.8.
        let transcript = spoken("a b c d e f g h i j", 10.0, 1.0, 0.0);
        let (start, end) = transcript.snap(12.3, 16.6);
        // "c" starts at 12.0; a little air before it, not into "b" (ends 11.8).
        assert!((start - 11.9).abs() < 1e-9, "{start}");
        // "g" ends at 16.8; a little air after, not into "h" (starts 17.0).
        assert!((end - 16.9).abs() < 1e-9, "{end}");
        // Nothing within reach: the times are kept.
        assert_eq!(transcript.snap(2.0, 5.0), (2.0, 5.0));
        // The first word has nothing before it to stay out of.
        let (start, _) = transcript.snap(10.1, 15.0);
        assert!((start - 9.85).abs() < 1e-9, "{start}");
    }

    #[test]
    fn a_page_is_sentences_with_their_times_and_says_where_the_next_starts() {
        let transcript = spoken("First thing. Second thing. Third thing.", 59.0, 0.5, 0.5);
        let (all, next) = transcript.page(0.0, f64::MAX, 10_000);
        assert_eq!(
            all,
            "[0:59] First thing.\n[1:00] Second thing.\n[1:02] Third thing.\n"
        );
        assert_eq!(next, None);
        let (first, next) = transcript.page(0.0, f64::MAX, 30);
        assert_eq!(first, "[0:59] First thing.\n");
        let next = next.expect("more to read");
        let (rest, _) = transcript.page(next, f64::MAX, 10_000);
        assert!(rest.starts_with("[1:00] Second thing."));
    }

    #[test]
    fn a_transcript_is_read_back_from_its_file() {
        let folder = tempfile::tempdir().expect("a scratch folder");
        let path = folder.path().join("transcript.json");
        assert_eq!(Transcript::load(&path), None);
        // Times a decimal carries exactly, so what is read is what was
        // written to the last bit.
        let transcript = Transcript {
            model: "test".to_owned(),
            duration: 4.5,
            words: vec![
                Word {
                    start: 2.0,
                    end: 2.25,
                    text: "Saved".to_owned(),
                },
                Word {
                    start: 2.5,
                    end: 2.75,
                    text: "and".to_owned(),
                },
                Word {
                    start: 3.0,
                    end: 3.5,
                    text: "read.".to_owned(),
                },
            ],
        };
        transcript.save(&path).expect("saves");
        assert_eq!(Transcript::load(&path), Some(transcript));
    }
}
