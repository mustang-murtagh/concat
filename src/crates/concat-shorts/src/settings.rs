// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! The settings every short is made with, and the file they live in.
//!
//! One JSON file in Concat's config folder, `shorts.json`: the brand kit
//! (colours, fonts, the logo, the end card), how captions look, how long a
//! clip runs, how hard pauses are tightened, how the sound is cleaned, the
//! shapes of frame each clip is rendered in, and how the export is
//! encoded. A file that is not there is the defaults; a field that is not
//! there is its default; a field nobody knows is refused, so a misspelt
//! key is an error to correct and not a setting quietly ignored.
//!
//! The defaults are a brand kit in their own right: the AI with Sally
//! identity - turquoise as the lead surface with ink text on it, Montserrat
//! for everything set in type, the wordmark's own serif for the end card -
//! so the first clip rendered is already on brand, and the logo file is the
//! one thing left to point at. Colours are named once in the palette and
//! referred to by name everywhere else (`"plate": "turquoise"`), so a
//! change of shade is a change in one place; a `#rrggbb` is taken as it is.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use concat_host::AppDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Error;

/// The file the settings are kept in, inside Concat's config folder.
pub const FILE_NAME: &str = "shorts.json";

/// Everything a short is made with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Settings {
    /// The folder jobs are kept in: downloads, transcripts, the Concat
    /// projects behind each clip and the finished files. Empty is the
    /// default, `Concat Shorts` in the user's videos folder.
    pub workspace: String,
    /// The brand kit.
    pub brand: Brand,
    /// How captions look and move.
    pub captions: Captions,
    /// How long a clip runs and how many are suggested.
    pub clips: ClipRules,
    /// How pauses are tightened.
    pub tighten: Tighten,
    /// How the sound is cleaned.
    pub audio: AudioCleanup,
    /// The shapes of frame each clip is rendered in.
    pub formats: Vec<Format>,
    /// How a wide picture is put in a tall or square frame.
    pub reframe: Reframe,
    /// How the files are encoded.
    pub export: ExportRules,
    /// How speech is turned into words.
    pub transcription: Transcription,
    /// How recordings are fetched from links.
    pub download: Download,
}

/// The brand kit: what makes a clip recognisably one maker's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Brand {
    /// The brand's name, as the end card's headline falls back to.
    pub name: String,
    /// The palette: a name for each colour and its `#rrggbb`.
    pub colors: BTreeMap<String, String>,
    /// The typefaces, each with the file it is read from when the system
    /// does not have it installed.
    pub fonts: Vec<BrandFont>,
    /// The logo laid over every clip.
    pub logo: Logo,
    /// The card each clip ends on.
    pub end_card: EndCard,
}

/// One typeface of the brand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct BrandFont {
    /// The family name titles ask for, e.g. "Montserrat".
    pub family: String,
    /// A `.ttf` or `.otf` to read it from. Empty when the family is
    /// installed on the system; a family found in neither place is drawn
    /// in Concat's bundled face.
    pub file: String,
}

/// Where the logo sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Corner {
    /// Top left.
    TopLeft,
    /// Top right.
    #[default]
    TopRight,
    /// Bottom left.
    BottomLeft,
    /// Bottom right.
    BottomRight,
}

/// The logo laid over every clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Logo {
    /// The image file, a PNG with a transparent background for
    /// preference. Empty for no logo.
    pub file: String,
    /// The corner it sits in.
    pub position: Corner,
    /// Its width as a fraction of the frame's width.
    pub width: f64,
    /// The gap between it and the frame's edges, as a fraction of the
    /// frame's width.
    pub margin: f64,
    /// How solid it is, 0 to 1.
    pub opacity: f64,
}

/// The card a clip ends on: the brand's surface colour with its name, a
/// fine rule, what it does and where to find it, set at the lower left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct EndCard {
    /// Whether clips end on one.
    pub enabled: bool,
    /// How long it holds, in seconds.
    pub seconds: f64,
    /// The surface: a palette name or `#rrggbb`.
    pub background: String,
    /// The colour of everything written on it.
    pub text: String,
    /// The large line. Empty is the brand's name. Not shown when the logo
    /// file is set: the logo stands there instead.
    pub headline: String,
    /// The headline's family.
    pub headline_font: String,
    /// The headline's weight, 100 to 900.
    pub headline_weight: f64,
    /// The small capitals under the rule: what the brand does.
    pub descriptor: String,
    /// The last line: where to find it.
    pub url: String,
    /// The family of the descriptor and the address.
    pub font: String,
}

/// How a caption follows the words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum CaptionMode {
    /// A phrase at a time, each word coming to full strength as it is
    /// said and the ones still to come held back.
    #[default]
    Highlight,
    /// A phrase at a time, all of it at once.
    Phrase,
    /// One word at a time.
    Word,
}

/// How captions look and move.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Captions {
    /// Whether clips are captioned.
    pub enabled: bool,
    /// How a caption follows the words.
    pub mode: CaptionMode,
    /// The family.
    pub font: String,
    /// The weight, 100 to 900.
    pub weight: f64,
    /// The words' colour: a palette name or `#rrggbb`.
    pub text: String,
    /// The plate behind the words; empty for none, and then the words
    /// wear an outline in `outline` instead.
    pub plate: String,
    /// The outline's colour when there is no plate.
    pub outline: String,
    /// How strongly the words still to come show in `highlight` mode, 0
    /// (hidden) to 1 (as the rest).
    pub dim: f64,
    /// Whether commas and full stops are kept. Question and exclamation
    /// marks always are.
    pub punctuation: bool,
    /// The most words in one phrase.
    pub max_words: usize,
    /// Every caption in capitals.
    pub uppercase: bool,
}

/// How long a clip runs and how many are suggested.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct ClipRules {
    /// The shortest a suggested clip runs, in seconds, after tightening.
    pub min_seconds: f64,
    /// The longest.
    pub max_seconds: f64,
    /// The most clips suggested from one recording.
    pub max_clips: usize,
}

/// How pauses are tightened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Tighten {
    /// Whether silences and dead air are cut out of clips.
    pub enabled: bool,
    /// A pause is cut when it runs at least this long, in seconds.
    pub min_pause: f64,
    /// How much of a cut pause is left either side of it, in seconds: the
    /// breath before a word and the tail after one.
    pub keep: f64,
}

/// How the sound is cleaned. Each strength is that Concat effect's own
/// dial; zero leaves the effect out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AudioCleanup {
    /// Whether the sound is cleaned at all.
    pub enabled: bool,
    /// Noise Reduction's strength, 0 to 100.
    pub denoise: f64,
    /// Enhance Voice's amount, 0 to 100.
    pub enhance_voice: f64,
    /// The loudness every clip is brought to, in LUFS, from -30 to -12;
    /// -14 is what the social platforms play at. Zero leaves it alone.
    pub loudness: f64,
}

/// One shape of frame a clip is rendered in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Format {
    /// The short name it is asked for by and files are named with.
    pub id: String,
    /// What a person is shown.
    pub label: String,
    /// The frame's width in pixels.
    pub width: u32,
    /// The frame's height in pixels.
    pub height: u32,
    /// Whether clips are rendered in it unless a render names its own.
    pub enabled: bool,
    /// Where captions sit, as a fraction of the frame's height from the
    /// centre, positive down.
    pub caption_y: f64,
    /// The captions' size, as a fraction of the frame's height.
    pub caption_size: f64,
    /// About how many characters fit on a caption's line.
    pub caption_chars: usize,
    /// How far the logo is kept from the top or bottom edge beyond its
    /// margin, as a fraction of the frame's height: room for the buttons
    /// an app draws over a tall video.
    pub safe_inset: f64,
}

/// How a picture is put in a frame of another shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ReframeMode {
    /// By what is in the picture: a person talking is cropped in on, a
    /// screen recording or a grid of faces is shown whole over a blurred
    /// copy of itself.
    #[default]
    Auto,
    /// Always cropped to fill the frame.
    Crop,
    /// Always whole, over a blurred copy.
    Fit,
}

/// How a wide picture is put in a tall or square frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Reframe {
    /// The rule.
    pub mode: ReframeMode,
    /// How soft the blurred copy behind a whole picture is, 0 to 1.
    pub blur: f64,
}

/// How the files are encoded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct ExportRules {
    /// "h264", "hevc" or "av1".
    pub codec: String,
    /// Quality: lower is better and bigger. 0 to 51.
    pub crf: u8,
    /// The encoder's speed: "ultrafast" to "veryslow".
    pub preset: String,
    /// The most frames a second a clip is rendered at; a recording at a
    /// higher rate is brought down to it.
    pub max_fps: f64,
}

/// How speech is turned into words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Transcription {
    /// The Whisper model: "tiny.en", "base.en" or "small.en" (or their
    /// multilingual twins without ".en"). Downloaded the first time.
    pub model: String,
}

/// How recordings are fetched from links.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Download {
    /// The yt-dlp program, for links that are pages and not files:
    /// YouTube, Zoom, Vimeo and the like. A name on the PATH or a path.
    pub yt_dlp: String,
    /// The tallest picture fetched, in pixels.
    pub max_height: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            workspace: String::new(),
            brand: Brand::default(),
            captions: Captions::default(),
            clips: ClipRules::default(),
            tighten: Tighten::default(),
            audio: AudioCleanup::default(),
            formats: Format::standard(),
            reframe: Reframe::default(),
            export: ExportRules::default(),
            transcription: Transcription::default(),
            download: Download::default(),
        }
    }
}

impl Default for Brand {
    fn default() -> Self {
        let colors = [
            ("turquoise", "#2BD4D1"),
            ("linen", "#FDF9F0"),
            ("porcelain", "#F6FBFB"),
            ("ink", "#1E1B18"),
            ("slate", "#33475B"),
            ("clay", "#C96A4B"),
            ("coral", "#F2593F"),
            ("chartreuse", "#C6E82E"),
        ];
        Brand {
            name: "AI with Sally".to_owned(),
            colors: colors
                .into_iter()
                .map(|(name, hex)| (name.to_owned(), hex.to_owned()))
                .collect(),
            fonts: ["Montserrat", "EB Garamond"]
                .into_iter()
                .map(|family| BrandFont {
                    family: family.to_owned(),
                    file: String::new(),
                })
                .collect(),
            logo: Logo::default(),
            end_card: EndCard::default(),
        }
    }
}

impl Default for BrandFont {
    fn default() -> Self {
        BrandFont {
            family: String::new(),
            file: String::new(),
        }
    }
}

impl Default for Logo {
    fn default() -> Self {
        Logo {
            file: String::new(),
            position: Corner::TopRight,
            width: 0.3,
            margin: 0.05,
            opacity: 1.0,
        }
    }
}

impl Default for EndCard {
    fn default() -> Self {
        EndCard {
            enabled: true,
            seconds: 2.5,
            background: "turquoise".to_owned(),
            text: "ink".to_owned(),
            headline: String::new(),
            headline_font: "EB Garamond".to_owned(),
            headline_weight: 500.0,
            descriptor: "AI ADVISORY AND COACHING".to_owned(),
            url: "AIWITHSALLY.COM".to_owned(),
            font: "Montserrat".to_owned(),
        }
    }
}

impl Default for Captions {
    fn default() -> Self {
        Captions {
            enabled: true,
            mode: CaptionMode::Highlight,
            font: "Montserrat".to_owned(),
            weight: 800.0,
            text: "ink".to_owned(),
            plate: "turquoise".to_owned(),
            outline: "porcelain".to_owned(),
            dim: 0.45,
            punctuation: false,
            max_words: 5,
            uppercase: false,
        }
    }
}

impl Default for ClipRules {
    fn default() -> Self {
        ClipRules {
            min_seconds: 60.0,
            max_seconds: 90.0,
            max_clips: 6,
        }
    }
}

impl Default for Tighten {
    fn default() -> Self {
        Tighten {
            enabled: true,
            min_pause: 0.7,
            keep: 0.18,
        }
    }
}

impl Default for AudioCleanup {
    fn default() -> Self {
        AudioCleanup {
            enabled: true,
            denoise: 30.0,
            enhance_voice: 50.0,
            loudness: -14.0,
        }
    }
}

impl Default for Format {
    fn default() -> Self {
        Format {
            id: String::new(),
            label: String::new(),
            width: 1920,
            height: 1080,
            enabled: true,
            caption_y: 0.34,
            caption_size: 0.04,
            caption_chars: 32,
            safe_inset: 0.0,
        }
    }
}

impl Format {
    /// The three shapes a clip is made in out of the box: tall for Reels,
    /// Shorts, TikTok and LinkedIn's vertical feed, wide for YouTube and
    /// the web, square for a feed post.
    pub fn standard() -> Vec<Format> {
        vec![
            Format {
                id: "vertical".to_owned(),
                label: "Vertical 9:16".to_owned(),
                width: 1080,
                height: 1920,
                enabled: true,
                caption_y: 0.24,
                caption_size: 0.027,
                caption_chars: 18,
                safe_inset: 0.06,
            },
            Format {
                id: "landscape".to_owned(),
                label: "Landscape 16:9".to_owned(),
                width: 1920,
                height: 1080,
                enabled: true,
                caption_y: 0.34,
                caption_size: 0.042,
                caption_chars: 32,
                safe_inset: 0.0,
            },
            Format {
                id: "square".to_owned(),
                label: "Square 1:1".to_owned(),
                width: 1080,
                height: 1080,
                enabled: true,
                caption_y: 0.37,
                caption_size: 0.04,
                caption_chars: 22,
                safe_inset: 0.0,
            },
        ]
    }

    /// Width over height.
    pub fn aspect(&self) -> f64 {
        f64::from(self.width) / f64::from(self.height.max(1))
    }
}

impl Default for Reframe {
    fn default() -> Self {
        Reframe {
            mode: ReframeMode::Auto,
            blur: 0.6,
        }
    }
}

impl Default for ExportRules {
    fn default() -> Self {
        ExportRules {
            codec: "h264".to_owned(),
            crf: 20,
            preset: "medium".to_owned(),
            max_fps: 30.0,
        }
    }
}

impl Default for Transcription {
    fn default() -> Self {
        Transcription {
            model: "base.en".to_owned(),
        }
    }
}

impl Default for Download {
    fn default() -> Self {
        Download {
            yt_dlp: "yt-dlp".to_owned(),
            max_height: 1080,
        }
    }
}

/// `#rrggbb` as three channels; `None` for anything else.
pub fn rgb(hex: &str) -> Option<[u8; 3]> {
    let digits = hex.trim().strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// `over` laid on `under` at `strength` (0 to 1), as one flat colour: what
/// a translucent ink looks like on a plate, for a renderer whose text
/// colours are opaque.
pub fn blend(over: [u8; 3], under: [u8; 3], strength: f64) -> String {
    let strength = strength.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| {
        (f64::from(a) * strength + f64::from(b) * (1.0 - strength)).round() as u8
    };
    format!(
        "#{:02x}{:02x}{:02x}",
        mix(over[0], under[0]),
        mix(over[1], under[1]),
        mix(over[2], under[2])
    )
}

impl Brand {
    /// The `#rrggbb` a colour setting means: the palette's colour of that
    /// name, or the setting itself when it is already a hex colour. `None`
    /// when it is neither.
    pub fn color(&self, setting: &str) -> Option<String> {
        let setting = setting.trim();
        if let Some([r, g, b]) = rgb(setting) {
            return Some(format!("#{r:02x}{g:02x}{b:02x}"));
        }
        let named = self
            .colors
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(setting))?;
        let [r, g, b] = rgb(named.1)?;
        Some(format!("#{r:02x}{g:02x}{b:02x}"))
    }

    /// [`Brand::color`], or `fallback` for a setting that names nothing.
    pub fn color_or(&self, setting: &str, fallback: &str) -> String {
        self.color(setting).unwrap_or_else(|| fallback.to_owned())
    }
}

impl Settings {
    /// Where the settings file is.
    pub fn path(dirs: &AppDirs) -> PathBuf {
        dirs.config.join(FILE_NAME)
    }

    /// The settings on disk; the defaults when no file has been written.
    pub fn load(dirs: &AppDirs) -> Result<Settings, Error> {
        let path = Settings::path(dirs);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(error) => {
                return Err(Error::Failed(format!(
                    "could not read {}: {error}",
                    path.display()
                )));
            }
        };
        let settings: Settings = serde_json::from_str(&text).map_err(|error| {
            Error::Invalid(format!(
                "{} is not valid shorts settings: {error}",
                path.display()
            ))
        })?;
        settings.checked()
    }

    /// Writes the settings, whole or not at all.
    pub fn save(&self, dirs: &AppDirs) -> Result<(), Error> {
        let path = Settings::path(dirs);
        write_json(&path, self)
    }

    /// These settings with `patch` merged over them (RFC 7396: an object
    /// merges key by key, anything else replaces, `null` puts a field back
    /// to its default), checked.
    pub fn patched(&self, patch: &Value) -> Result<Settings, Error> {
        if !patch.is_object() {
            return Err(Error::Invalid(
                "the settings patch is an object of the fields to change".to_owned(),
            ));
        }
        let mut whole = serde_json::to_value(self)
            .map_err(|error| Error::Failed(format!("the settings did not encode: {error}")))?;
        merge(&mut whole, patch);
        let settings: Settings = serde_json::from_value(whole)
            .map_err(|error| Error::Invalid(format!("invalid settings: {error}")))?;
        settings.checked()
    }

    /// The settings if they make sense, or what is wrong with them.
    pub fn checked(self) -> Result<Settings, Error> {
        let mut wrong: Vec<String> = Vec::new();
        let unit = |value: f64| value.is_finite() && (0.0..=1.0).contains(&value);

        for (name, hex) in &self.brand.colors {
            if rgb(hex).is_none() {
                wrong.push(format!("brand.colors.{name} is {hex:?}, not a #rrggbb colour"));
            }
        }
        let colors = [
            ("brand.endCard.background", &self.brand.end_card.background, false),
            ("brand.endCard.text", &self.brand.end_card.text, false),
            ("captions.text", &self.captions.text, false),
            ("captions.plate", &self.captions.plate, true),
            ("captions.outline", &self.captions.outline, true),
        ];
        for (field, setting, optional) in colors {
            let empty = setting.trim().is_empty();
            if (empty && !optional) || (!empty && self.brand.color(setting).is_none()) {
                wrong.push(format!(
                    "{field} is {setting:?}: use a name from brand.colors or a #rrggbb colour"
                ));
            }
        }
        for font in &self.brand.fonts {
            if font.family.trim().is_empty() {
                wrong.push("a brand font has no family name".to_owned());
            }
        }
        let logo = &self.brand.logo;
        if !(logo.width.is_finite() && logo.width > 0.0 && logo.width <= 1.0) {
            wrong.push("brand.logo.width is a fraction of the frame's width, above 0 up to 1".to_owned());
        }
        if !(logo.margin.is_finite() && (0.0..=0.4).contains(&logo.margin)) {
            wrong.push("brand.logo.margin is a fraction of the frame's width, 0 to 0.4".to_owned());
        }
        if !unit(logo.opacity) {
            wrong.push("brand.logo.opacity is 0 to 1".to_owned());
        }
        let card = &self.brand.end_card;
        if !(card.seconds.is_finite() && (0.5..=15.0).contains(&card.seconds)) {
            wrong.push("brand.endCard.seconds is 0.5 to 15".to_owned());
        }
        for (field, weight) in [
            ("brand.endCard.headlineWeight", card.headline_weight),
            ("captions.weight", self.captions.weight),
        ] {
            if !(weight.is_finite() && (100.0..=900.0).contains(&weight)) {
                wrong.push(format!("{field} is a font weight, 100 to 900"));
            }
        }
        if !unit(self.captions.dim) {
            wrong.push("captions.dim is 0 to 1".to_owned());
        }
        if !(1..=12).contains(&self.captions.max_words) {
            wrong.push("captions.maxWords is 1 to 12".to_owned());
        }

        let clips = &self.clips;
        if !(clips.min_seconds.is_finite() && clips.min_seconds >= 3.0) {
            wrong.push("clips.minSeconds is at least 3".to_owned());
        }
        if !(clips.max_seconds.is_finite() && clips.max_seconds <= 600.0) {
            wrong.push("clips.maxSeconds is at most 600".to_owned());
        }
        if clips.max_seconds < clips.min_seconds {
            wrong.push("clips.maxSeconds is not below clips.minSeconds".to_owned());
        }
        if !(1..=50).contains(&clips.max_clips) {
            wrong.push("clips.maxClips is 1 to 50".to_owned());
        }

        let tighten = &self.tighten;
        if !(tighten.min_pause.is_finite() && (0.2..=10.0).contains(&tighten.min_pause)) {
            wrong.push("tighten.minPause is 0.2 to 10 seconds".to_owned());
        }
        if !(tighten.keep.is_finite() && tighten.keep >= 0.03 && tighten.keep * 2.0 < tighten.min_pause) {
            wrong.push(
                "tighten.keep is at least 0.03 seconds and less than half of tighten.minPause"
                    .to_owned(),
            );
        }

        let audio = &self.audio;
        for (field, value) in [
            ("audio.denoise", audio.denoise),
            ("audio.enhanceVoice", audio.enhance_voice),
        ] {
            if !(value.is_finite() && (0.0..=100.0).contains(&value)) {
                wrong.push(format!("{field} is 0 to 100"));
            }
        }
        if !(audio.loudness == 0.0 || (-30.0..=-12.0).contains(&audio.loudness)) {
            wrong.push("audio.loudness is -30 to -12 LUFS, or 0 to leave the level alone".to_owned());
        }

        if self.formats.is_empty() {
            wrong.push("formats lists at least one frame shape".to_owned());
        }
        let mut seen: Vec<&str> = Vec::new();
        for format in &self.formats {
            let id = format.id.as_str();
            let plain = !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !plain {
                wrong.push(format!(
                    "a format's id is {id:?}: use lowercase letters, digits and hyphens"
                ));
                continue;
            }
            if seen.contains(&id) {
                wrong.push(format!("two formats are called {id:?}"));
            }
            seen.push(id);
            let side = |pixels: u32| (128..=4096).contains(&pixels) && pixels % 2 == 0;
            if !(side(format.width) && side(format.height)) {
                wrong.push(format!(
                    "format {id}: width and height are even numbers of pixels, 128 to 4096"
                ));
            }
            if !(format.caption_y.is_finite() && (-0.48..=0.48).contains(&format.caption_y)) {
                wrong.push(format!("format {id}: captionY is -0.48 to 0.48"));
            }
            if !(format.caption_size.is_finite() && (0.01..=0.2).contains(&format.caption_size)) {
                wrong.push(format!("format {id}: captionSize is 0.01 to 0.2"));
            }
            if !(6..=80).contains(&format.caption_chars) {
                wrong.push(format!("format {id}: captionChars is 6 to 80"));
            }
            if !(format.safe_inset.is_finite() && (0.0..=0.3).contains(&format.safe_inset)) {
                wrong.push(format!("format {id}: safeInset is 0 to 0.3"));
            }
        }

        if !unit(self.reframe.blur) {
            wrong.push("reframe.blur is 0 to 1".to_owned());
        }
        if !["h264", "hevc", "av1"].contains(&self.export.codec.as_str()) {
            wrong.push("export.codec is \"h264\", \"hevc\" or \"av1\"".to_owned());
        }
        if self.export.crf > 51 {
            wrong.push("export.crf is 0 to 51".to_owned());
        }
        const PRESETS: [&str; 9] = [
            "ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower",
            "veryslow",
        ];
        if !PRESETS.contains(&self.export.preset.as_str()) {
            wrong.push(format!("export.preset is one of {}", PRESETS.join(", ")));
        }
        if !(self.export.max_fps.is_finite() && (10.0..=120.0).contains(&self.export.max_fps)) {
            wrong.push("export.maxFps is 10 to 120".to_owned());
        }
        const MODELS: [&str; 6] = ["tiny.en", "tiny", "base.en", "base", "small.en", "small"];
        if !MODELS.contains(&self.transcription.model.as_str()) {
            wrong.push(format!("transcription.model is one of {}", MODELS.join(", ")));
        }
        if self.download.yt_dlp.trim().is_empty() {
            wrong.push("download.ytDlp names the yt-dlp program".to_owned());
        }
        if !(144..=4320).contains(&self.download.max_height) {
            wrong.push("download.maxHeight is 144 to 4320 pixels".to_owned());
        }

        if wrong.is_empty() {
            Ok(self)
        } else {
            Err(Error::Invalid(wrong.join("; ")))
        }
    }

    /// The folder jobs are kept in.
    pub fn workspace_dir(&self) -> Result<PathBuf, Error> {
        let named = self.workspace.trim();
        if !named.is_empty() {
            let path = expand_home(named)?;
            if !path.is_absolute() {
                return Err(Error::Invalid(format!(
                    "workspace is {named:?}: use an absolute path, or one starting with ~"
                )));
            }
            return Ok(path);
        }
        let videos = if cfg!(target_os = "macos") {
            "Movies"
        } else {
            "Videos"
        };
        Ok(home()?.join(videos).join("Concat Shorts"))
    }

    /// The format called `id`.
    pub fn format(&self, id: &str) -> Option<&Format> {
        self.formats.iter().find(|format| format.id == id)
    }
}

/// The user's home folder.
pub fn home() -> Result<PathBuf, Error> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| Error::Failed("could not locate the home folder".to_owned()))
}

/// A path as a person writes one: a leading `~` is their home folder.
pub fn expand_home(path: &str) -> Result<PathBuf, Error> {
    let path = path.trim();
    if path == "~" {
        return home();
    }
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => Ok(home()?.join(rest)),
        None => Ok(PathBuf::from(path)),
    }
}

/// RFC 7396: `patch` merged into `target`.
fn merge(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    let Value::Object(fields) = target else {
        return;
    };
    for (key, value) in patch {
        if value.is_null() {
            fields.remove(key);
        } else {
            merge(fields.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// Writes `value` as pretty JSON to `path` through a file beside it, so a
/// crash halfway leaves the old file and never half of a new one.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let fail = |what: &str, error: &dyn std::fmt::Display| {
        Error::Failed(format!("could not {what} {}: {error}", path.display()))
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| fail("make the folder for", &error))?;
    }
    let mut text = serde_json::to_string_pretty(value).map_err(|error| fail("encode", &error))?;
    text.push('\n');
    let partial = path.with_extension(format!("part-{}", std::process::id()));
    std::fs::write(&partial, text).map_err(|error| fail("write", &error))?;
    std::fs::rename(&partial, path).map_err(|error| {
        let _ = std::fs::remove_file(&partial);
        fail("write", &error)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_defaults_are_valid_and_survive_a_round_trip() {
        let settings = Settings::default().checked().expect("the defaults are valid");
        let text = serde_json::to_string(&settings).expect("encodes");
        let back: Settings = serde_json::from_str(&text).expect("decodes");
        assert_eq!(back, settings);
        assert_eq!(settings.formats.len(), 3);
        assert_eq!(settings.clips.min_seconds, 60.0);
        assert_eq!(settings.clips.max_seconds, 90.0);
    }

    #[test]
    fn a_colour_is_a_palette_name_or_a_hex() {
        let brand = Brand::default();
        assert_eq!(brand.color("turquoise").as_deref(), Some("#2bd4d1"));
        assert_eq!(brand.color("Ink").as_deref(), Some("#1e1b18"));
        assert_eq!(brand.color("#ABCDEF").as_deref(), Some("#abcdef"));
        assert_eq!(brand.color("mauve"), None);
        assert_eq!(brand.color("#12"), None);
    }

    #[test]
    fn a_dimmed_ink_on_a_plate_is_one_flat_colour() {
        let ink = rgb("#1E1B18").expect("ink");
        let turquoise = rgb("#2BD4D1").expect("turquoise");
        assert_eq!(blend(ink, turquoise, 1.0), "#1e1b18");
        assert_eq!(blend(ink, turquoise, 0.0), "#2bd4d1");
        assert_eq!(blend(ink, turquoise, 0.62), "#23615e");
    }

    #[test]
    fn a_patch_changes_only_what_it_names() {
        let settings = Settings::default();
        let patched = settings
            .patched(&json!({
                "brand": { "logo": { "file": "/logos/mark.png" }, "colors": { "ink": "#000000" } },
                "clips": { "maxClips": 3 },
            }))
            .expect("a valid patch");
        assert_eq!(patched.brand.logo.file, "/logos/mark.png");
        assert_eq!(patched.brand.logo.width, settings.brand.logo.width);
        assert_eq!(patched.brand.colors["ink"], "#000000");
        assert_eq!(patched.brand.colors["turquoise"], "#2BD4D1");
        assert_eq!(patched.clips.max_clips, 3);
        assert_eq!(patched.clips.min_seconds, 60.0);
        // null puts a field back to its default.
        let back = patched
            .patched(&json!({ "clips": { "maxClips": null } }))
            .expect("a valid patch");
        assert_eq!(back.clips.max_clips, ClipRules::default().max_clips);
    }

    #[test]
    fn a_misspelt_or_impossible_setting_is_refused_with_its_name() {
        let settings = Settings::default();
        let unknown = settings.patched(&json!({ "clips": { "maxClip": 3 } }));
        assert!(matches!(unknown, Err(Error::Invalid(why)) if why.contains("maxClip")));
        let colour = settings.patched(&json!({ "captions": { "plate": "mauve" } }));
        assert!(matches!(colour, Err(Error::Invalid(why)) if why.contains("captions.plate")));
        let order = settings.patched(&json!({ "clips": { "minSeconds": 120 } }));
        assert!(matches!(order, Err(Error::Invalid(why)) if why.contains("maxSeconds")));
        let odd = settings.patched(&json!({ "formats": [{ "id": "tall", "width": 1081, "height": 1920 }] }));
        assert!(matches!(odd, Err(Error::Invalid(why)) if why.contains("even")));
    }

    #[test]
    fn settings_are_read_back_from_their_file() {
        let folder = tempfile::tempdir().expect("a scratch folder");
        let dirs = AppDirs::under(folder.path());
        assert_eq!(Settings::load(&dirs).expect("defaults"), Settings::default());
        let mut settings = Settings::default();
        settings.brand.logo.file = "/logos/mark.png".to_owned();
        settings.save(&dirs).expect("saves");
        assert_eq!(Settings::load(&dirs).expect("loads"), settings);
        std::fs::write(Settings::path(&dirs), "{ not json").expect("writes");
        assert!(matches!(Settings::load(&dirs), Err(Error::Invalid(_))));
    }

    #[test]
    fn the_workspace_is_the_named_folder_or_one_in_the_videos_folder() {
        let mut settings = Settings::default();
        settings.workspace = "/data/shorts".to_owned();
        assert_eq!(settings.workspace_dir().expect("a path"), PathBuf::from("/data/shorts"));
        settings.workspace = "shorts".to_owned();
        assert!(matches!(settings.workspace_dir(), Err(Error::Invalid(_))));
    }
}
