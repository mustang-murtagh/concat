// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! Which stretches of a recording stand on their own as a short clip.
//!
//! A suggestion, made from the transcript alone and on this machine: no
//! model reads the words, so what is weighed is what can be counted. Every
//! run of whole sentences that comes to the wanted length once its pauses
//! are tightened is a candidate, and each is scored on five things a
//! viewer would notice:
//!
//! - **how it opens** - a question, a line that promises something ("the
//!   biggest mistake...", "here's how..."), the viewer addressed, a
//!   number; and against it, an opening that leans on what came before
//!   ("And then...", "It was...");
//! - **how it closes** - on a finished sentence, before a pause, and not
//!   where the next sentence carries the thought on;
//! - **whether it starts clean** - after a pause, where a speaker has
//!   turned to something new;
//! - **what is in it** - words this stretch uses and the rest of the
//!   recording mostly does not: the stretch about something in particular;
//! - **its pace** - a steady rate of speech, neither thin nor a rush.
//!
//! The best are taken greedily without overlap. It is a first cut for a
//! person, or an agent that has read the transcript, to approve, move or
//! replace - which is why every suggestion says why it was made.

use std::collections::{HashMap, HashSet};

use crate::settings::{ClipRules, Tighten};
use crate::sound::{Quiet, Tightened};
use crate::transcript::{Sentence, Transcript};

/// A stretch suggested as a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Moment {
    /// Where it starts, in seconds of the recording.
    pub start: f64,
    /// Where it ends.
    pub end: f64,
    /// A few words to call it by, from its opening line.
    pub title: String,
    /// Its opening line.
    pub hook: String,
    /// How it scored, 0 to 1.
    pub score: f64,
    /// Why it was suggested, for a person to read.
    pub reason: String,
    /// How long it runs once tightened, in seconds.
    pub length: f64,
}

/// Words a sentence opens with when it carries on from the one before.
const CONTINUES: &[&str] = &[
    "and", "but", "or", "because", "which", "also", "then", "plus", "whereas",
];
/// Openers that are a speaker finding their feet: fine, but not a hook.
const SOFT_OPENERS: &[&str] = &[
    "so", "okay", "ok", "right", "well", "now", "yeah", "yes", "um", "uh",
];
/// Openers that point back at something the viewer has not heard.
const POINTS_BACK: &[&str] = &[
    "it", "it's", "its", "they", "they're", "he", "she", "those", "that", "that's",
];
/// Ways a line promises the viewer something.
const PROMISES: &[&str] = &[
    "how to",
    "how do",
    "how i",
    "how we",
    "what if",
    "what i",
    "what most",
    "why",
    "here's",
    "here is",
    "the biggest",
    "the best",
    "the worst",
    "the problem",
    "the truth",
    "the secret",
    "the reason",
    "the first",
    "the one thing",
    "the mistake",
    "the key",
    "the number one",
    "the fastest",
    "the easiest",
    "most people",
    "nobody",
    "everyone",
    "stop",
    "never",
    "imagine",
    "let me tell",
    "let me show",
    "if you",
    "when you",
    "have you ever",
    "do you",
    "did you know",
    "there are",
    "one of the",
    "my biggest",
];
/// Words that give a line weight.
const WEIGHTY: &[&str] = &[
    "mistake",
    "mistakes",
    "secret",
    "truth",
    "problem",
    "wrong",
    "never",
    "always",
    "biggest",
    "best",
    "worst",
    "simple",
    "easy",
    "hard",
    "surprising",
    "actually",
    "important",
    "key",
    "tip",
    "lesson",
    "learned",
    "changed",
    "difference",
    "nobody",
];
/// Ways a line closes a thought.
const CONCLUDES: &[&str] = &[
    "that's why",
    "that is why",
    "that's how",
    "that's it",
    "that's the",
    "the point is",
    "which means",
    "so that",
    "in the end",
    "at the end of the day",
    "bottom line",
    "simple as that",
    "remember",
    "and that",
];
/// Numbers spelt out.
const NUMBERS: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "twenty",
    "fifty", "hundred", "thousand", "million", "percent",
];
/// Words too common to say what a stretch is about.
const COMMON: &[&str] = &[
    "about",
    "after",
    "again",
    "also",
    "always",
    "another",
    "around",
    "because",
    "been",
    "before",
    "being",
    "both",
    "came",
    "come",
    "could",
    "does",
    "doing",
    "done",
    "down",
    "each",
    "even",
    "every",
    "from",
    "going",
    "good",
    "have",
    "having",
    "here",
    "into",
    "just",
    "kind",
    "know",
    "like",
    "little",
    "look",
    "made",
    "make",
    "many",
    "maybe",
    "mean",
    "more",
    "most",
    "much",
    "need",
    "never",
    "okay",
    "only",
    "other",
    "over",
    "people",
    "really",
    "right",
    "said",
    "same",
    "say",
    "should",
    "some",
    "something",
    "sort",
    "still",
    "such",
    "take",
    "than",
    "that",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "things",
    "think",
    "this",
    "those",
    "through",
    "time",
    "very",
    "want",
    "well",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "will",
    "with",
    "would",
    "yeah",
    "your",
    "you're",
    "that's",
    "don't",
    "it's",
    "we're",
    "i'm",
];

/// A word as it is compared: lowercase, with the punctuation around it
/// gone and the apostrophe inside it kept.
fn plain(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .chars()
        .map(|c| if c == '’' { '\'' } else { c })
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether a plain word says what a stretch is about: long enough, and not
/// one everybody uses everywhere.
fn telling(word: &str) -> bool {
    word.chars().count() >= 4 && !COMMON.contains(&word)
}

/// Whether `line` opens with the words of `phrase`, whole: "stop" opens
/// "stop doing this" and not "stopped".
fn opens_with(line: &str, phrase: &str) -> bool {
    line.strip_prefix(phrase)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// A value held to 0..=1.
fn unit(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

/// How well a sentence opens a clip, 0 to 1, and what is good about it.
fn opening(sentence: &str) -> (f64, Option<&'static str>) {
    let words: Vec<String> = sentence.split_whitespace().map(plain).collect();
    let lower = words.join(" ");
    let first = words.first().map(String::as_str).unwrap_or_default();
    let mut score = 0.3;
    let mut why = None;
    if PROMISES.iter().any(|promise| opens_with(&lower, promise)) {
        score += 0.22;
        why = Some("opens on a line that promises something");
    }
    if sentence.trim_end().ends_with('?') {
        score += 0.25;
        why = Some("opens on a question");
    }
    if words
        .iter()
        .any(|word| word == "you" || word == "your" || word == "you're")
    {
        score += 0.1;
    }
    if words
        .iter()
        .any(|word| word.chars().any(|c| c.is_ascii_digit()) || NUMBERS.contains(&word.as_str()))
    {
        score += 0.08;
    }
    if words.iter().any(|word| WEIGHTY.contains(&word.as_str())) {
        score += 0.1;
    }
    score += match words.len() {
        0..=2 => -0.2,
        3..=4 => 0.0,
        5..=18 => 0.1,
        19..=30 => 0.0,
        _ => -0.15,
    };
    if CONTINUES.contains(&first) {
        score -= 0.35;
        why = None;
    } else if SOFT_OPENERS.contains(&first) {
        score -= 0.12;
    } else if POINTS_BACK.contains(&first) {
        score -= 0.15;
    }
    (unit(score), why)
}

/// How well a sentence closes a clip, 0 to 1: `pause` is the silence after
/// it, and `next` the sentence that follows, if one does.
fn closing(sentence: &str, pause: f64, next: Option<&str>) -> f64 {
    let lower = sentence.to_lowercase().replace('’', "'");
    let trimmed = sentence.trim_end_matches(['"', '\'', ')', '”', '’']);
    let mut score = 0.4;
    if trimmed.ends_with(['.', '!']) {
        score += 0.2;
    } else if trimmed.ends_with('?') {
        score += 0.1;
    }
    if pause >= 1.0 {
        score += 0.2;
    } else if pause >= 0.5 {
        score += 0.1;
    }
    if CONCLUDES.iter().any(|cue| lower.contains(cue)) {
        score += 0.15;
    }
    if let Some(next) = next {
        let first = next
            .split_whitespace()
            .next()
            .map(plain)
            .unwrap_or_default();
        if CONTINUES.contains(&first.as_str()) {
            score -= 0.3;
        }
    } else {
        // The end of the recording is as closed as it gets.
        score += 0.2;
    }
    unit(score)
}

/// How steady the pace is, 0 to 1: full marks from two to three and a half
/// words a second, falling away to nothing at one and at five.
fn pace(words: usize, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        return 0.0;
    }
    let rate = words as f64 / seconds;
    if rate < 2.0 {
        unit(rate - 1.0)
    } else if rate <= 3.5 {
        1.0
    } else {
        unit((5.0 - rate) / 1.5)
    }
}

/// A few words to call a clip by, from its opening line: its first words
/// up to a natural stop, without the punctuation, at most `limit` words.
pub fn title_of(line: &str, limit: usize) -> String {
    let mut words: Vec<String> = Vec::new();
    for word in line.split_whitespace() {
        let bare = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’');
        if bare.is_empty() {
            continue;
        }
        words.push(bare.to_owned());
        let stops = word.ends_with([',', ';', ':', '.', '?', '!']);
        if words.len() >= limit || (stops && words.len() >= 3) {
            break;
        }
    }
    // Cut off by the limit, a title should not end on a word that leans
    // on the next one: "give up on a".
    if words.len() >= limit {
        const LEANING: &[&str] = &[
            "a", "an", "the", "of", "on", "in", "to", "is", "and", "or", "for", "with", "at", "by",
            "that", "from", "as", "your", "my", "this", "it's",
        ];
        while words.len() > 3
            && words
                .last()
                .is_some_and(|last| LEANING.contains(&last.to_lowercase().as_str()))
        {
            words.pop();
        }
    }
    let mut title = words.join(" ");
    if let Some(first) = title.chars().next() {
        let upper: String = first.to_uppercase().collect();
        title.replace_range(..first.len_utf8(), &upper);
    }
    if title.is_empty() {
        "Untitled clip".to_owned()
    } else {
        title
    }
}

/// What the recording says between two moments, as a clip would carry it:
/// a title and an opening line. For a stretch somebody else chose.
pub fn describe(transcript: &Transcript, start: f64, end: f64) -> (String, String) {
    let words = transcript.between(start, end);
    let mut hook = String::new();
    for word in words.iter().take(40) {
        if !hook.is_empty() {
            hook.push(' ');
        }
        hook.push_str(&word.text);
        if word.text.ends_with(['.', '?', '!']) && hook.split_whitespace().count() >= 3 {
            break;
        }
    }
    (title_of(&hook, 8), hook)
}

/// One candidate: sentences `first..=last`, scored.
struct Candidate {
    first: usize,
    last: usize,
    start: f64,
    end: f64,
    length: f64,
    removed: f64,
    words: usize,
    hook: f64,
    hook_why: Option<&'static str>,
    close: f64,
    lead: f64,
    substance: f64,
    pace: f64,
}

impl Candidate {
    fn score(&self) -> f64 {
        0.34 * self.hook
            + 0.22 * self.close
            + 0.10 * self.lead
            + 0.22 * self.substance
            + 0.12 * self.pace
    }

    fn reason(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(why) = self.hook_why {
            parts.push(why.to_owned());
        } else if self.hook >= 0.5 {
            parts.push("opens on a strong line".to_owned());
        }
        if self.lead >= 0.9 {
            parts.push("starts after a pause".to_owned());
        }
        if self.close >= 0.75 {
            parts.push("ends on a finished thought".to_owned());
        }
        if self.substance >= 0.75 {
            parts.push("about something specific".to_owned());
        }
        if parts.is_empty() {
            parts.push("the best-rounded stretch of its length".to_owned());
        }
        let mut reason = parts.join("; ");
        if let Some(first) = reason.chars().next() {
            let upper: String = first.to_uppercase().collect();
            reason.replace_range(..first.len_utf8(), &upper);
        }
        let cut = if self.removed >= 0.5 {
            format!(", {:.0} s of pauses cut", self.removed)
        } else {
            String::new()
        };
        format!(
            "{reason}. {} words in {:.0} s{cut}.",
            self.words, self.length
        )
    }
}

/// How much each telling word says, by how few sentences use it.
fn rarity(sentences: &[Sentence], transcript: &Transcript) -> HashMap<String, f64> {
    let mut used: HashMap<String, usize> = HashMap::new();
    for sentence in sentences {
        let distinct: HashSet<String> = transcript.words[sentence.first..=sentence.last]
            .iter()
            .map(|word| plain(&word.text))
            .filter(|word| telling(word))
            .collect();
        for word in distinct {
            *used.entry(word).or_default() += 1;
        }
    }
    let count = sentences.len().max(1) as f64;
    used.into_iter()
        .map(|(word, sentences)| (word, (count / sentences as f64).ln().max(0.0)))
        .collect()
}

/// The moments of `transcript` worth cutting, best first by the clock: at
/// most `rules.max_clips`, each between `rules.min_seconds` and
/// `rules.max_seconds` long once tightened, none overlapping another.
///
/// A recording shorter than the shortest clip is one moment, all of it.
pub fn suggest(
    transcript: &Transcript,
    quiet: &Quiet,
    rules: &ClipRules,
    tighten: &Tighten,
) -> Vec<Moment> {
    let sentences = transcript.sentences();
    if sentences.is_empty() {
        return Vec::new();
    }
    let rarity = rarity(&sentences, transcript);
    let pause_before = |index: usize| match index.checked_sub(1) {
        Some(before) => sentences[index].start - sentences[before].end,
        None => f64::INFINITY,
    };
    let pause_after = |index: usize| match sentences.get(index + 1) {
        Some(after) => after.start - sentences[index].end,
        None => f64::INFINITY,
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    for first in 0..sentences.len() {
        for last in first..sentences.len() {
            let (start, end) = transcript.snap(sentences[first].start, sentences[last].end);
            let tightened = Tightened::of(start, end, quiet, tighten);
            let length = tightened.length();
            if length > rules.max_seconds {
                break;
            }
            if length < rules.min_seconds {
                continue;
            }
            candidates.push(weigh(
                transcript,
                &sentences,
                &rarity,
                first,
                last,
                &tightened,
                pause_before(first),
                pause_after(last),
            ));
        }
    }
    if candidates.is_empty() {
        // Nothing of the wanted length: the recording is shorter than a
        // clip, or one unbroken sentence is longer than one. All of what
        // was said, or the opening of it, is the one suggestion.
        let first = 0;
        let mut last = sentences.len() - 1;
        let fits = |last: usize| {
            let (start, end) = transcript.snap(sentences[first].start, sentences[last].end);
            Tightened::of(start, end, quiet, tighten).length() <= rules.max_seconds
        };
        while last > first && !fits(last) {
            last -= 1;
        }
        let (start, end) = transcript.snap(sentences[first].start, sentences[last].end);
        let mut tightened = Tightened::of(start, end, quiet, tighten);
        if tightened.length() > rules.max_seconds {
            // One sentence too long for a clip: cut at the length.
            let end = start + rules.max_seconds;
            tightened = Tightened::of(start, end, quiet, tighten);
        }
        if tightened.length() < 1.0 {
            return Vec::new();
        }
        candidates.push(weigh(
            transcript,
            &sentences,
            &rarity,
            first,
            last,
            &tightened,
            pause_before(first),
            pause_after(last),
        ));
    }

    // Substance is relative: the stretch with the most to say is the one.
    let richest = candidates
        .iter()
        .map(|candidate| candidate.substance)
        .fold(0.0, f64::max);
    if richest > 0.0 {
        for candidate in &mut candidates {
            candidate.substance /= richest;
        }
    }

    candidates.sort_by(|a, b| {
        b.score()
            .total_cmp(&a.score())
            .then(a.start.total_cmp(&b.start))
    });
    let mut chosen: Vec<&Candidate> = Vec::new();
    for candidate in &candidates {
        if chosen.len() >= rules.max_clips {
            break;
        }
        let overlaps = chosen
            .iter()
            .any(|taken| candidate.first <= taken.last && taken.first <= candidate.last);
        if !overlaps {
            chosen.push(candidate);
        }
    }
    chosen.sort_by(|a, b| a.start.total_cmp(&b.start));
    chosen
        .into_iter()
        .map(|candidate| {
            let hook = sentences[candidate.first].text.clone();
            Moment {
                start: candidate.start,
                end: candidate.end,
                title: title_of(&hook, 8),
                hook,
                score: (candidate.score() * 100.0).round() / 100.0,
                reason: candidate.reason(),
                length: candidate.length,
            }
        })
        .collect()
}

fn weigh(
    transcript: &Transcript,
    sentences: &[Sentence],
    rarity: &HashMap<String, f64>,
    first: usize,
    last: usize,
    tightened: &Tightened,
    pause_before: f64,
    pause_after: f64,
) -> Candidate {
    let words = &transcript.words[sentences[first].first..=sentences[last].last];
    let (hook, hook_why) = opening(&sentences[first].text);
    let close = closing(
        &sentences[last].text,
        pause_after,
        sentences.get(last + 1).map(|next| next.text.as_str()),
    );
    let lead = if pause_before >= 1.0 {
        1.0
    } else if pause_before >= 0.5 {
        0.6
    } else {
        0.3
    };
    let substance = words
        .iter()
        .map(|word| plain(&word.text))
        .map(|word| rarity.get(&word).copied().unwrap_or(0.0))
        .sum::<f64>()
        / words.len().max(1) as f64;
    let length = tightened.length();
    Candidate {
        first,
        last,
        start: tightened
            .kept
            .first()
            .map_or(sentences[first].start, |kept| kept.0),
        end: tightened
            .kept
            .last()
            .map_or(sentences[last].end, |kept| kept.1),
        length,
        removed: tightened.removed(),
        words: words.len(),
        hook,
        hook_why,
        close,
        lead,
        substance,
        pace: pace(words.len(), length),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::tests::spoken;

    fn rules(min: f64, max: f64, clips: usize) -> ClipRules {
        ClipRules {
            min_seconds: min,
            max_seconds: max,
            max_clips: clips,
        }
    }

    fn tighten() -> Tighten {
        Tighten {
            enabled: true,
            min_pause: 0.7,
            keep: 0.2,
        }
    }

    #[test]
    fn an_opening_is_weighed_by_what_a_viewer_hears_first() {
        let (question, why) = opening("Why do most people get this wrong?");
        assert_eq!(why, Some("opens on a question"));
        let (promise, why) = opening("Here's the biggest mistake you can make with a prompt.");
        assert_eq!(why, Some("opens on a line that promises something"));
        let (plain, _) = opening("We went through the settings on the second page.");
        let (carried, why) = opening("And then it stopped working for the whole team.");
        assert_eq!(why, None);
        assert!(
            question > plain && promise > plain,
            "{question} {promise} {plain}"
        );
        assert!(plain > carried, "{plain} {carried}");
        // "stopped" is not the promise "stop".
        assert_eq!(opening("Stopped it was by then.").1, None);
        assert!(opening("Stop writing prompts like this.").1.is_some());
    }

    #[test]
    fn a_close_is_better_finished_and_worse_carried_on() {
        let finished = closing("And that's why it works.", 1.5, Some("Next topic."));
        let carried = closing("It was fine", 0.1, Some("And then it broke."));
        assert!(finished > 0.8, "{finished}");
        assert!(carried < 0.2, "{carried}");
        assert!(closing("That is all.", 0.0, None) > closing("That is all.", 0.0, Some("Next.")));
    }

    #[test]
    fn pace_is_best_at_a_talking_rate() {
        assert_eq!(pace(150, 60.0), 1.0);
        assert!(pace(70, 60.0) < 0.3);
        assert!(pace(280, 60.0) < 0.3);
        assert_eq!(pace(10, 0.0), 0.0);
    }

    #[test]
    fn a_title_is_the_opening_words_without_punctuation() {
        assert_eq!(
            title_of("here's the thing, nobody reads the manual.", 8),
            "Here's the thing"
        );
        assert_eq!(
            title_of("Why do most people get this wrong every single time?", 8),
            "Why do most people get this wrong every"
        );
        assert_eq!(
            title_of("Why do most people give up on a tool after a week?", 8),
            "Why do most people give up"
        );
        assert_eq!(title_of("  ", 8), "Untitled clip");
        assert_eq!(title_of("OK. So, here we go.", 8), "OK So here we go");
    }

    /// A talk of `topics` parts, each opening on a question, with a long
    /// pause between parts; about 20 seconds a part.
    fn talk(topics: usize) -> (Transcript, Quiet) {
        let subjects = [
            "prompts",
            "meetings",
            "spreadsheets",
            "inboxes",
            "calendars",
            "reports",
            "budgets",
            "proposals",
        ];
        let mut text = String::new();
        for topic in 0..topics {
            let subject = subjects[topic % subjects.len()];
            text.push_str(&format!(
                "Why do most people struggle with {subject}? It comes down to three habits around {subject} that nobody teaches. First you write the {subject} down before you start. Then you check the {subject} against what you wanted. And that's why {subject} stop being a problem. "
            ));
        }
        let transcript = spoken(&text, 1.0, 0.4, 0.3);
        (transcript, Quiet::default())
    }

    #[test]
    fn suggestions_are_whole_sentences_of_the_wanted_length_without_overlap() {
        let (transcript, quiet) = talk(8);
        let moments = suggest(&transcript, &quiet, &rules(30.0, 50.0, 3), &tighten());
        assert_eq!(moments.len(), 3);
        for moment in &moments {
            assert!((30.0..=50.0).contains(&moment.length), "{moment:?}");
            // Each opens on a part's question, not mid-thought.
            assert!(moment.hook.starts_with("Why do most people"), "{moment:?}");
            assert!(moment.reason.contains("question"), "{moment:?}");
            assert!(moment.score > 0.0 && moment.score <= 1.0);
        }
        for pair in moments.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{pair:?}");
        }
    }

    #[test]
    fn a_recording_shorter_than_a_clip_is_one_suggestion() {
        let (transcript, quiet) = talk(1);
        let moments = suggest(&transcript, &quiet, &rules(60.0, 90.0, 6), &tighten());
        assert_eq!(moments.len(), 1);
        assert!(moments[0].length < 60.0);
        assert!(moments[0].start < 1.5);
        assert_eq!(moments[0].title, "Why do most people struggle with prompts");
    }

    #[test]
    fn a_recording_with_no_words_has_no_suggestions() {
        let silent = Transcript::default();
        assert!(
            suggest(
                &silent,
                &Quiet::default(),
                &rules(60.0, 90.0, 6),
                &tighten()
            )
            .is_empty()
        );
    }

    #[test]
    fn tightened_pauses_count_towards_the_length() {
        // 40 s of talk with 10 s of dead air in the middle: 50 s on the
        // clock, about 40 s once tightened.
        let mut transcript = spoken(
            &"We keep talking about the plan for the week. ".repeat(11),
            0.0,
            0.4,
            0.0,
        );
        let half = transcript.words.len() / 2;
        for word in &mut transcript.words[half..] {
            word.start += 10.0;
            word.end += 10.0;
        }
        transcript.duration += 10.0;
        let gap = (transcript.words[half - 1].end, transcript.words[half].start);
        let quiet = Quiet { spans: vec![gap] };
        let moments = suggest(&transcript, &quiet, &rules(35.0, 45.0, 1), &tighten());
        assert_eq!(moments.len(), 1, "{moments:?}");
        assert!(moments[0].end - moments[0].start > 45.0);
        assert!(moments[0].length <= 45.0);
        assert!(
            moments[0].reason.contains("pauses cut"),
            "{}",
            moments[0].reason
        );
    }

    #[test]
    fn a_chosen_stretch_is_described_from_its_words() {
        let transcript = spoken(
            "Skip this. Here is the part that matters. And more.",
            0.0,
            1.0,
            0.0,
        );
        let (title, hook) = describe(&transcript, 2.0, 9.0);
        assert_eq!(hook, "Here is the part that matters.");
        assert_eq!(title, "Here is the part that matters");
    }
}
