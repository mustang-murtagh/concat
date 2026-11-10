// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! Where a recording goes quiet, and what is left of a clip once its
//! pauses are tightened.
//!
//! The recording's loudness is measured a hundredth of a second at a time
//! and the stretches that fall well under its speech are its quiet spans.
//! "Well under" is set against the recording itself - a headset in a
//! silent room and a laptop microphone in a café have nothing in common
//! but the gap between the voice and what is behind it - and a recording
//! with no such gap (music throughout, a noise as loud as the speaker)
//! has no quiet spans and is cut nowhere.
//!
//! Tightening a clip ([`kept`]) is then arithmetic: a quiet span long
//! enough to be dead air is cut down to a breath either side, and quiet at
//! the clip's own two ends is trimmed to the same breath, so a clip starts
//! on its first word and stops after its last. What comes back is the
//! stretches of the recording to keep, in order; [`Tightened`] maps a
//! moment of the recording to where it lands once they are laid end to
//! end, which is how a caption finds its word again.
//!
//! Energy decides, not the transcript: whisper's word times are good to a
//! tenth of a second or so, and a cut placed by them alone clips
//! consonants. The words pick the neighbourhood of an edge
//! ([`crate::transcript::Transcript::snap`]); the sound settles it.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_media::{AudioDecoder, AudioOptions, SampleFormat};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::settings::Tighten;

/// The rate the sound is measured at: speech has nothing above it worth
/// weighing, and whisper asked for the same.
const RATE: u32 = 16_000;
/// Samples in one measure: a hundredth of a second.
const WINDOW: usize = 160;
/// Seconds one measure covers.
pub const STEP: f64 = WINDOW as f64 / RATE as f64;
/// The level of digital silence, in decibels: as quiet as a measure gets.
const SILENT: f32 = -100.0;
/// How far under the speech a measure must fall to be quiet.
const UNDER_SPEECH: f32 = 28.0;
/// The least the speech must stand above what is behind it for quiet to
/// mean anything.
const LEAST_GAP: f32 = 12.0;
/// A burst of sound this short inside a quiet span is a click, not a word.
const CLICK: f64 = 0.04;
/// The shortest stretch worth calling quiet.
const SHORTEST_QUIET: f64 = 0.25;
/// The shortest stretch kept: anything less between two cuts is a blip of
/// noise, and goes with them.
const SHORTEST_KEPT: f64 = 0.15;

/// How near a clip's edge a quiet span may end and still be taken for the
/// quiet at that edge.
const EDGE: f64 = 0.3;

/// The quiet spans of a recording, each `(start, end)` in seconds, in
/// order and apart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Quiet {
    /// The spans.
    pub spans: Vec<(f64, f64)>,
}

impl Quiet {
    /// Reads spans written by [`Quiet::save`].
    pub fn load(path: &Path) -> Option<Quiet> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    /// Writes the spans to `path`.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let text = serde_json::to_string(self)
            .map_err(|error| Error::Failed(format!("the quiet spans did not encode: {error}")))?;
        let partial = path.with_extension(format!("part-{}", std::process::id()));
        std::fs::write(&partial, text)
            .and_then(|()| std::fs::rename(&partial, path))
            .map_err(|error| Error::Failed(format!("could not write {}: {error}", path.display())))
    }
}

/// The loudness of `media`'s first sound stream, a decibel figure every
/// [`STEP`] seconds. Reads the whole file; `progress` is told how far it
/// is through `duration` seconds.
pub fn levels(
    media: &Path,
    duration: f64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Option<f64>, &str),
) -> Result<Vec<f32>, Error> {
    let options = AudioOptions {
        start: None,
        duration: None,
        filters: Vec::new(),
        rate: RATE,
        channels: 1,
        format: SampleFormat::F32,
        stream: None,
    };
    let mut decoder = AudioDecoder::open(media, &options)
        .map_err(|error| Error::Failed(format!("the sound could not be read: {error}")))?;
    let mut levels: Vec<f32> = Vec::with_capacity((duration.max(0.0) / STEP) as usize + 16);
    let mut sum = 0.0f64;
    let mut count = 0usize;
    let mut reported = 0usize;
    while let Some(samples) = decoder
        .next_f32()
        .map_err(|error| Error::Failed(format!("the sound could not be read: {error}")))?
    {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Failed("cancelled".to_owned()));
        }
        for sample in samples {
            sum += f64::from(sample) * f64::from(sample);
            count += 1;
            if count == WINDOW {
                levels.push(decibels(sum / WINDOW as f64));
                sum = 0.0;
                count = 0;
            }
        }
        // Every ten seconds of sound.
        if levels.len() - reported >= 1000 {
            reported = levels.len();
            let done = (duration > 0.0).then(|| (levels.len() as f64 * STEP / duration).min(1.0));
            progress(done, "listening for pauses");
        }
    }
    if count > WINDOW / 2 {
        levels.push(decibels(sum / count as f64));
    }
    Ok(levels)
}

/// A mean square as decibels, floored at [`SILENT`].
fn decibels(mean_square: f64) -> f32 {
    if mean_square <= 1e-10 {
        return SILENT;
    }
    (10.0 * mean_square.log10()).max(f64::from(SILENT)) as f32
}

/// The level under which a measure is quiet, for a recording with these
/// levels; `None` when its speech does not stand clear of what is behind
/// it, and nothing in it can be called a pause.
pub fn threshold(levels: &[f32]) -> Option<f32> {
    if levels.len() < 50 {
        return None;
    }
    let mut sorted = levels.to_vec();
    sorted.sort_by(f32::total_cmp);
    let at = |fraction: f64| sorted[((sorted.len() - 1) as f64 * fraction) as usize];
    // The speech is the loud end; what is behind it, the quiet end - the
    // very quiet end, since a talk with few pauses in it is quiet for only
    // a few hundredths of its length.
    let speech = at(0.95);
    let floor = at(0.03);
    if speech <= SILENT + 1.0 || speech - floor < LEAST_GAP {
        return None;
    }
    // Well under the speech, but clear of the floor: in a noisy room the
    // floor is not far down, and the line sits just above it.
    Some((speech - UNDER_SPEECH).max(floor + 5.0).min(speech - LEAST_GAP + 2.0))
}

/// The quiet spans in `levels`: runs under the recording's threshold, with
/// clicks inside them ignored, at least [`SHORTEST_QUIET`] long.
pub fn quiet(levels: &[f32]) -> Quiet {
    let Some(threshold) = threshold(levels) else {
        return Quiet::default();
    };
    // Runs of quiet measures, as index ranges.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<usize> = None;
    for (index, level) in levels.iter().enumerate() {
        match (*level < threshold, open) {
            (true, None) => open = Some(index),
            (false, Some(from)) => {
                runs.push((from, index));
                open = None;
            }
            _ => {}
        }
    }
    if let Some(from) = open {
        runs.push((from, levels.len()));
    }
    // Two runs with only a click between them are one.
    let click = (CLICK / STEP).round() as usize;
    let mut joined: Vec<(usize, usize)> = Vec::new();
    for run in runs {
        match joined.last_mut() {
            Some(last) if run.0 - last.1 <= click => last.1 = run.1,
            _ => joined.push(run),
        }
    }
    Quiet {
        spans: joined
            .into_iter()
            .map(|(from, to)| (from as f64 * STEP, to as f64 * STEP))
            .filter(|(from, to)| to - from >= SHORTEST_QUIET)
            .collect(),
    }
}

/// The stretches of `start..end` to keep, in order: the clip with its dead
/// air cut down to a breath (`tighten.keep`) either side of each pause of
/// `tighten.min_pause` or more, and the quiet at its two ends trimmed to
/// the same breath. With tightening off, only the ends are trimmed. A clip
/// that is quiet throughout is kept whole: there is nothing to cut to.
pub fn kept(start: f64, end: f64, quiet: &Quiet, tighten: &Tighten) -> Vec<(f64, f64)> {
    if end <= start {
        return Vec::new();
    }
    let keep = tighten.keep.max(0.0);
    let mut cuts: Vec<(f64, f64)> = Vec::new();
    for &(from, to) in &quiet.spans {
        if to <= start || from >= end {
            continue;
        }
        // A span that all but reaches an edge is that edge's quiet: what
        // would be left between them is the first breath of whatever comes
        // next, too short to be anything but a blip.
        let (at_start, at_end) = (from <= start + EDGE, to >= end - EDGE);
        let (from, to) = (from.max(start), to.min(end));
        let cut = match (at_start, at_end) {
            // Quiet from edge to edge.
            (true, true) => return vec![(start, end)],
            // The lead-in: everything up to a breath before the first sound.
            (true, false) => (start, to - keep),
            // The tail: from a breath after the last sound.
            (false, true) => (from + keep, end),
            (false, false) if tighten.enabled && to - from >= tighten.min_pause => {
                (from + keep, to - keep)
            }
            (false, false) => continue,
        };
        if cut.1 - cut.0 > 0.02 {
            cuts.push(cut);
        }
    }
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut at = start;
    for (from, to) in cuts {
        if from - at >= SHORTEST_KEPT {
            out.push((at, from));
        }
        at = at.max(to);
    }
    if end - at >= SHORTEST_KEPT {
        out.push((at, end));
    }
    if out.is_empty() {
        return vec![(start, end)];
    }
    out
}

/// A clip's kept stretches, and the arithmetic of laying them end to end.
#[derive(Clone, Debug, PartialEq)]
pub struct Tightened {
    /// The stretches of the recording kept, in order.
    pub kept: Vec<(f64, f64)>,
}

impl Tightened {
    /// The kept stretches of `start..end`; see [`kept`].
    pub fn of(start: f64, end: f64, quiet: &Quiet, tighten: &Tighten) -> Tightened {
        Tightened {
            kept: kept(start, end, quiet, tighten),
        }
    }

    /// How long the clip runs once its stretches are end to end.
    pub fn length(&self) -> f64 {
        self.kept.iter().map(|(from, to)| to - from).sum()
    }

    /// How much was cut out between the first kept moment and the last.
    pub fn removed(&self) -> f64 {
        match (self.kept.first(), self.kept.last()) {
            (Some(first), Some(last)) => (last.1 - first.0) - self.length(),
            _ => 0.0,
        }
    }

    /// Where a moment of the recording lands on the clip's own timeline. A
    /// moment that was cut lands on the cut: the end of the stretch before
    /// it.
    pub fn place(&self, source: f64) -> f64 {
        let mut before = 0.0;
        for &(from, to) in &self.kept {
            if source < from {
                return before;
            }
            if source <= to {
                return before + (source - from);
            }
            before += to - from;
        }
        before
    }

    /// Every boundary between a kept stretch and a cut one, in seconds of
    /// the recording: where the recording is split to take the cuts out.
    pub fn boundaries(&self) -> Vec<f64> {
        self.kept
            .iter()
            .flat_map(|&(from, to)| [from, to])
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Levels for a recording of speech at -20 dB with silence at -70 dB
    /// over the given spans.
    pub(crate) fn recording(seconds: f64, silences: &[(f64, f64)]) -> Vec<f32> {
        (0..(seconds / STEP) as usize)
            .map(|index| {
                let at = index as f64 * STEP;
                if silences.iter().any(|(from, to)| at >= *from && at < *to) {
                    -70.0
                } else {
                    -20.0
                }
            })
            .collect()
    }

    fn tighten() -> Tighten {
        Tighten {
            enabled: true,
            min_pause: 0.7,
            keep: 0.2,
        }
    }

    fn close(a: &[(f64, f64)], b: &[(f64, f64)]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(a, b)| (a.0 - b.0).abs() < 0.011 && (a.1 - b.1).abs() < 0.011)
    }

    #[test]
    fn quiet_is_measured_against_the_recording_itself() {
        let levels = recording(60.0, &[(10.0, 12.0), (30.0, 30.1), (40.0, 41.0)]);
        let found = quiet(&levels);
        // The tenth of a second is too short to be a pause.
        assert!(close(&found.spans, &[(10.0, 12.0), (40.0, 41.0)]), "{found:?}");

        // The same shape 30 dB down is the same pauses.
        let soft: Vec<f32> = levels.iter().map(|level| level - 30.0).collect();
        assert!(close(&quiet(&soft).spans, &found.spans));

        // Music as loud as the voice throughout: nothing is a pause.
        let flat = vec![-20.0f32; 6000];
        assert_eq!(quiet(&flat), Quiet::default());
        // And neither is anything in digital silence.
        assert_eq!(quiet(&vec![SILENT; 6000]), Quiet::default());
    }

    #[test]
    fn a_click_does_not_split_a_pause() {
        let mut levels = recording(30.0, &[(10.0, 12.0)]);
        // Two hundredths of a second of noise in the middle of the pause.
        levels[1100] = -20.0;
        levels[1101] = -20.0;
        assert!(close(&quiet(&levels).spans, &[(10.0, 12.0)]));
    }

    #[test]
    fn dead_air_is_cut_to_a_breath_and_short_pauses_are_left() {
        let quiet = Quiet {
            spans: vec![(10.0, 12.0), (20.0, 20.5), (30.0, 31.0)],
        };
        let tightened = Tightened::of(5.0, 40.0, &quiet, &tighten());
        assert!(close(
            &tightened.kept,
            &[(5.0, 10.2), (11.8, 30.2), (30.8, 40.0)]
        ));
        assert!((tightened.length() - (35.0 - 1.6 - 0.6)).abs() < 1e-9);
        assert!((tightened.removed() - 2.2).abs() < 1e-9);
    }

    #[test]
    fn quiet_at_the_ends_is_trimmed_even_with_tightening_off() {
        let quiet = Quiet {
            spans: vec![(4.0, 6.0), (10.0, 12.0), (19.5, 25.0)],
        };
        let off = Tighten {
            enabled: false,
            ..tighten()
        };
        let tightened = Tightened::of(5.0, 20.0, &quiet, &off);
        assert!(close(&tightened.kept, &[(5.8, 19.7)]), "{tightened:?}");
        let on = Tightened::of(5.0, 20.0, &quiet, &tighten());
        assert!(close(&on.kept, &[(5.8, 10.2), (11.8, 19.7)]), "{on:?}");
    }

    #[test]
    fn quiet_that_all_but_reaches_an_edge_is_that_edges() {
        // The pause after the last word runs to a fifth of a second
        // before the clip's end: no sliver of what follows is kept.
        let quiet = Quiet {
            spans: vec![(4.9, 5.2), (17.0, 19.8)],
        };
        let tightened = Tightened::of(5.0, 20.0, &quiet, &tighten());
        assert!(close(&tightened.kept, &[(5.0, 17.2)]), "{tightened:?}");
    }

    #[test]
    fn a_clip_with_nothing_to_cut_is_kept_whole() {
        let none = Quiet::default();
        assert_eq!(kept(3.0, 9.0, &none, &tighten()), [(3.0, 9.0)]);
        let all = Quiet {
            spans: vec![(0.0, 100.0)],
        };
        assert_eq!(kept(3.0, 9.0, &all, &tighten()), [(3.0, 9.0)]);
        assert!(kept(9.0, 3.0, &none, &tighten()).is_empty());
    }

    #[test]
    fn a_moment_of_the_recording_finds_its_place_on_the_clip() {
        let tightened = Tightened {
            kept: vec![(5.0, 10.0), (12.0, 20.0)],
        };
        assert_eq!(tightened.place(4.0), 0.0);
        assert_eq!(tightened.place(5.0), 0.0);
        assert_eq!(tightened.place(7.5), 2.5);
        // Cut: it lands on the cut.
        assert_eq!(tightened.place(11.0), 5.0);
        assert_eq!(tightened.place(12.0), 5.0);
        assert_eq!(tightened.place(15.0), 8.0);
        assert_eq!(tightened.place(99.0), 13.0);
        assert_eq!(tightened.boundaries(), [5.0, 10.0, 12.0, 20.0]);
    }

    #[test]
    fn decibels_follow_the_mean_square() {
        assert_eq!(decibels(0.0), SILENT);
        assert!((decibels(1.0) - 0.0).abs() < 1e-6);
        assert!((decibels(0.01) + 20.0).abs() < 1e-4);
    }
}
