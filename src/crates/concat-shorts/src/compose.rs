// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! One clip in one frame shape, built as a Concat project.
//!
//! Everything here is a request to the engine, written as the JSON the API
//! documents and nothing an agent with the same tools could not send:
//!
//! 1. a project in the frame's size and the recording's rate, with the
//!    recording in its bin and on its main lane;
//! 2. the recording split at every edge of the clip's kept stretches - one
//!    batch, latest edge first, so every split is of the same clip - and
//!    everything that is not kept ripple-deleted, which closes the pauses
//!    and brings the clip to the start of the timeline;
//! 3. each piece framed ([`Placement`]): filled and moved to where the
//!    speaker is, or fitted over a blurred copy of itself;
//! 4. a sound layer over the whole clip with the cleanup chain - Concat's
//!    own Noise Reduction, Enhance Voice and Normalize Loudness - so the
//!    level is judged across the clip and not piece by piece;
//! 5. captions, as titles on a lane of their own;
//! 6. the logo, on the lane above them;
//! 7. the end card after the last piece: the brand's surface colour, its
//!    name or logo, a rule, what it does and where to find it.
//!
//! The project is saved and left where it is. It is an ordinary Concat
//! project: opening it in the window shows every one of those as a clip
//! that can be moved, restyled or deleted, and exported again.

use std::path::Path;

use concat_api::Request;
use serde_json::{Value, json};

use crate::captions::{self, Caption, Spoken};
use crate::framing::Placement;
use crate::plan::Recording;
use crate::settings::{self, Corner, Format, Settings};
use crate::sound::Tightened;
use crate::transcript::Word;
use crate::{Engine, Error};

/// How long the sound ramps at each cut: long enough that a cut in room
/// tone does not click, short enough that no syllable is softened.
const CUT_FADE: f64 = 0.012;
/// How far a piece's middle may sit from a kept stretch and still be
/// taken for part of it: the engine rounds a split to the frame.
const SLACK: f64 = 0.05;

/// Everything one build needs.
pub struct Brief<'a> {
    /// The settings in force.
    pub settings: &'a Settings,
    /// The frame shape.
    pub format: &'a Format,
    /// The recording.
    pub media: &'a Path,
    /// What the recording measures.
    pub recording: &'a Recording,
    /// The clip's kept stretches of the recording.
    pub tightened: &'a Tightened,
    /// The words said in the clip, in seconds of the recording.
    pub words: &'a [Word],
    /// How the picture is framed.
    pub placement: Placement,
    /// The folder the project is made in.
    pub location: &'a Path,
    /// The project's name: its folder's name in `location`.
    pub name: &'a str,
}

/// What was built.
#[derive(Clone, Debug, PartialEq)]
pub struct Built {
    /// The project's folder.
    pub project: String,
    /// How long the clip's own footage runs, in seconds.
    pub content: f64,
    /// How long the whole timeline runs, end card included.
    pub total: f64,
    /// Things left out and why, for a person to read: a logo file that is
    /// not there, a font that has no file.
    pub notes: Vec<String>,
}

/// Sends one request, written as the API's JSON.
pub fn call(engine: &mut dyn Engine, request: Value) -> Result<Value, Error> {
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    let request: Request = serde_json::from_value(request)
        .map_err(|error| Error::Failed(format!("the {method} request was malformed: {error}")))?;
    engine
        .call(request)
        .map_err(|error| Error::Failed(format!("{method}: {}", error.message)))
}

/// Applies one edit command to the project at `path`.
fn edit(engine: &mut dyn Engine, path: &str, command: Value) -> Result<Value, Error> {
    let op = command["op"].as_str().unwrap_or_default().to_owned();
    call(
        engine,
        json!({ "method": "edit.apply", "path": path, "command": command }),
    )
    .map_err(|error| Error::Failed(format!("{op}: {}", error.message())))
}

/// Applies several commands as one step; nothing when there are none.
fn batch(engine: &mut dyn Engine, path: &str, commands: Vec<Value>) -> Result<(), Error> {
    if commands.is_empty() {
        return Ok(());
    }
    edit(engine, path, json!({ "op": "batch", "commands": commands })).map(|_| ())
}

/// The id an edit minted.
fn created(view: &Value, what: &str) -> Result<String, Error> {
    view["createdId"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Failed(format!("the engine did not say which {what} it made")))
}

/// The active timeline of an editor view.
fn timeline(view: &Value) -> Result<&Value, Error> {
    let active = view["project"]["activeTimelineId"].as_str();
    view["project"]["timelines"]
        .as_array()
        .and_then(|timelines| {
            timelines
                .iter()
                .find(|timeline| timeline["id"].as_str() == active)
                .or(timelines.first())
        })
        .ok_or_else(|| Error::Failed("the project has no timeline".to_owned()))
}

/// The clips of `media` on the active timeline, as `(id, start, duration)`
/// in time order.
fn pieces(view: &Value, media: &str) -> Result<Vec<(String, f64, f64)>, Error> {
    let mut pieces: Vec<(String, f64, f64)> = timeline(view)?["clips"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|clip| clip["mediaId"].as_str() == Some(media))
        .filter_map(|clip| {
            Some((
                clip["id"].as_str()?.to_owned(),
                clip["start"].as_f64()?,
                clip["duration"].as_f64()?,
            ))
        })
        .collect();
    pieces.sort_by(|a, b| a.1.total_cmp(&b.1));
    Ok(pieces)
}

/// The frame rate a clip is rendered at, as a fraction: the recording's
/// own, brought under `most` by halving when it is double a rate that
/// fits (60 to 30, 59.94 to 29.97) and by taking `most` otherwise. The
/// broadcast rates keep their thousand-and-oneths.
pub fn rate(fps: f64, most: f64) -> (i64, i64) {
    let mut fps = if fps.is_finite() && fps >= 1.0 { fps } else { 30.0 };
    if fps > most + 0.5 {
        fps = if fps / 2.0 <= most + 0.5 && fps / 2.0 >= 20.0 {
            fps / 2.0
        } else {
            most
        };
    }
    for whole in [24.0, 30.0, 48.0, 60.0, 120.0] {
        let broadcast = whole * 1000.0 / 1001.0;
        if (fps - broadcast).abs() < 0.01 {
            return (whole as i64 * 1000, 1001);
        }
    }
    if (fps - fps.round()).abs() < 0.01 {
        return (fps.round() as i64, 1);
    }
    ((fps * 1000.0).round() as i64, 1000)
}

/// The sound cleanup chain the settings ask for, as a clip's `filters`.
fn cleanup(settings: &Settings) -> Vec<Value> {
    let audio = &settings.audio;
    let mut chain = Vec::new();
    if !audio.enabled {
        return chain;
    }
    if audio.denoise > 0.0 {
        chain.push(json!({ "id": "concat.denoise", "params": { "strength": audio.denoise } }));
    }
    if audio.enhance_voice > 0.0 {
        chain.push(
            json!({ "id": "concat.enhance-voice", "params": { "amount": audio.enhance_voice } }),
        );
    }
    if audio.loudness != 0.0 {
        chain.push(json!({ "id": "concat.loudness", "params": { "target": audio.loudness } }));
    }
    chain
}

/// A caption as a title's style, in the brand's type and colours.
fn caption_style(caption: &Caption, format: &Format, settings: &Settings) -> Value {
    let look = &settings.captions;
    let brand = &settings.brand;
    let size = format.caption_size;
    let text = brand.color_or(&look.text, "#1e1b18");
    let plate = brand.color(&look.plate);
    let outline = brand.color_or(&look.outline, "#f6fbfb");
    // What a held-back word is drawn in: the text colour let through at
    // the dim strength onto whatever is behind it.
    let behind = plate.as_deref().unwrap_or(&outline);
    let dimmed = match (settings::rgb(&text), settings::rgb(behind)) {
        (Some(over), Some(under)) => settings::blend(over, under, look.dim),
        _ => text.clone(),
    };
    let mut style = json!({
        "content": caption.text,
        "fontFamily": look.font,
        "fontSize": size,
        "fontWeight": look.weight,
        "color": text,
        "align": "center",
        "shadow": false,
        "background": plate.clone().unwrap_or_default(),
        "backgroundRadius": 0.0,
        "backgroundPaddingX": size * 0.6,
        "backgroundPaddingY": size * 0.42,
        // Without a plate the words wear an outline, to read over anything.
        "strokeWidth": if plate.is_some() { 0.0 } else { size * 0.14 },
        "strokeColor": outline,
    });
    if let Some((start, end)) = caption.dim {
        style["runs"] = json!([{ "start": start, "end": end, "color": dimmed }]);
    }
    style
}

/// Where the logo goes: its scale over the size it is fitted at, and its
/// centre as fractions of the frame from the frame's centre.
pub fn logo_place(
    logo: (u32, u32),
    frame: (u32, u32),
    width: f64,
    margin: f64,
    inset: f64,
    corner: Corner,
) -> (f64, f64, f64) {
    let (frame_w, frame_h) = (f64::from(frame.0.max(1)), f64::from(frame.1.max(1)));
    let aspect = f64::from(logo.0.max(1)) / f64::from(logo.1.max(1));
    // Scale one is the logo fitted inside the frame.
    let fitted_w = frame_w.min(frame_h * aspect);
    let wanted_w = width * frame_w;
    let scale = (wanted_w / fitted_w).clamp(0.05, 8.0);
    let shown_w = fitted_w * scale / frame_w;
    let shown_h = fitted_w * scale / aspect / frame_h;
    // The margin is the same distance on both axes: a fraction of the
    // frame's width, so the gap to the side and to the top look equal.
    let margin_y = margin * frame_w / frame_h + inset;
    let x = (0.5 - margin - shown_w / 2.0).max(0.0);
    let y = (0.5 - margin_y - shown_h / 2.0).max(0.0);
    match corner {
        Corner::TopLeft => (scale, -x, -y),
        Corner::TopRight => (scale, x, -y),
        Corner::BottomLeft => (scale, -x, y),
        Corner::BottomRight => (scale, x, y),
    }
}

/// The size of a picture file, from the engine's probe.
fn picture_size(engine: &mut dyn Engine, file: &str) -> Result<(u32, u32), Error> {
    let probe = call(engine, json!({ "method": "media.probe", "path": file }))?;
    let side = |key: &str| {
        probe["video"][key]
            .as_u64()
            .filter(|side| *side > 0)
            .map(|side| side as u32)
    };
    match (side("width"), side("height")) {
        (Some(width), Some(height)) => Ok((width, height)),
        _ => Err(Error::Failed(format!("{file} has no picture"))),
    }
}

/// Imports `file` into the project's bin and returns its media id.
fn import(engine: &mut dyn Engine, path: &str, file: &str) -> Result<String, Error> {
    let view = call(
        engine,
        json!({ "method": "media.import", "path": path, "file": file }),
    )?;
    if let Some(id) = view["createdId"].as_str() {
        return Ok(id.to_owned());
    }
    // Already in the bin: found by its path.
    view["project"]["media"]
        .as_array()
        .and_then(|media| {
            media
                .iter()
                .find(|item| item["path"].as_str() == Some(file))
        })
        .and_then(|item| item["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| Error::Failed(format!("{file} did not arrive in the project's bin")))
}

/// A fresh lane on top of the timeline.
fn lane(engine: &mut dyn Engine, path: &str) -> Result<String, Error> {
    created(&edit(engine, path, json!({ "op": "addTrack" }))?, "lane")
}

/// Builds the project for one clip in one frame shape, and saves it.
pub fn build(engine: &mut dyn Engine, brief: &Brief<'_>) -> Result<Built, Error> {
    let Brief {
        settings,
        format,
        recording,
        tightened,
        ..
    } = brief;
    let mut notes: Vec<String> = Vec::new();
    if tightened.kept.is_empty() {
        return Err(Error::Invalid("the clip covers no time".to_owned()));
    }
    let media_file = brief.media.to_string_lossy().into_owned();
    std::fs::create_dir_all(brief.location).map_err(|error| {
        Error::Failed(format!("could not make {}: {error}", brief.location.display()))
    })?;

    // 1. The project, the recording in it and on its main lane.
    let (rate_num, rate_den) = rate(recording.fps, settings.export.max_fps);
    call(
        engine,
        json!({
            "method": "project.create",
            "location": brief.location.to_string_lossy(),
            "name": brief.name,
            "video": {
                "width": format.width,
                "height": format.height,
                "rateNum": rate_num,
                "rateDen": rate_den,
            },
        }),
    )?;
    let path = brief
        .location
        .join(brief.name)
        .to_string_lossy()
        .into_owned();
    // From here on the project is open in the engine: whatever happens,
    // it is closed again, so a failed build does not leave it held.
    let outcome = compose(engine, brief, &path, &media_file, &mut notes);
    match outcome {
        Ok((content, total)) => {
            call(engine, json!({ "method": "project.save", "path": path }))?;
            Ok(Built {
                project: path,
                content,
                total,
                notes,
            })
        }
        Err(error) => {
            let _ = call(
                engine,
                json!({ "method": "project.close", "path": path, "save": false }),
            );
            Err(error)
        }
    }
}

fn compose(
    engine: &mut dyn Engine,
    brief: &Brief<'_>,
    path: &str,
    media_file: &str,
    notes: &mut Vec<String>,
) -> Result<(f64, f64), Error> {
    let Brief {
        settings,
        format,
        recording,
        tightened,
        ..
    } = brief;
    let frame = (format.width, format.height);
    let media = import(engine, path, media_file)?;
    let view = edit(
        engine,
        path,
        json!({ "op": "addClipAtFirstFree", "mediaId": media, "start": 0.0 }),
    )?;
    let whole = created(&view, "clip")?;
    let whole_length = pieces(&view, &media)?
        .first()
        .map_or(recording.duration, |piece| piece.2);

    // 2. Split at every edge, latest first: the head keeps the clip's id,
    // so each split is of the clip just split.
    let mut edges: Vec<f64> = tightened
        .boundaries()
        .into_iter()
        .filter(|edge| *edge > 1.0 / 30.0 && *edge < whole_length - 1.0 / 30.0)
        .collect();
    edges.sort_by(|a, b| b.total_cmp(a));
    edges.dedup_by(|a, b| (*a - *b).abs() < 1.0 / 60.0);
    batch(
        engine,
        path,
        edges
            .iter()
            .map(|edge| json!({ "op": "splitClips", "clipIds": [whole], "time": edge }))
            .collect(),
    )?;
    let view = call(engine, json!({ "method": "project.get", "path": path }))?;
    let is_kept = |start: f64, duration: f64| {
        let middle = start + duration / 2.0;
        tightened
            .kept
            .iter()
            .any(|(from, to)| middle > from - SLACK && middle < to + SLACK)
    };
    let doomed: Vec<String> = pieces(&view, &media)?
        .into_iter()
        .filter(|(_, start, duration)| !is_kept(*start, *duration))
        .map(|(id, _, _)| id)
        .collect();
    let view = if doomed.is_empty() {
        view
    } else {
        edit(
            engine,
            path,
            json!({ "op": "removeClips", "clipIds": doomed, "ripple": true }),
        )?
    };
    let kept = pieces(&view, &media)?;
    let Some(last) = kept.last() else {
        return Err(Error::Failed(
            "nothing of the recording was left on the timeline".to_owned(),
        ));
    };
    let content = last.1 + last.2;
    let timeline_id = timeline(&view)?["id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    // 3. The picture, framed, and the sound ramped at each cut.
    let mut commands: Vec<Value> = Vec::new();
    for (id, _, _) in &kept {
        match brief.placement {
            Placement::Fill { offset_x, offset_y } => {
                commands.push(json!({ "op": "frameClip", "clipId": id, "framing": "fill" }));
                if offset_x != 0.0 || offset_y != 0.0 {
                    commands.push(json!({
                        "op": "setClipTransform",
                        "clipId": id,
                        "offsetX": offset_x,
                        "offsetY": offset_y,
                    }));
                }
            }
            Placement::Fit => {
                commands.push(json!({ "op": "frameClip", "clipId": id, "framing": "fit" }));
            }
        }
        if kept.len() > 1 {
            commands.push(json!({
                "op": "updateClip",
                "clipId": id,
                "patch": { "fadeIn": CUT_FADE, "fadeOut": CUT_FADE },
            }));
        }
    }
    if brief.placement == Placement::Fit {
        commands.push(json!({
            "op": "setTimelineBackground",
            "timelineId": timeline_id,
            "background": { "kind": "blur", "amount": settings.reframe.blur },
        }));
    }
    batch(engine, path, commands)?;

    // 4. The sound, cleaned across the whole clip.
    let chain = cleanup(settings);
    if let Some(first) = chain.first() {
        let layer = created(
            &edit(
                engine,
                path,
                json!({
                    "op": "addLayerClip",
                    "trackId": null,
                    "start": 0.0,
                    "duration": content,
                    "effectId": first["id"],
                    "name": "Voice cleanup",
                    "sound": true,
                }),
            )?,
            "sound layer",
        )?;
        edit(
            engine,
            path,
            json!({ "op": "updateClip", "clipId": layer, "patch": { "filters": chain } }),
        )?;
    }

    // The brand's faces, for every title that follows.
    let mut fonts: Vec<Value> = Vec::new();
    for font in &settings.brand.fonts {
        if font.file.trim().is_empty() {
            continue;
        }
        let file = settings::expand_home(&font.file)?;
        if file.is_file() {
            fonts.push(json!({
                "op": "addFont",
                "family": font.family,
                "path": file.to_string_lossy(),
            }));
        } else {
            notes.push(format!(
                "the font file for {} is not at {}; titles in it are drawn in the system's {} if it is installed, or Concat's own face",
                font.family,
                file.display(),
                font.family
            ));
        }
    }
    batch(engine, path, fonts)?;

    // 5. Captions.
    if settings.captions.enabled && !brief.words.is_empty() {
        let spoken: Vec<Spoken> = brief
            .words
            .iter()
            .map(|word| Spoken {
                start: tightened.place(word.start),
                end: tightened.place(word.end),
                text: word.text.clone(),
            })
            .collect();
        let laid = captions::lay_out(&spoken, format.caption_chars, content, &settings.captions);
        if !laid.is_empty() {
            let track = lane(engine, path)?;
            batch(
                engine,
                path,
                laid.iter()
                    .map(|caption| {
                        json!({
                            "op": "addTextClip",
                            "trackId": track,
                            "start": caption.start,
                            "duration": caption.end - caption.start,
                            "style": caption_style(caption, format, settings),
                            "offsetY": format.caption_y,
                        })
                    })
                    .collect(),
            )?;
        }
    }

    // 6. The logo.
    let logo = &settings.brand.logo;
    let mut logo_media: Option<(String, (u32, u32))> = None;
    if !logo.file.trim().is_empty() {
        let file = settings::expand_home(&logo.file)?;
        if file.is_file() {
            let file = file.to_string_lossy().into_owned();
            let size = picture_size(engine, &file)?;
            let id = import(engine, path, &file)?;
            let track = lane(engine, path)?;
            let clip = still(engine, path, &id, &track, 0.0, content)?;
            let (scale, x, y) = logo_place(
                size,
                frame,
                logo.width,
                logo.margin,
                format.safe_inset,
                logo.position,
            );
            batch(
                engine,
                path,
                vec![
                    json!({
                        "op": "setClipTransform",
                        "clipId": clip,
                        "scale": scale,
                        "offsetX": x,
                        "offsetY": y,
                    }),
                    json!({
                        "op": "updateClip",
                        "clipId": clip,
                        "patch": { "opacity": logo.opacity },
                    }),
                ],
            )?;
            logo_media = Some((id, size));
        } else {
            notes.push(format!(
                "the logo file is not at {}; the clip was made without a logo",
                file.display()
            ));
        }
    }

    // 7. The end card.
    let card = &settings.brand.end_card;
    let mut total = content;
    if card.enabled {
        end_card(engine, brief, path, content, logo_media.as_ref())?;
        total += card.seconds;
    }
    Ok((content, total))
}

/// Places a still from the bin on `track` from `start` for `length`
/// seconds and returns the clip.
fn still(
    engine: &mut dyn Engine,
    path: &str,
    media: &str,
    track: &str,
    start: f64,
    length: f64,
) -> Result<String, Error> {
    let view = edit(
        engine,
        path,
        json!({ "op": "addClip", "mediaId": media, "trackId": track, "start": start }),
    )?;
    let clip = created(&view, "clip")?;
    let placed = timeline(&view)?["clips"]
        .as_array()
        .and_then(|clips| clips.iter().find(|item| item["id"].as_str() == Some(&clip)))
        .and_then(|item| item["duration"].as_f64())
        .unwrap_or(5.0);
    if (length - placed).abs() > 1.0 / 60.0 {
        edit(
            engine,
            path,
            json!({ "op": "trimClip", "clipId": clip, "edge": "end", "delta": length - placed }),
        )?;
    }
    Ok(clip)
}

/// The end card: from `at` for the settings' seconds.
fn end_card(
    engine: &mut dyn Engine,
    brief: &Brief<'_>,
    path: &str,
    at: f64,
    logo: Option<&(String, (u32, u32))>,
) -> Result<(), Error> {
    let Brief {
        settings, format, ..
    } = brief;
    let brand = &settings.brand;
    let card = &brand.end_card;
    let aspect = format.aspect();
    let seconds = card.seconds;
    let surface = brand.color_or(&card.background, "#2bd4d1");
    let ink = brand.color_or(&card.text, "#1e1b18");
    // Type is sized against the frame's height; in a tall frame that would
    // run off the side, so there it is sized against the width instead.
    let unit = (aspect * 1.2).min(1.0);
    // Everything hangs from one left edge.
    let margin = 0.08;
    let left = -0.5 + margin;

    // The surface, a little larger than the frame so no edge shows.
    let ground = lane(engine, path)?;
    edit(
        engine,
        path,
        json!({
            "op": "addShapeClip",
            "trackId": ground,
            "start": at,
            "duration": seconds,
            "name": "End card",
            "style": { "kind": "square", "width": aspect * 1.02, "height": 1.02, "fill": surface },
        }),
    )?;

    // Lower left, top to bottom: the name (or the logo), a fine rule, what
    // the brand does, where to find it.
    let rule_y = 0.2;
    let line = |text: &str, font: &str, weight: f64, size: f64, tracking: f64, y: f64| {
        json!({
            "op": "addTextClip",
            "trackId": null,
            "above": true,
            "start": at,
            "duration": seconds,
            "offsetY": y,
            "style": {
                "content": text,
                "fontFamily": font,
                "fontSize": size,
                "fontWeight": weight,
                "color": ink,
                "align": "left",
                "shadow": false,
                "tracking": tracking,
            },
        })
    };
    let headline = if card.headline.trim().is_empty() {
        brand.name.trim()
    } else {
        card.headline.trim()
    };
    let mut titles: Vec<(Value, f64)> = Vec::new();
    if logo.is_none() && !headline.is_empty() {
        // As large as the column allows on one line: a letter of a
        // serif this size is about half as wide as its capitals are
        // tall times ten sevenths.
        let column = (1.0 - margin * 2.0) * aspect;
        let letters = headline.chars().count().max(1) as f64;
        let size = (0.085 * unit).min(column / letters / 0.56 * 0.7);
        titles.push((
            line(headline, &card.headline_font, card.headline_weight, size, 0.0, rule_y - size * 1.25),
            left,
        ));
    }
    let small = 0.019 * unit;
    let mut below = rule_y + small * 2.2;
    for text in [card.descriptor.trim(), card.url.trim()] {
        if text.is_empty() {
            continue;
        }
        titles.push((line(text, &card.font, 500.0, small, small * 0.2, below), left));
        below += small * 2.6;
    }
    // The rule, as wide as the text column.
    let rule_width = (1.0 - margin * 2.0) * aspect;
    let rule = created(
        &edit(
            engine,
            path,
            json!({
                "op": "addShapeClip",
                "trackId": null,
                "above": true,
                "start": at,
                "duration": seconds,
                "name": "End card rule",
                "style": {
                    "kind": "line",
                    "width": rule_width,
                    "stroke": ink,
                    "strokeWidth": 0.0016 * unit.max(0.6),
                },
            }),
        )?,
        "shape",
    )?;
    edit(
        engine,
        path,
        json!({ "op": "setClipTransform", "clipId": rule, "offsetX": 0.0, "offsetY": rule_y }),
    )?;
    // Each title is placed as it is made: its id is the one just minted.
    for (title, x) in titles {
        let id = created(&edit(engine, path, title)?, "title")?;
        edit(
            engine,
            path,
            json!({ "op": "setClipTransform", "clipId": id, "offsetX": x }),
        )?;
    }

    // The logo stands where the name would, when there is one.
    if let Some((media, size)) = logo {
        let track = lane(engine, path)?;
        let clip = still(engine, path, media, &track, at, seconds)?;
        let logo_aspect = f64::from(size.0.max(1)) / f64::from(size.1.max(1));
        // Half the frame wide, and no taller than a sixth of it.
        let mut width = 0.5_f64.min(1.0 - margin * 2.0);
        let tallest = 0.17;
        if width * aspect / logo_aspect > tallest {
            width = tallest * logo_aspect / aspect;
        }
        let (scale, _, _) = logo_place(*size, (format.width, format.height), width, 0.0, 0.0, Corner::TopLeft);
        let height = width * aspect / logo_aspect;
        edit(
            engine,
            path,
            json!({
                "op": "setClipTransform",
                "clipId": clip,
                "scale": scale,
                "offsetX": left + width / 2.0,
                "offsetY": rule_y - 0.035 * unit - height / 2.0,
            }),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::CaptionMode;

    #[test]
    fn the_rate_is_the_recordings_brought_under_the_most() {
        assert_eq!(rate(30.0, 30.0), (30, 1));
        assert_eq!(rate(29.97, 30.0), (30000, 1001));
        assert_eq!(rate(60.0, 30.0), (30, 1));
        assert_eq!(rate(59.94, 30.0), (30000, 1001));
        assert_eq!(rate(25.0, 30.0), (25, 1));
        assert_eq!(rate(23.976, 30.0), (24000, 1001));
        assert_eq!(rate(50.0, 30.0), (25, 1));
        assert_eq!(rate(120.0, 30.0), (30, 1));
        assert_eq!(rate(60.0, 60.0), (60, 1));
        assert_eq!(rate(0.0, 30.0), (30, 1));
        assert_eq!(rate(f64::NAN, 30.0), (30, 1));
        assert_eq!(rate(12.5, 30.0), (12500, 1000));
    }

    #[test]
    fn the_cleanup_chain_is_the_effects_that_are_turned_up() {
        let mut settings = Settings::default();
        let ids = |settings: &Settings| -> Vec<String> {
            cleanup(settings)
                .iter()
                .map(|filter| filter["id"].as_str().unwrap_or_default().to_owned())
                .collect()
        };
        assert_eq!(
            ids(&settings),
            ["concat.denoise", "concat.enhance-voice", "concat.loudness"]
        );
        settings.audio.denoise = 0.0;
        settings.audio.loudness = 0.0;
        assert_eq!(ids(&settings), ["concat.enhance-voice"]);
        settings.audio.enabled = false;
        assert!(cleanup(&settings).is_empty());
    }

    #[test]
    fn a_caption_wears_the_brand_and_dims_what_is_to_come() {
        let settings = Settings::default();
        let format = &settings.formats[0];
        let caption = Caption {
            start: 0.0,
            end: 1.0,
            text: "Start with why".to_owned(),
            dim: Some((5, 14)),
        };
        let style = caption_style(&caption, format, &settings);
        assert_eq!(style["fontFamily"], "Montserrat");
        assert_eq!(style["color"], "#1e1b18");
        assert_eq!(style["background"], "#2bd4d1");
        assert_eq!(style["strokeWidth"], 0.0);
        assert_eq!(style["runs"][0]["start"], 5);
        assert_eq!(style["runs"][0]["end"], 14);
        // Ink at the dim strength on turquoise: between the two.
        let dim = style["runs"][0]["color"].as_str().expect("a colour");
        assert!(dim != "#1e1b18" && dim != "#2bd4d1", "{dim}");

        // No plate: an outline instead, and no runs when all is lit.
        let mut bare = settings.clone();
        bare.captions.plate = String::new();
        bare.captions.mode = CaptionMode::Phrase;
        let lit = Caption { dim: None, ..caption };
        let style = caption_style(&lit, format, &bare);
        assert_eq!(style["background"], "");
        assert!(style["strokeWidth"].as_f64().expect("a width") > 0.0);
        assert!(style.get("runs").is_none());
    }

    #[test]
    fn the_logo_sits_in_its_corner_at_its_width() {
        // A 4:1 logo in a 1080x1920 frame, 30 % wide, 5 % margin, top right.
        let (scale, x, y) = logo_place((800, 200), (1080, 1920), 0.3, 0.05, 0.0, Corner::TopRight);
        // Fitted, it is the frame's width; so the scale is its share.
        assert!((scale - 0.3).abs() < 1e-9, "{scale}");
        assert!((x - (0.5 - 0.05 - 0.15)).abs() < 1e-9, "{x}");
        // 81 px tall of 1920; the margin is 54 px of 1920.
        let expected = -(0.5 - 54.0 / 1920.0 - 81.0 / 1920.0 / 2.0);
        assert!((y - expected).abs() < 1e-9, "{y}");
        // The other corners mirror it, and an inset pushes it in.
        let (_, left, bottom) = logo_place((800, 200), (1080, 1920), 0.3, 0.05, 0.06, Corner::BottomLeft);
        assert!((left + x).abs() < 1e-9);
        assert!((bottom - (-y - 0.06)).abs() < 1e-9, "{bottom}");
        // A square logo in a wide frame is fitted to the height.
        let (scale, _, _) = logo_place((500, 500), (1920, 1080), 0.1, 0.05, 0.0, Corner::TopLeft);
        assert!((scale - 192.0 / 1080.0).abs() < 1e-9, "{scale}");
    }
}
