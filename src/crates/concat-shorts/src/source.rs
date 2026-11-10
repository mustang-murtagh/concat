// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Concat contributors

//! Where a recording comes from: a file on this machine, or a link.
//!
//! A link is one of two things. Some are the file itself - an address
//! ending in `.mp4`, or a share link from Google Drive or Dropbox, which
//! is rewritten into the address its Download button uses - and are
//! streamed straight into the job's folder. The rest are pages with a
//! player on them (YouTube, Zoom's cloud recordings, Vimeo, Loom...), and
//! for those the work is yt-dlp's: it is run as a program, with the link
//! as an argument and never through a shell, and its progress lines are
//! read as it goes. yt-dlp is a separate install; a page link without it
//! is refused with a sentence that says so.
//!
//! Either way the file lands as `source.<ext>` beside the job, written to
//! a `.part` first, so a download that was cut short is never mistaken
//! for a recording.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use concat_host::net::{self, Trust};

use crate::Error;
use crate::settings::{self, Download};

/// File extensions taken for a recording: what a link's address or a
/// download's name ends in when it is the file itself.
const MEDIA: [&str; 14] = [
    "mp4", "mov", "m4v", "mkv", "webm", "avi", "mpg", "mpeg", "ts", "m2ts", "wmv", "mp3", "m4a",
    "wav",
];

/// How long a connection may take to open, and a read to give something.
const CONNECT: Duration = Duration::from_secs(20);
const READ: Duration = Duration::from_secs(90);

/// What a job was pointed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A file already on this machine.
    File(PathBuf),
    /// An `http` or `https` address to fetch.
    Link(String),
}

/// Reads what a person gave as the recording: a link when it starts with
/// `http://` or `https://`, a path otherwise (a leading `~` is their home,
/// and a `file://` address is its path). A path must name a file that is
/// there.
pub fn read(source: &str) -> Result<Source, Error> {
    let source = source.trim();
    if source.is_empty() {
        return Err(Error::Invalid(
            "the source is a video file's path or a link to one".to_owned(),
        ));
    }
    let lower = source.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        if source.chars().any(char::is_whitespace) || source.len() < 12 {
            return Err(Error::Invalid(format!("{source:?} is not a link")));
        }
        return Ok(Source::Link(source.to_owned()));
    }
    if lower.contains("://") && !lower.starts_with("file://") {
        return Err(Error::Invalid(format!(
            "{source:?}: only http and https links can be fetched"
        )));
    }
    let named = source
        .strip_prefix("file://")
        .or_else(|| source.strip_prefix("FILE://"))
        .unwrap_or(source);
    let path = settings::expand_home(named)?;
    if !path.is_absolute() {
        return Err(Error::Invalid(format!(
            "{source:?}: give the file's full path, e.g. /Users/you/Movies/talk.mp4"
        )));
    }
    if !path.is_file() {
        return Err(Error::NotFound(format!(
            "there is no file at {}",
            path.display()
        )));
    }
    Ok(Source::File(path))
}

/// A short, file-safe name for a job from its source: the file's stem, or
/// the last telling part of a link.
pub fn stem(source: &Source) -> String {
    let raw = match source {
        Source::File(path) => path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default(),
        Source::Link(link) => {
            let rest = link.split("://").nth(1).unwrap_or(link);
            let path = rest.split(['?', '#']).next().unwrap_or(rest);
            let mut parts = path.split('/').filter(|part| !part.is_empty());
            let host = parts.next().unwrap_or_default();
            let last = parts
                .filter(|part| part.len() >= 3 && !["view", "edit", "watch", "share"].contains(part))
                .next_back()
                .unwrap_or_default();
            let last = last.rsplit_once('.').map_or(last, |(name, _)| name);
            let host = host.trim_start_matches("www.");
            let site = host.split('.').next().unwrap_or(host);
            format!("{site} {last}")
        }
    };
    slug(&raw, 40)
}

/// `text` as lowercase words joined by hyphens, at most `limit` characters:
/// safe in a file name on every system.
pub fn slug(text: &str, limit: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if out.len() >= limit {
            break;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() { "clip".to_owned() } else { out }
}

/// The address a share link's file is downloaded from, when the link is
/// one this knows how to rewrite; `None` for any other link.
///
/// - Google Drive: `drive.google.com/file/d/<id>/view`, `open?id=<id>` and
///   `uc?id=<id>` all name a file by its id; the download address takes
///   that id, with the confirmation a large file's interstitial page asks
///   for already given.
/// - Dropbox: the same link with `dl=1`.
pub fn direct_address(link: &str) -> Option<String> {
    let (_, rest) = link.split_once("://")?;
    let (host, after) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let (path, query) = after.split_once('?').unwrap_or((after, ""));
    let query = query.split('#').next().unwrap_or_default();
    let field = |name: &str| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
    };
    let id_like = |id: &str| {
        id.len() >= 10
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    if host == "drive.google.com" || host == "docs.google.com" {
        let id = path
            .split('/')
            .skip_while(|part| *part != "d")
            .nth(1)
            .or_else(|| field("id"))?;
        if !id_like(id) {
            return None;
        }
        return Some(format!(
            "https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t"
        ));
    }
    if host == "www.dropbox.com" || host == "dropbox.com" {
        let kept: Vec<&str> = query
            .split('&')
            .filter(|pair| !pair.is_empty() && !pair.starts_with("dl="))
            .collect();
        let mut query = kept.join("&");
        if !query.is_empty() {
            query.push('&');
        }
        return Some(format!("https://www.dropbox.com/{path}?{query}dl=1"));
    }
    None
}

/// The media extension an address's path ends in, if it does.
fn media_extension(address: &str) -> Option<String> {
    let rest = address.split("://").nth(1).unwrap_or(address);
    let path = rest.split(['?', '#']).next().unwrap_or(rest);
    let name = path.rsplit('/').next().unwrap_or_default();
    let (_, extension) = name.rsplit_once('.')?;
    let extension = extension.to_ascii_lowercase();
    MEDIA.contains(&extension.as_str()).then_some(extension)
}

/// The extension a response's own headers give its file: the name in
/// `Content-Disposition`, or the type in `Content-Type`.
fn response_extension(disposition: Option<&str>, kind: Option<&str>) -> Option<String> {
    if let Some(disposition) = disposition {
        let named = disposition
            .split(';')
            .map(str::trim)
            .filter_map(|part| part.split_once('='))
            .filter(|(key, _)| key.trim().to_ascii_lowercase().starts_with("filename"))
            .map(|(_, value)| value.trim().trim_matches('"'))
            .filter_map(|name| name.rsplit_once('.'))
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .find(|extension| MEDIA.contains(&extension.as_str()));
        if named.is_some() {
            return named;
        }
    }
    let kind = kind?.split(';').next()?.trim().to_ascii_lowercase();
    let extension = match kind.as_str() {
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/x-msvideo" => "avi",
        "video/mpeg" => "mpg",
        "video/mp2t" => "ts",
        "video/x-m4v" => "m4v",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/wav" | "audio/x-wav" => "wav",
        _ => return None,
    };
    Some(extension.to_owned())
}

/// A recording already fetched into `folder`, from a run before.
pub fn fetched(folder: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.file_stem().is_some_and(|stem| stem == "source")
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| MEDIA.contains(&extension.to_ascii_lowercase().as_str()))
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Fetches `link` into `folder` as `source.<ext>` and returns the file.
/// `progress` is told how far along it is - a fraction when the size is
/// known - and what is happening; `cancel` stops it at the next chunk.
pub fn fetch(
    link: &str,
    folder: &Path,
    download: &Download,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Option<f64>, &str),
) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(folder).map_err(|error| {
        Error::Failed(format!("could not make {}: {error}", folder.display()))
    })?;
    if let Some(file) = fetched(folder) {
        return Ok(file);
    }
    let rewritten = direct_address(link);
    let address = rewritten.as_deref().unwrap_or(link);
    let looks_direct = rewritten.is_some() || media_extension(address).is_some();

    if looks_direct {
        match direct(address, folder, cancel, progress)? {
            Some(file) => return Ok(file),
            None => log::info!("shorts: {link} is a page, not a file; handing it to yt-dlp"),
        }
        return page(link, folder, download, cancel, progress);
    }
    match page(link, folder, download, cancel, progress) {
        Ok(file) => Ok(file),
        // No yt-dlp: the link may still be the file itself under an
        // address that does not say so.
        Err(Error::NotFound(missing)) => match direct(address, folder, cancel, progress)? {
            Some(file) => Ok(file),
            None => Err(Error::Failed(missing)),
        },
        Err(other) => Err(other),
    }
}

fn cancelled() -> Error {
    Error::Failed("the download was cancelled".to_owned())
}

/// Streams `address` into the folder when what answers is a recording.
/// `None` when it answers with a page instead.
fn direct(
    address: &str,
    folder: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Option<f64>, &str),
) -> Result<Option<PathBuf>, Error> {
    progress(None, "connecting");
    let response = net::get(address, CONNECT, READ, Trust::System, &|request| request)
        .map_err(|failure| Error::Failed(format!("could not fetch the link: {failure}")))?;
    let kind = response.header("Content-Type").map(str::to_owned);
    let disposition = response.header("Content-Disposition").map(str::to_owned);
    let lower = kind.as_deref().unwrap_or_default().to_ascii_lowercase();
    if lower.starts_with("text/") || lower.contains("html") || lower.contains("json") {
        return Ok(None);
    }
    let Some(extension) = response_extension(disposition.as_deref(), kind.as_deref())
        .or_else(|| media_extension(response.get_url()))
        .or_else(|| media_extension(address))
    else {
        return Ok(None);
    };
    let total = response
        .header("Content-Length")
        .and_then(|length| length.parse::<u64>().ok())
        .filter(|length| *length > 0);

    let destination = folder.join(format!("source.{extension}"));
    let partial = folder.join(format!("source.{extension}.part"));
    let fail = |error: std::io::Error| {
        Error::Failed(format!("the download stopped: {error}"))
    };
    let mut file = std::fs::File::create(&partial).map_err(fail)?;
    let mut reader = response.into_reader();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut received: u64 = 0;
    let mut reported = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Err(cancelled());
        }
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => {
                drop(file);
                let _ = std::fs::remove_file(&partial);
                return Err(fail(error));
            }
        };
        file.write_all(&buffer[..count]).map_err(fail)?;
        received += count as u64;
        // Once a megabyte: a progress line a megabyte is plenty.
        if received - reported >= 1024 * 1024 {
            reported = received;
            let megabytes = received as f64 / 1_048_576.0;
            match total {
                Some(total) => progress(
                    Some((received as f64 / total as f64).min(1.0)),
                    &format!(
                        "downloading, {megabytes:.0} of {:.0} MB",
                        total as f64 / 1_048_576.0
                    ),
                ),
                None => progress(None, &format!("downloading, {megabytes:.0} MB")),
            }
        }
    }
    file.flush().map_err(fail)?;
    drop(file);
    if total.is_some_and(|total| received < total) {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::Failed(format!(
            "the download ended early, {received} of {} bytes",
            total.unwrap_or_default()
        )));
    }
    if received == 0 {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::Failed("the link gave an empty file".to_owned()));
    }
    std::fs::rename(&partial, &destination).map_err(fail)?;
    progress(Some(1.0), "downloaded");
    Ok(Some(destination))
}

/// The percentage in one of yt-dlp's progress lines,
/// `[download]  42.3% of ~120.00MiB at 3.2MiB/s ETA 00:21`.
fn percent(line: &str) -> Option<f64> {
    let rest = line.trim().strip_prefix("[download]")?.trim_start();
    let (number, _) = rest.split_once('%')?;
    let percent: f64 = number.trim().parse().ok()?;
    (0.0..=100.0).contains(&percent).then_some(percent)
}

/// Fetches a page's video with yt-dlp. [`Error::NotFound`] when yt-dlp is
/// not installed, so a caller can tell that apart from a download that
/// failed.
fn page(
    link: &str,
    folder: &Path,
    download: &Download,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Option<f64>, &str),
) -> Result<PathBuf, Error> {
    let height = download.max_height;
    // An MP4 with AAC when the site has one, since every player and every
    // platform takes it; whatever is best under the height otherwise.
    let format = format!(
        "bv*[height<={height}][ext=mp4]+ba[ext=m4a]/b[height<={height}][ext=mp4]/bv*[height<={height}]+ba/b[height<={height}]/b"
    );
    let template = folder.join("source.%(ext)s");
    let program = settings::expand_home(&download.yt_dlp)?;
    let mut command = Command::new(&program);
    command
        .arg("--no-playlist")
        .arg("--no-warnings")
        .arg("--newline")
        .arg("--progress")
        .arg("--no-mtime")
        .args(["--format", &format])
        .args(["--merge-output-format", "mp4"])
        .arg("--output")
        .arg(&template)
        // After `--`, nothing is an option: a link cannot be read as one.
        .arg("--")
        .arg(link)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    progress(None, "starting yt-dlp");
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::NotFound(format!(
                "this link is a page, not a video file, and fetching it needs yt-dlp, which was not found as {:?}. Install it (macOS: `brew install yt-dlp`; Windows: `winget install yt-dlp`; Linux: `pipx install yt-dlp`) or set download.ytDlp in the shorts settings to where it is; or download the recording yourself and give its file path",
                download.yt_dlp
            )));
        }
        Err(error) => {
            return Err(Error::Failed(format!(
                "could not start yt-dlp ({}): {error}",
                program.display()
            )));
        }
    };

    // Both pipes are read on threads of their own, so neither fills and
    // stalls yt-dlp, and this one stays free to notice a cancel.
    let (send, lines) = mpsc::channel::<(bool, String)>();
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let send = send.clone();
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if send.send((false, line)).is_err() {
                    break;
                }
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let send = send.clone();
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if send.send((true, line)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(send);

    let mut complaints: Vec<String> = Vec::new();
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(cancelled());
        }
        match lines.recv_timeout(Duration::from_millis(200)) {
            Ok((from_stderr, line)) => {
                if let Some(percent) = percent(&line) {
                    progress(Some(percent / 100.0), &format!("downloading, {percent:.0}%"));
                } else if line.contains("[Merger]") {
                    progress(None, "joining picture and sound");
                } else if from_stderr && !line.trim().is_empty() {
                    complaints.push(line.trim().to_owned());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break child.wait().map_err(|error| {
                    Error::Failed(format!("yt-dlp could not be waited for: {error}"))
                })?;
            }
        }
    };
    for reader in readers {
        let _ = reader.join();
    }
    if !status.success() {
        let why = complaints
            .iter()
            .rev()
            .find(|line| line.starts_with("ERROR"))
            .or(complaints.last())
            .cloned()
            .unwrap_or_else(|| format!("it ended with {status}"));
        return Err(Error::Failed(format!("yt-dlp could not fetch the link: {why}")));
    }
    fetched(folder).ok_or_else(|| {
        Error::Failed("yt-dlp finished without leaving a video file".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_is_a_link_or_a_file_that_is_there() {
        assert_eq!(
            read(" https://example.com/talk.mp4 ").expect("a link"),
            Source::Link("https://example.com/talk.mp4".to_owned())
        );
        assert!(matches!(read(""), Err(Error::Invalid(_))));
        assert!(matches!(read("ftp://example.com/a.mp4"), Err(Error::Invalid(_))));
        assert!(matches!(read("talk.mp4"), Err(Error::Invalid(_))));
        let folder = tempfile::tempdir().expect("a scratch folder");
        let missing = folder.path().join("missing.mp4");
        assert!(matches!(
            read(&missing.to_string_lossy()),
            Err(Error::NotFound(_))
        ));
        let file = folder.path().join("talk.mp4");
        std::fs::write(&file, b"x").expect("writes");
        assert_eq!(
            read(&file.to_string_lossy()).expect("a file"),
            Source::File(file.clone())
        );
        assert_eq!(
            read(&format!("file://{}", file.display())).expect("a file"),
            Source::File(file)
        );
    }

    #[test]
    fn share_links_are_rewritten_to_their_download_address() {
        let drive = "https://drive.usercontent.google.com/download?id=1AbCdEfGhIjKlMnOp&export=download&confirm=t";
        for link in [
            "https://drive.google.com/file/d/1AbCdEfGhIjKlMnOp/view?usp=sharing",
            "https://drive.google.com/open?id=1AbCdEfGhIjKlMnOp",
            "https://drive.google.com/uc?export=download&id=1AbCdEfGhIjKlMnOp",
        ] {
            assert_eq!(direct_address(link).as_deref(), Some(drive), "{link}");
        }
        assert_eq!(
            direct_address("https://www.dropbox.com/scl/fi/abc/talk.mp4?rlkey=k1&dl=0").as_deref(),
            Some("https://www.dropbox.com/scl/fi/abc/talk.mp4?rlkey=k1&dl=1")
        );
        assert_eq!(direct_address("https://youtu.be/dQw4w9WgXcQ"), None);
        assert_eq!(direct_address("https://drive.google.com/file/d/../view"), None);
    }

    #[test]
    fn a_file_is_known_by_its_address_or_its_headers() {
        assert_eq!(media_extension("https://x.test/a/Talk.MP4?sig=1").as_deref(), Some("mp4"));
        assert_eq!(media_extension("https://x.test/watch?v=abc"), None);
        assert_eq!(
            response_extension(Some("attachment; filename=\"Team call.mov\""), Some("application/octet-stream")).as_deref(),
            Some("mov")
        );
        assert_eq!(response_extension(None, Some("video/mp4; codecs=avc1")).as_deref(), Some("mp4"));
        assert_eq!(response_extension(None, Some("application/octet-stream")), None);
    }

    #[test]
    fn a_job_is_named_after_its_source() {
        assert_eq!(stem(&Source::File(PathBuf::from("/v/Team Webinar (final).mp4"))), "team-webinar-final");
        assert_eq!(stem(&Source::Link("https://www.youtube.com/watch?v=abc".to_owned())), "youtube");
        assert_eq!(stem(&Source::Link("https://cdn.example.com/talks/keynote-2026.mp4".to_owned())), "cdn-keynote-2026");
        assert_eq!(slug("  ¡¡ ", 10), "clip");
        assert_eq!(slug("A very long title indeed", 10), "a-very-lon");
    }

    #[test]
    fn yt_dlp_progress_lines_are_read() {
        assert_eq!(percent("[download]  42.3% of ~120.00MiB at 3.2MiB/s ETA 00:21"), Some(42.3));
        assert_eq!(percent("[download] 100% of 5.00MiB in 00:01"), Some(100.0));
        assert_eq!(percent("[download] Destination: source.mp4"), None);
        assert_eq!(percent("[youtube] abc: Downloading webpage"), None);
    }

    #[test]
    fn a_recording_fetched_before_is_found_again() {
        let folder = tempfile::tempdir().expect("a scratch folder");
        assert_eq!(fetched(folder.path()), None);
        std::fs::write(folder.path().join("source.mp4.part"), b"x").expect("writes");
        std::fs::write(folder.path().join("source.json"), b"x").expect("writes");
        assert_eq!(fetched(folder.path()), None);
        std::fs::write(folder.path().join("source.mp4"), b"x").expect("writes");
        assert_eq!(fetched(folder.path()), Some(folder.path().join("source.mp4")));
    }
}
