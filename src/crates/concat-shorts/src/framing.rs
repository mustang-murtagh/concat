// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! How a picture of one shape goes into a frame of another.
//!
//! A wide recording in a tall frame can be cropped to fill it, or shown
//! whole with a blurred copy of itself behind. Which is right depends on
//! what is in the picture, so a few frames of each clip are looked at with
//! the person model Concat ships for cutouts ([`concat_vision::Segmenter`]):
//!
//! - **somebody talking** - one person, taking up a fair part of the
//!   picture - is cropped in on, with the crop centred where they are and
//!   not where the middle of the sensor happened to be;
//! - **a screen recording** - nobody there, or a small face in a corner
//!   bubble - is shown whole, since cropping a screen cuts the words on it
//!   in half;
//! - **several people** - a meeting's grid, a panel - are shown whole too:
//!   any crop loses somebody.
//!
//! The decision can always be made by hand, per clip or for a whole job,
//! and that is all that is used when the person model cannot run on a
//! machine: then every picture that does not fit is shown whole, the
//! choice that never cuts anything off.
//!
//! The geometry is the engine's own: a picture is first fitted or filled
//! by the `frameClip` command, and a crop's centre is then an offset in
//! fractions of the frame ([`Placement`]), which is what
//! `setClipTransform` takes.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_core::Rational;
use concat_media::{DecodeOptions, Decoder, FrameSource};
use concat_vision::{Mask, Segmenter};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::settings::ReframeMode;

/// How many frames of a clip are looked at.
const GLANCES: usize = 9;
/// A mask value from here up is the person.
const PERSON: u8 = 128;
/// The least of the picture a person covers to count as there at all.
const PRESENT: f64 = 0.03;
/// Under this much of the picture, a person is a face in a corner bubble
/// or someone far off: not who the picture is of.
const SUBJECT: f64 = 0.06;
/// A person, or people, spread across more than this much of the width is
/// more than one crop can hold.
const WIDE: f64 = 0.58;
/// The least of a picture wider than its frame a crop may keep; under it
/// the picture is shown whole even with one person in it.
const LEAST_KEPT: f64 = 0.28;
/// The same for a picture taller than its frame.
const LEAST_KEPT_TALL: f64 = 0.5;
/// Where a person's eye line is put in a frame taller than the picture is
/// cropped to, as a fraction of the frame from its top.
const EYE_LINE: f64 = 0.42;

/// What a clip's picture is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Subject {
    /// One person, large enough to be the point of the picture.
    Person,
    /// Nobody, or nobody large: a screen, slides, a scene.
    Screen,
    /// People across the width: a grid, a panel, a wide shot.
    Group,
}

/// What was seen in a clip's picture.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Look {
    /// What it is of.
    pub subject: Subject,
    /// Where the person is across the picture, 0 (left) to 1 (right).
    pub focus_x: f64,
    /// Where their head is down the picture, 0 (top) to 1 (bottom).
    pub focus_y: f64,
    /// How much of the picture they cover, 0 to 1.
    pub covers: f64,
}

/// How one clip's picture is framed, when it is chosen by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Choice {
    /// By what is in the picture and the job's settings.
    #[default]
    Auto,
    /// Cropped to fill the frame.
    Crop,
    /// Whole, over a blurred copy.
    Fit,
}

/// Where a picture sits in a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// Filling the frame, moved by these fractions of the frame's width
    /// and height from the centre (positive is right and down) so the
    /// part that matters is what is left in view.
    Fill {
        /// Fractions of the frame's width.
        offset_x: f64,
        /// Fractions of the frame's height.
        offset_y: f64,
    },
    /// Whole, with bars where the shapes differ.
    Fit,
}

/// What one frame's mask shows: how much is person, where, how wide.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Glance {
    covers: f64,
    center_x: f64,
    head_y: f64,
    spread: f64,
    bodies: usize,
}

/// The point below which `share` of `mass` lies, as a fraction of its
/// length: a weighted quantile.
fn quantile(mass: &[f64], share: f64) -> f64 {
    let total: f64 = mass.iter().sum();
    if total <= 0.0 || mass.is_empty() {
        return 0.5;
    }
    let mut seen = 0.0;
    for (index, amount) in mass.iter().enumerate() {
        seen += amount;
        if seen >= total * share {
            return (index as f64 + 0.5) / mass.len() as f64;
        }
    }
    1.0
}

/// How many separate bodies stand along `columns`: stretches that rise
/// above a quarter of the tallest column, wide enough to be somebody, with
/// a clear gap between one and the next.
fn bodies(columns: &[f64]) -> usize {
    let tallest = columns.iter().copied().fold(0.0, f64::max);
    if tallest <= 0.0 {
        return 0;
    }
    let gap = (columns.len() as f64 * 0.06).ceil() as usize;
    let least = (columns.len() as f64 * 0.04).ceil() as usize;
    // Stretches above the line, as index ranges; two with less than a gap
    // between them are one body with an arm out.
    let mut stretches: Vec<(usize, usize)> = Vec::new();
    for (index, amount) in columns.iter().enumerate() {
        if *amount < tallest * 0.25 {
            continue;
        }
        match stretches.last_mut() {
            Some(last) if index - last.1 < gap => last.1 = index + 1,
            _ => stretches.push((index, index + 1)),
        }
    }
    stretches
        .into_iter()
        .filter(|(from, to)| to - from >= least)
        .count()
}

fn glance(mask: &Mask) -> Glance {
    let (width, height) = (mask.width() as usize, mask.height() as usize);
    let mut columns = vec![0.0f64; width];
    let mut rows = vec![0.0f64; height];
    for (index, value) in mask.bytes().iter().enumerate() {
        if *value >= PERSON {
            columns[index % width] += 1.0;
            rows[index / width] += 1.0;
        }
    }
    let covered: f64 = columns.iter().sum();
    let top = quantile(&rows, 0.04);
    let bottom = quantile(&rows, 0.96);
    Glance {
        covers: covered / (width * height).max(1) as f64,
        center_x: quantile(&columns, 0.5),
        // The eyes are about a sixth of the way down what shows of them.
        head_y: top + (bottom - top) * 0.18,
        spread: quantile(&columns, 0.9) - quantile(&columns, 0.1),
        bodies: bodies(&columns),
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// What the glances add up to.
fn conclude(glances: &[Glance]) -> Look {
    let present: Vec<&Glance> = glances
        .iter()
        .filter(|glance| glance.covers >= PRESENT)
        .collect();
    let covers = median(glances.iter().map(|glance| glance.covers).collect());
    let nobody = Look {
        subject: Subject::Screen,
        focus_x: 0.5,
        focus_y: 0.5,
        covers,
    };
    // There in fewer than half the frames is not who the clip is of.
    if present.len() * 2 < glances.len().max(1) {
        return nobody;
    }
    let covers = median(present.iter().map(|glance| glance.covers).collect());
    if covers < SUBJECT {
        return Look { covers, ..nobody };
    }
    let look = Look {
        subject: Subject::Person,
        focus_x: median(present.iter().map(|glance| glance.center_x).collect()),
        focus_y: median(present.iter().map(|glance| glance.head_y).collect()),
        covers,
    };
    let spread = median(present.iter().map(|glance| glance.spread).collect());
    let several = present.iter().filter(|glance| glance.bodies >= 2).count() * 2 > present.len();
    if spread > WIDE || several {
        return Look {
            subject: Subject::Group,
            ..look
        };
    }
    look
}

/// Looks at `media` between `start` and `end` seconds: a few frames, the
/// person in each, and what that adds up to.
///
/// An error when the person model cannot run here (its runtime is not
/// installed, or the file has no picture); a caller then frames without
/// one.
pub fn look(media: &Path, start: f64, end: f64, cancel: &AtomicBool) -> Result<Look, Error> {
    let segmenter = Segmenter::selfie()
        .map_err(|error| Error::Failed(format!("the person model could not start: {error}")))?;
    let (width, height) = segmenter.input_size(0, 0);
    let span = (end - start).max(0.0);
    let mut glances = Vec::with_capacity(GLANCES);
    for index in 0..GLANCES {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Failed("cancelled".to_owned()));
        }
        // Evenly through the clip, clear of its two ends.
        let at = start + span * (index as f64 + 0.5) / GLANCES as f64;
        let options = DecodeOptions::default()
            .starting_at(Rational::new((at * 1000.0).round() as i64, 1000))
            .scaled_to(width, height)
            .limited_to(1);
        let mut decoder = Decoder::open(media, &options)
            .map_err(|error| Error::Failed(format!("the picture could not be read: {error}")))?;
        let Some(frame) = decoder
            .next_frame()
            .map_err(|error| Error::Failed(format!("the picture could not be read: {error}")))?
        else {
            continue;
        };
        segmenter.begin();
        let mask = segmenter.mask(&frame).map_err(Error::Failed)?;
        glances.push(glance(&mask));
    }
    if glances.is_empty() {
        return Err(Error::Failed(
            "the clip has no picture to look at".to_owned(),
        ));
    }
    Ok(conclude(&glances))
}

/// Where a picture `picture` pixels in size sits in a frame `frame` pixels
/// in size.
///
/// `choice` is the clip's own, `mode` the settings' rule, and `look` what
/// was seen, when anything was. Shapes within a fiftieth of each other
/// simply fill. Otherwise a chosen crop or fit is what it says, and left
/// to itself a picture is cropped only when one person is the point of it
/// and the crop keeps enough of the picture to be worth it.
pub fn place(
    picture: (u32, u32),
    frame: (u32, u32),
    choice: Choice,
    mode: ReframeMode,
    look: Option<&Look>,
) -> Placement {
    let picture_aspect = f64::from(picture.0.max(1)) / f64::from(picture.1.max(1));
    let frame_aspect = f64::from(frame.0.max(1)) / f64::from(frame.1.max(1));
    let ratio = picture_aspect / frame_aspect;
    if (ratio - 1.0).abs() < 0.02 {
        return Placement::Fill {
            offset_x: 0.0,
            offset_y: 0.0,
        };
    }
    // How much of the picture a fill leaves in view.
    let kept = ratio.min(1.0 / ratio);
    let crop = match (choice, mode) {
        (Choice::Crop, _) => true,
        (Choice::Fit, _) => false,
        (Choice::Auto, ReframeMode::Crop) => true,
        (Choice::Auto, ReframeMode::Fit) => false,
        (Choice::Auto, ReframeMode::Auto) => {
            // A wide picture cropped tall keeps a person whole from head
            // to waist; a tall one cropped wide keeps a band across their
            // face, so it has to keep much more to be worth it.
            let least = if ratio > 1.0 {
                LEAST_KEPT
            } else {
                LEAST_KEPT_TALL
            };
            look.is_some_and(|look| look.subject == Subject::Person) && kept >= least
        }
    };
    if !crop {
        return Placement::Fit;
    }
    let person = look.filter(|look| look.subject != Subject::Screen);
    if ratio > 1.0 {
        // Wider than the frame: the picture overhangs left and right, by
        // `ratio` frame widths in all.
        let focus = person.map_or(0.5, |look| look.focus_x);
        let reach = (ratio - 1.0) / 2.0;
        Placement::Fill {
            offset_x: ((0.5 - focus) * ratio).clamp(-reach, reach),
            offset_y: 0.0,
        }
    } else {
        // Taller than the frame: it overhangs top and bottom.
        let tall = 1.0 / ratio;
        let reach = (tall - 1.0) / 2.0;
        let offset = match person {
            Some(look) => (EYE_LINE - 0.5) - (look.focus_y - 0.5) * tall,
            None => 0.0,
        };
        Placement::Fill {
            offset_x: 0.0,
            offset_y: offset.clamp(-reach, reach),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square mask with people standing in the given column ranges, each
    /// from `top` down to the bottom.
    fn mask(people: &[(f64, f64)], top: f64) -> Mask {
        let size = 100u32;
        let mut data = vec![0u8; (size * size) as usize];
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (f64::from(x) / 100.0, f64::from(y) / 100.0);
                if fy >= top && people.iter().any(|(from, to)| fx >= *from && fx < *to) {
                    data[(y * size + x) as usize] = 255;
                }
            }
        }
        Mask::from_bytes(size, size, data).expect("a mask")
    }

    fn seen(masks: &[Mask]) -> Look {
        conclude(&masks.iter().map(glance).collect::<Vec<_>>())
    }

    #[test]
    fn one_person_is_found_where_they_stand() {
        let look = seen(&vec![mask(&[(0.55, 0.85)], 0.2); 5]);
        assert_eq!(look.subject, Subject::Person);
        assert!((look.focus_x - 0.7).abs() < 0.02, "{look:?}");
        // The head is near the top of them, not their middle.
        assert!(look.focus_y > 0.2 && look.focus_y < 0.45, "{look:?}");
        assert!((look.covers - 0.24).abs() < 0.01, "{look:?}");
    }

    #[test]
    fn nobody_or_a_face_in_a_bubble_is_a_screen() {
        assert_eq!(seen(&vec![mask(&[], 0.0); 5]).subject, Subject::Screen);
        // A webcam bubble in the corner: 4 % of the picture.
        let bubble = mask(&[(0.8, 1.0)], 0.8);
        assert_eq!(seen(&vec![bubble; 5]).subject, Subject::Screen);
        // Somebody walks through two frames of nine.
        let mut passing = vec![mask(&[], 0.0); 7];
        passing.extend(vec![mask(&[(0.3, 0.6)], 0.2); 2]);
        assert_eq!(seen(&passing).subject, Subject::Screen);
    }

    #[test]
    fn people_across_the_width_are_a_group() {
        let grid = mask(&[(0.05, 0.3), (0.38, 0.62), (0.7, 0.95)], 0.3);
        assert_eq!(seen(&vec![grid; 5]).subject, Subject::Group);
        let pair = mask(&[(0.1, 0.35), (0.6, 0.85)], 0.3);
        assert_eq!(seen(&vec![pair; 5]).subject, Subject::Group);
        assert_eq!(bodies(&[0.0, 9.0, 9.0, 9.0, 0.0, 0.0, 0.0]), 1);
    }

    fn person(focus_x: f64, focus_y: f64) -> Look {
        Look {
            subject: Subject::Person,
            focus_x,
            focus_y,
            covers: 0.3,
        }
    }

    #[test]
    fn a_person_in_a_wide_picture_is_cropped_to_where_they_are() {
        let wide = (1920, 1080);
        let tall = (1080, 1920);
        let Placement::Fill { offset_x, offset_y } = place(
            wide,
            tall,
            Choice::Auto,
            ReframeMode::Auto,
            Some(&person(0.5, 0.3)),
        ) else {
            panic!("a person is cropped in on");
        };
        assert_eq!((offset_x, offset_y), (0.0, 0.0));
        // Right of centre: the picture moves left, and no further than
        // keeps the frame covered.
        let Placement::Fill { offset_x, .. } = place(
            wide,
            tall,
            Choice::Auto,
            ReframeMode::Auto,
            Some(&person(0.7, 0.3)),
        ) else {
            panic!("a person is cropped in on");
        };
        let ratio = (16.0 / 9.0) / (9.0 / 16.0);
        assert!((offset_x + 0.2 * ratio).abs() < 1e-9, "{offset_x}");
        let Placement::Fill { offset_x, .. } = place(
            wide,
            tall,
            Choice::Auto,
            ReframeMode::Auto,
            Some(&person(0.99, 0.3)),
        ) else {
            panic!("a person is cropped in on");
        };
        assert!((offset_x + (ratio - 1.0) / 2.0).abs() < 1e-9, "{offset_x}");
    }

    #[test]
    fn a_screen_or_a_group_is_shown_whole_unless_told_otherwise() {
        let wide = (1920, 1080);
        let tall = (1080, 1920);
        let screen = Look {
            subject: Subject::Screen,
            ..person(0.5, 0.5)
        };
        let group = Look {
            subject: Subject::Group,
            ..person(0.5, 0.5)
        };
        for look in [Some(&screen), Some(&group), None] {
            assert_eq!(
                place(wide, tall, Choice::Auto, ReframeMode::Auto, look),
                Placement::Fit
            );
            assert!(matches!(
                place(wide, tall, Choice::Crop, ReframeMode::Auto, look),
                Placement::Fill { .. }
            ));
            assert!(matches!(
                place(wide, tall, Choice::Auto, ReframeMode::Crop, look),
                Placement::Fill { .. }
            ));
        }
        assert_eq!(
            place(
                wide,
                tall,
                Choice::Fit,
                ReframeMode::Crop,
                Some(&person(0.5, 0.3))
            ),
            Placement::Fit
        );
    }

    #[test]
    fn the_same_shape_just_fills_and_a_tall_picture_keeps_the_head() {
        assert_eq!(
            place(
                (3840, 2160),
                (1920, 1080),
                Choice::Auto,
                ReframeMode::Auto,
                None
            ),
            Placement::Fill {
                offset_x: 0.0,
                offset_y: 0.0
            }
        );
        // A phone video into a square: the head, high in the picture,
        // stays in view.
        let Placement::Fill { offset_x, offset_y } = place(
            (1080, 1920),
            (1080, 1080),
            Choice::Auto,
            ReframeMode::Auto,
            Some(&person(0.5, 0.25)),
        ) else {
            panic!("a person is cropped in on");
        };
        assert_eq!(offset_x, 0.0);
        // The picture is moved down, so its upper part shows.
        assert!(offset_y > 0.2, "{offset_y}");
        let reach = (16.0 / 9.0 - 1.0) / 2.0;
        assert!(offset_y <= reach + 1e-9);
        // A phone video into a wide frame keeps too little to crop.
        assert_eq!(
            place(
                (1080, 1920),
                (1920, 1080),
                Choice::Auto,
                ReframeMode::Auto,
                Some(&person(0.5, 0.25))
            ),
            Placement::Fit
        );
    }
}
