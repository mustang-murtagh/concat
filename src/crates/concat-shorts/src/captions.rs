// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! A clip's words laid out as captions: which words share the screen, and
//! from when to when.
//!
//! Words are gathered into phrases short enough for one line of the frame
//! they are for - a tall frame holds two or three words, a wide one six or
//! seven - and a phrase breaks where the speaker does: at a stop, at a
//! pause, or when the line is full. Each phrase is on screen from its
//! first word until just after its last, and never into the next.
//!
//! How a phrase follows the words is the settings' choice
//! ([`CaptionMode`]): all at once; a word at a time; or, by default, the
//! whole phrase with the words still to come held back and each coming to
//! full strength as it is said. That last is one title per word, each the
//! same phrase with a different stretch of it dimmed - the words do not
//! move as they light, because nothing about their size changes.
//!
//! What comes out is times and text ([`Caption`]); turning that into
//! titles in the brand's type is [`crate::compose`]'s.

use crate::settings::{CaptionMode, Captions};

/// A pause between two words this long breaks a phrase.
const BREATH: f64 = 0.7;
/// How long a phrase stays after its last word, when nothing follows.
const LINGER: f64 = 0.3;
/// The least time anything is on screen: two frames at thirty a second.
const SHORTEST: f64 = 0.066;

/// One word of a clip, in seconds of the clip's own timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Spoken {
    /// When it starts.
    pub start: f64,
    /// When it ends.
    pub end: f64,
    /// The word as transcribed.
    pub text: String,
}

/// One title: these words, from `start` to `end`.
#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    /// When it appears, in seconds of the clip.
    pub start: f64,
    /// When it goes.
    pub end: f64,
    /// What it says.
    pub text: String,
    /// The stretch of `text` held back, as a range of characters (not
    /// bytes): the words not yet said. `None` when all of it is lit.
    pub dim: Option<(usize, usize)>,
}

/// A word as a caption shows it: without the commas and full stops that
/// clutter a line read at a glance, unless they are wanted; question and
/// exclamation marks stay, since they change how a line reads.
fn shown(word: &str, captions: &Captions) -> String {
    let mut text = word.trim().to_owned();
    if !captions.punctuation {
        text = text
            .trim_end_matches(['.', ',', ';', ':', '…'])
            .trim_start_matches(['…'])
            .to_owned();
    }
    if captions.uppercase {
        text = text.to_uppercase();
    }
    text
}

/// Whether a word ends what the speaker was saying: a stop of any kind.
fn ends_thought(word: &str) -> bool {
    word.trim_end_matches(['"', '\'', ')', '”', '’'])
        .ends_with(['.', '?', '!', '…'])
}

/// Lays `words` out as captions for a frame whose line holds about `line`
/// characters, on a clip `length` seconds long.
pub fn lay_out(words: &[Spoken], line: usize, length: f64, captions: &Captions) -> Vec<Caption> {
    // The words as shown, with what was said kept for where it breaks.
    let words: Vec<(&Spoken, String)> = words
        .iter()
        .map(|word| (word, shown(&word.text, captions)))
        .filter(|(word, text)| !text.is_empty() && word.start < length)
        .collect();

    // Phrases, as ranges of `words`.
    let mut phrases: Vec<(usize, usize)> = Vec::new();
    let mut first = 0usize;
    let mut width = 0usize;
    for index in 0..words.len() {
        let (word, text) = &words[index];
        let letters = text.chars().count();
        if index > first {
            let paused = word.start - words[index - 1].0.end > BREATH;
            let full = width + 1 + letters > line || index - first >= captions.max_words;
            if paused || full {
                phrases.push((first, index));
                first = index;
                width = 0;
            }
        }
        width += letters + usize::from(index > first);
        let said = index + 1 - first;
        let comma = word.text.trim_end().ends_with([',', ';', ':']) && said >= 3;
        if ends_thought(&word.text) || comma {
            phrases.push((first, index + 1));
            first = index + 1;
            width = 0;
        }
    }
    if first < words.len() {
        phrases.push((first, words.len()));
    }

    let mut out: Vec<Caption> = Vec::new();
    for (number, &(from, to)) in phrases.iter().enumerate() {
        let phrase = &words[from..to];
        // On screen from its first word, and never before the last
        // caption has gone.
        let floor = out.last().map_or(0.0, |last: &Caption| last.end);
        let start = phrase[0].0.start.max(floor);
        let ceiling = phrases
            .get(number + 1)
            .map_or(length, |next| words[next.0].0.start.min(length));
        let last_word = phrase[phrase.len() - 1].0.end;
        let end = (last_word + LINGER)
            .max(start + SHORTEST * 4.0)
            .min(ceiling);
        if end - start < SHORTEST {
            continue;
        }
        let text = phrase
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        match captions.mode {
            CaptionMode::Phrase => out.push(Caption {
                start,
                end,
                text,
                dim: None,
            }),
            CaptionMode::Word => {
                steps(phrase, start, end, |index, from, to| {
                    out.push(Caption {
                        start: from,
                        end: to,
                        text: phrase[index].1.clone(),
                        dim: None,
                    });
                });
            }
            CaptionMode::Highlight => {
                let total = text.chars().count();
                // Where each word ends in the phrase, in characters.
                let mut ends = Vec::with_capacity(phrase.len());
                let mut at = 0usize;
                for (index, (_, text)) in phrase.iter().enumerate() {
                    at += text.chars().count() + usize::from(index > 0);
                    ends.push(at);
                }
                steps(phrase, start, end, |index, from, to| {
                    // Lit up to this word; the space after it and the
                    // rest are still to come.
                    let lit = ends[index];
                    out.push(Caption {
                        start: from,
                        end: to,
                        text: text.clone(),
                        dim: (lit < total).then_some((lit, total)),
                    });
                });
            }
        }
    }
    out
}

/// A phrase's time shared out among its words: `each(index, from, to)` for
/// every word that gets a step, from the moment it is said to the moment
/// the next is, the first from the phrase's start and the last to its end.
/// A word said too fast to be a step of its own shares the next one's, so
/// the step that follows lights both.
fn steps(
    phrase: &[(&Spoken, String)],
    start: f64,
    end: f64,
    mut each: impl FnMut(usize, f64, f64),
) {
    let mut from = start;
    for index in 0..phrase.len() {
        let to = match phrase.get(index + 1) {
            Some((next, _)) => next.start.clamp(from, end),
            None => end,
        };
        if to - from < SHORTEST && index + 1 < phrase.len() {
            continue;
        }
        if to > from {
            each(index, from, to);
        }
        from = to;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str, start: f64, pace: f64) -> Vec<Spoken> {
        text.split_whitespace()
            .enumerate()
            .map(|(index, word)| Spoken {
                start: start + index as f64 * pace,
                end: start + index as f64 * pace + pace * 0.8,
                text: word.to_owned(),
            })
            .collect()
    }

    fn captions(mode: CaptionMode) -> Captions {
        Captions {
            mode,
            max_words: 5,
            ..Captions::default()
        }
    }

    fn texts(captions: &[Caption]) -> Vec<&str> {
        captions
            .iter()
            .map(|caption| caption.text.as_str())
            .collect()
    }

    #[test]
    fn phrases_fit_the_line_and_break_where_the_speaker_does() {
        let words = said("Here is the thing, nobody reads the manual. Ever", 0.0, 0.5);
        let laid = lay_out(&words, 18, 10.0, &captions(CaptionMode::Phrase));
        assert_eq!(
            texts(&laid),
            ["Here is the thing", "nobody reads the", "manual", "Ever"]
        );
        for caption in &laid {
            assert!(caption.text.chars().count() <= 18);
            assert!(caption.end > caption.start);
            assert_eq!(caption.dim, None);
        }
        for pair in laid.windows(2) {
            assert!(pair[0].end <= pair[1].start + 1e-9, "{pair:?}");
        }
        // A wide frame holds more of it on a line.
        let wide = lay_out(&words, 40, 10.0, &captions(CaptionMode::Phrase));
        assert_eq!(
            texts(&wide),
            ["Here is the thing", "nobody reads the manual", "Ever"]
        );
    }

    #[test]
    fn a_pause_breaks_a_phrase_and_the_caption_does_not_sit_through_it() {
        let mut words = said("one two three four", 0.0, 0.4);
        words[2].start += 3.0;
        words[2].end += 3.0;
        words[3].start += 3.0;
        words[3].end += 3.0;
        let laid = lay_out(&words, 40, 10.0, &captions(CaptionMode::Phrase));
        assert_eq!(texts(&laid), ["one two", "three four"]);
        // Gone a moment after "two", well before "three" at 3.8.
        assert!(laid[0].end < 1.2, "{laid:?}");
        assert!((laid[1].start - 3.8).abs() < 1e-9);
    }

    #[test]
    fn highlight_lights_a_phrase_word_by_word_without_changing_it() {
        let words = said("Start with why", 1.0, 0.5);
        let laid = lay_out(&words, 40, 10.0, &captions(CaptionMode::Highlight));
        assert_eq!(texts(&laid), ["Start with why"; 3]);
        // "Start" lit, the rest held back; then "Start with"; then all.
        assert_eq!(laid[0].dim, Some((5, 14)));
        assert_eq!(laid[1].dim, Some((10, 14)));
        assert_eq!(laid[2].dim, None);
        assert_eq!(laid[0].start, 1.0);
        assert_eq!(laid[0].end, 1.5);
        assert_eq!(laid[1].end, 2.0);
        assert!(laid[2].end > 2.4);
    }

    #[test]
    fn word_mode_is_one_word_at_a_time() {
        let words = said("Start with why", 1.0, 0.5);
        let laid = lay_out(&words, 40, 10.0, &captions(CaptionMode::Word));
        assert_eq!(texts(&laid), ["Start", "with", "why"]);
        assert_eq!(laid[1].start, 1.5);
    }

    #[test]
    fn words_said_too_fast_share_a_step() {
        let mut words = said("a b c", 0.0, 0.5);
        // "b" lasts a hundredth of a second before "c".
        words[2].start = 0.51;
        let laid = lay_out(&words, 40, 10.0, &captions(CaptionMode::Highlight));
        assert_eq!(laid.len(), 2);
        assert_eq!(laid[0].dim, Some((1, 5)));
        assert_eq!(laid[1].dim, None);
        assert!((laid[1].start - 0.5).abs() < 1e-9);
    }

    #[test]
    fn punctuation_and_case_follow_the_settings() {
        let words = said("Really? Yes, it works.", 0.0, 0.5);
        let plain = lay_out(&words, 40, 10.0, &captions(CaptionMode::Phrase));
        assert_eq!(texts(&plain), ["Really?", "Yes it works"]);
        let kept = Captions {
            punctuation: true,
            uppercase: true,
            ..captions(CaptionMode::Phrase)
        };
        assert_eq!(
            texts(&lay_out(&words, 40, 10.0, &kept)),
            ["REALLY?", "YES, IT WORKS."]
        );
    }

    #[test]
    fn nothing_is_shown_past_the_end_of_the_clip() {
        let words = said("one two three four five six", 0.0, 1.0);
        let laid = lay_out(&words, 40, 3.5, &captions(CaptionMode::Phrase));
        assert!(
            laid.iter().all(|caption| caption.end <= 3.5 + 1e-9),
            "{laid:?}"
        );
        assert!(lay_out(&[], 40, 3.5, &captions(CaptionMode::Phrase)).is_empty());
    }
}
