//! yt-dlp inspection and downloads. Processes run off the UI thread.

use serde::Deserialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    /// A video format, merged with `audio` when it has none of its own.
    /// As a bundle it comes in a folder with subtitles and the description.
    Video {
        video: String,
        audio: Option<String>,
        bundle: bool,
    },
    Audio(String),
    Subtitle(String, bool),
    Description,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Format {
    pub format_id: String,
    #[serde(default)]
    pub ext: String,
    pub height: Option<u32>,
    pub abr: Option<f64>,
    pub fps: Option<f64>,
    pub tbr: Option<f64>,
    pub filesize: Option<f64>,
    pub filesize_approx: Option<f64>,
    #[serde(default)]
    pub protocol: String,
    #[serde(default, deserialize_with = "codec")]
    pub vcodec: String,
    #[serde(default, deserialize_with = "codec")]
    pub acodec: String,
}

/// yt-dlp sends null for a codec it doesn't know.
fn codec<'de, D: serde::Deserializer<'de>>(from: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(from)?.unwrap_or_default())
}

/// A codec as people know it, rather than its RFC 6381 tag.
fn codec_name(codec: &str) -> &str {
    match codec.split('.').next().unwrap_or(codec) {
        "avc1" | "avc3" | "h264" => "H.264",
        "hvc1" | "hev1" | "h265" | "hevc" => "HEVC",
        "vp09" | "vp9" => "VP9",
        "vp8" => "VP8",
        "av01" | "av1" => "AV1",
        "mp4a" | "aac" => "AAC",
        "opus" => "Opus",
        "vorbis" => "Vorbis",
        "mp3" => "MP3",
        "flac" => "FLAC",
        "ac-3" | "ac3" => "AC-3",
        "ec-3" | "eac3" => "E-AC-3",
        _ => codec,
    }
}

impl Format {
    pub fn is_video(&self) -> bool {
        self.vcodec != "none" && !self.vcodec.is_empty()
    }
    pub fn is_audio(&self) -> bool {
        !self.is_video() && self.has_audio()
    }
    pub fn has_audio(&self) -> bool {
        self.acodec != "none" && !self.acodec.is_empty()
    }

    /// Whether QuickTime and the rest of macOS play it without help.
    pub fn plays_natively(&self) -> bool {
        let video = !self.is_video() || matches!(codec_name(&self.vcodec), "H.264" | "HEVC");
        let audio = !self.has_audio() || matches!(codec_name(&self.acodec), "AAC" | "MP3");
        video && audio
    }

    pub fn size(&self) -> Option<u64> {
        self.filesize.or(self.filesize_approx).map(|size| size as u64)
    }

    /// "1080p60 · H.264" or "AAC · 128 kbps".
    pub fn quality(&self) -> String {
        if self.is_video() {
            let resolution = match (self.height, self.fps) {
                (Some(height), Some(fps)) if fps > 31.0 => format!("{height}p{}", fps.round()),
                (Some(height), _) => format!("{height}p"),
                (None, _) => "Video".into(),
            };
            format!("{resolution} · {}", codec_name(&self.vcodec))
        } else {
            match self.abr.or(self.tbr) {
                Some(rate) => format!("{} · {} kbps", codec_name(&self.acodec), rate.round()),
                None => codec_name(&self.acodec).to_owned(),
            }
        }
    }

    /// Streamed in pieces rather than served as one file.
    fn segmented(&self) -> bool {
        self.protocol.contains("m3u8") || self.protocol.contains("dash")
    }
}

/// Video formats, highest first, each resolution and codec once.
pub fn video_formats(info: &Info) -> Vec<&Format> {
    let mut formats: Vec<&Format> = Vec::new();
    for format in info.formats.iter().filter(|format| format.is_video()) {
        let same = |other: &&Format| {
            other.height == format.height
                && other.fps.map(f64::round) == format.fps.map(f64::round)
                && codec_name(&other.vcodec) == codec_name(&format.vcodec)
                && other.has_audio() == format.has_audio()
        };
        // Of copies, one served whole, then the larger.
        let better = |other: &Format| {
            (!format.segmented(), format.tbr.unwrap_or(0.0))
                > (!other.segmented(), other.tbr.unwrap_or(0.0))
        };
        match formats.iter().position(same) {
            Some(at) if better(formats[at]) => formats[at] = format,
            Some(_) => {}
            None => formats.push(format),
        }
    }
    formats.sort_by(|a, b| {
        b.height.cmp(&a.height)
            .then_with(|| b.fps.unwrap_or(0.0).total_cmp(&a.fps.unwrap_or(0.0)))
            .then_with(|| b.plays_natively().cmp(&a.plays_natively()))
            .then_with(|| b.tbr.unwrap_or(0.0).total_cmp(&a.tbr.unwrap_or(0.0)))
    });
    formats
}

pub fn audio_formats(info: &Info) -> Vec<&Format> {
    let mut formats: Vec<_> = info.formats.iter().filter(|format| format.is_audio()).collect();
    formats.sort_by(|a, b| {
        b.abr.or(b.tbr).unwrap_or(0.0).total_cmp(&a.abr.or(a.tbr).unwrap_or(0.0))
            .then_with(|| b.plays_natively().cmp(&a.plays_natively()))
    });
    formats
}

/// The best audio macOS plays, or failing that the best there is.
pub fn best_audio(info: &Info) -> Option<&Format> {
    let formats = audio_formats(info);
    formats.iter().find(|format| format.plays_natively()).or(formats.first()).copied()
}

/// The highest video macOS plays, or failing that the highest there is.
/// Higher resolutions in other codecs stay a choice in the full list.
pub fn best_video(info: &Info) -> Option<&Format> {
    let formats = video_formats(info);
    formats.iter().find(|format| format.plays_natively()).or(formats.first()).copied()
}

/// The audio to merge into `video`: none when it has its own.
pub fn audio_for<'a>(info: &'a Info, video: &Format) -> Option<&'a Format> {
    if video.has_audio() { None } else { best_audio(info) }
}

pub fn format<'a>(info: &'a Info, id: &str) -> Option<&'a Format> {
    info.formats.iter().find(|format| format.format_id == id)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Info {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub formats: Vec<Format>,
    #[serde(default)]
    pub subtitles: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub automatic_captions: std::collections::BTreeMap<String, serde_json::Value>,
    pub description: Option<String>,
}

fn executable() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join("yt-dlp"))
        .chain([
            PathBuf::from("/opt/homebrew/bin/yt-dlp"),
            PathBuf::from("/usr/local/bin/yt-dlp"),
        ])
        .find(|path| path.is_file())
}

pub fn inspect(url: &str) -> Result<Info, String> {
    let exe = executable()
        .ok_or("yt-dlp is not installed. Install it with Homebrew: brew install yt-dlp")?;
    let output = Command::new(exe)
        .args([
            "--no-playlist",
            "--dump-single-json",
            "--no-warnings",
            "--",
            url,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(error_message(&output.stderr));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("Couldn't read yt-dlp formats: {e}"))
}

fn error_message(stderr: &[u8]) -> String {
    let message = String::from_utf8_lossy(stderr);
    message
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("yt-dlp failed")
        .trim()
        .to_owned()
}

pub fn folder_name(info: &Info) -> String {
    let name: String = info
        .title
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:%".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(100)
        .collect();
    let name = name.trim().trim_matches('.');
    let name = if name.is_empty() { "Media" } else { name };
    let id: String = info
        .id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    if id.is_empty() {
        name.to_owned()
    } else {
        format!("{name} [{id}]")
    }
}

/// Where a choice saves: a new folder for a bundle, otherwise a file
/// stem in `dir` that yt-dlp completes with the extension.
pub fn destination(dir: &Path, info: &Info, choice: &Choice) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let base = folder_name(info);
    let tag = match choice {
        Choice::Video { bundle: true, .. } => None,
        Choice::Video { video, .. } => Some(format(info, video).map_or_else(|| "video".into(), Format::quality)),
        Choice::Audio(id) => Some(format(info, id).map_or_else(|| "audio".into(), Format::quality)),
        Choice::Subtitle(lang, automatic) => {
            Some(format!("subtitles {lang}{}", if *automatic { " automatic" } else { "" }))
        }
        Choice::Description => Some("description".into()),
    };
    let Some(tag) = tag else {
        loop {
            let path = crate::downloads::unique_path(
                dir,
                &base,
                &std::collections::HashMap::<PathBuf, ()>::new(),
            );
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
    };
    let tag: String = tag
        .replace(" · ", " ")
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:%".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let base = format!("{base} · {tag}");
    let mut reserved = reservations().lock().map_err(|e| e.to_string())?;
    for number in 1.. {
        let name = if number == 1 {
            base.clone()
        } else {
            format!("{base} {number}")
        };
        let stem = dir.join(name);
        if !reserved.contains(&stem) && individual_file(&stem).is_err() {
            reserved.insert(stem.clone());
            return Ok(stem);
        }
    }
    unreachable!();
}

/// Runs one download at a time: YouTube answers concurrent ones with 403
/// Forbidden. A 403 can still come alone, and a fresh run with newly
/// extracted addresses usually gets through.
pub fn download(url: &str, destination: &Path, choice: &Choice) -> Result<PathBuf, String> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let result = {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut result = download_inner(url, destination, choice);
        for wait in [2, 6] {
            match &result {
                Err(error) if forbidden(error) => {
                    std::thread::sleep(std::time::Duration::from_secs(wait));
                    result = download_inner(url, destination, choice);
                }
                _ => break,
            }
        }
        result.map_err(|error| {
            if forbidden(&error) {
                format!("{error}\n\nThe site refused the download three times. Try again later, or update yt-dlp: brew upgrade yt-dlp")
            } else {
                error
            }
        })
    };
    if !bundle(choice) {
        if let Ok(mut reserved) = reservations().lock() {
            reserved.remove(destination);
        }
    }
    result
}

fn forbidden(error: &str) -> bool {
    error.contains("HTTP Error 403")
}

fn bundle(choice: &Choice) -> bool {
    matches!(choice, Choice::Video { bundle: true, .. })
}

fn reservations() -> &'static Mutex<HashSet<PathBuf>> {
    static RESERVED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    RESERVED.get_or_init(Mutex::default)
}

/// Asks for exactly the formats chosen, so the file is what the menu said.
fn configure_choice(command: &mut Command, choice: &Choice) {
    match choice {
        Choice::Video { video, audio, bundle } => {
            match audio {
                Some(audio) => command.args(["-f", &format!("{video}+{audio}"), "--merge-output-format", "mp4/mkv"]),
                None => command.args(["-f", video]),
            };
            if *bundle {
                command.args(["--write-description", "--write-subs", "--sub-langs", "all,-live_chat"]);
            }
        }
        Choice::Audio(id) => {
            command.args(["-f", id]);
        }
        Choice::Subtitle(lang, automatic) => {
            command.args([
                "--skip-download",
                if *automatic {
                    "--write-auto-subs"
                } else {
                    "--write-subs"
                },
                "--sub-langs",
                lang,
            ]);
        }
        Choice::Description => {
            command.args(["--skip-download", "--write-description"]);
        }
    }
}

fn download_inner(url: &str, destination: &Path, choice: &Choice) -> Result<PathBuf, String> {
    let exe = executable().ok_or("yt-dlp is not installed")?;
    // A bundle's files share the folder's name: the video, then its
    // subtitles and description beside it.
    let template = if bundle(choice) {
        let name = destination.file_name().ok_or("Invalid download name")?.to_string_lossy();
        destination.join(format!("{name}.%(ext)s"))
    } else {
        PathBuf::from(format!("{}.%(ext)s", destination.display()))
    };
    let mut command = Command::new(exe);
    command
        .args([
            "--no-playlist",
            "--no-warnings",
            "--no-progress",
            "--no-overwrites",
            "-o",
        ])
        .arg(template);
    configure_choice(&mut command, choice);
    let output = command
        .arg("--")
        .arg(url)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(error_message(&output.stderr));
    }
    if bundle(choice) {
        Ok(destination.to_owned())
    } else {
        individual_file(destination)
    }
}

fn individual_file(stem: &Path) -> Result<PathBuf, String> {
    let parent = stem.parent().ok_or("Invalid download path")?;
    let prefix = format!(
        "{}.",
        stem.file_name()
            .ok_or("Invalid download name")?
            .to_string_lossy()
    );
    std::fs::read_dir(parent)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .ok_or_else(|| "yt-dlp reported success, but no file was saved".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_realistic_format_metadata() {
        let info: Info = serde_json::from_str(r#"{
            "id":"abc", "title":"A video", "description":null,
            "formats":[
                {"format_id":"v1","ext":"mp4","format_note":null,"height":1080,"vcodec":"avc1.640028","acodec":"none"},
                {"format_id":"a1","ext":"m4a","abr":128.0,"vcodec":"none","acodec":"mp4a.40.2"},
                {"format_id":"233","ext":"mp4","vcodec":"none","acodec":null}
            ],
            "subtitles":{"en":[{"ext":"vtt"}]}
        }"#).unwrap();
        assert!(info.formats[0].is_video());
        assert!(info.formats[1].is_audio());
        assert!(!info.formats[2].is_audio(), "An unknown codec isn't offered");
        assert_eq!(info.formats[0].quality(), "1080p · H.264");
        assert_eq!(info.formats[1].quality(), "AAC · 128 kbps");
        assert_eq!(folder_name(&info), "A video [abc]");
        assert!(info.subtitles.contains_key("en"));
    }

    /// As YouTube serves a video: the same resolutions in several codecs,
    /// some twice over HLS, video and audio apart.
    fn youtube() -> Info {
        serde_json::from_str(r#"{
            "id":"kNSwXvBw9sI", "title":"Saxo beat",
            "formats":[
                {"format_id":"139","ext":"m4a","abr":49,"protocol":"https","vcodec":"none","acodec":"mp4a.40.5"},
                {"format_id":"140","ext":"m4a","abr":130,"filesize":3000000,"protocol":"https","vcodec":"none","acodec":"mp4a.40.2"},
                {"format_id":"251","ext":"webm","abr":131,"protocol":"https","vcodec":"none","acodec":"opus"},
                {"format_id":"230","ext":"mp4","height":360,"fps":30,"tbr":577,"protocol":"m3u8_native","vcodec":"avc1.4D401E","acodec":"none"},
                {"format_id":"134","ext":"mp4","height":360,"fps":30,"tbr":313,"filesize":7250000,"protocol":"https","vcodec":"avc1.4D401E","acodec":"none"},
                {"format_id":"243","ext":"webm","height":360,"fps":30,"tbr":356,"protocol":"https","vcodec":"vp9","acodec":"none"},
                {"format_id":"396","ext":"mp4","height":360,"fps":30,"tbr":376,"protocol":"https","vcodec":"av01.0.01M.08","acodec":"none"},
                {"format_id":"133","ext":"mp4","height":240,"fps":30,"tbr":169,"protocol":"https","vcodec":"avc1.4D4015","acodec":"none"},
                {"format_id":"sb0","ext":"mhtml","protocol":"mhtml","vcodec":"none","acodec":"none"}
            ]
        }"#).unwrap()
    }

    #[test]
    fn formats_are_ranked_highest_first_each_once() {
        let info = youtube();
        let videos: Vec<_> = video_formats(&info).iter().map(|format| format.format_id.as_str()).collect();
        let audios: Vec<_> = audio_formats(&info).iter().map(|format| format.format_id.as_str()).collect();
        assert_eq!(videos, ["134", "396", "243", "133"], "Served whole beats HLS; macOS's codecs first");
        assert_eq!(audios, ["251", "140", "139"]);
    }

    #[test]
    fn the_default_plays_in_quicktime() {
        let info = youtube();
        let video = best_video(&info).unwrap();
        assert_eq!(video.format_id, "134");
        assert_eq!(audio_for(&info, video).unwrap().format_id, "140", "AAC over Opus");
        assert_eq!(best_audio(&info).unwrap().format_id, "140");
    }

    #[test]
    fn downloads_ask_for_exactly_the_chosen_formats() {
        let args = |choice: Choice| {
            let mut command = Command::new("yt-dlp");
            configure_choice(&mut command, &choice);
            command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>()
        };
        let merged = args(Choice::Video { video: "396".into(), audio: Some("140".into()), bundle: false });
        assert_eq!(merged, ["-f", "396+140", "--merge-output-format", "mp4/mkv"]);
        let bundle = args(Choice::Video { video: "134".into(), audio: Some("140".into()), bundle: true });
        assert!(bundle.starts_with(&["-f".into(), "134+140".into()]), "One merged video, not separate streams");
        assert!(bundle.contains(&"--write-subs".into()));
        assert_eq!(args(Choice::Video { video: "18".into(), audio: None, bundle: false }), ["-f", "18"]);
        assert_eq!(args(Choice::Audio("140".into())), ["-f", "140"]);
    }

    #[test]
    fn folder_name_cannot_escape_download_directory() {
        let info = Info {
            title: "../bad/name: title".into(),
            id: "id/other".into(),
            formats: Vec::new(),
            subtitles: Default::default(),
            automatic_captions: Default::default(),
            description: None,
        };
        assert_eq!(folder_name(&info), "_bad_name_ title [idother]");
    }

    #[test]
    fn individual_destination_is_a_file_stem_in_downloads() {
        let dir = std::env::temp_dir().join(format!(
            "vamp-media-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let info = Info {
            title: "Example".into(),
            id: "abc".into(),
            formats: Vec::new(),
            subtitles: Default::default(),
            automatic_captions: Default::default(),
            description: None,
        };
        let choice = Choice::Audio("140".into());
        let stem = destination(&dir, &info, &choice).unwrap();
        assert_eq!(stem.parent(), Some(dir.as_path()));
        assert!(!stem.exists());
        let second = destination(&dir, &info, &choice).unwrap();
        assert_ne!(stem, second);
        let file = PathBuf::from(format!("{}.mp4", stem.display()));
        std::fs::write(&file, b"sample").unwrap();
        assert_eq!(individual_file(&stem).unwrap(), file);
        let mut reserved = reservations().lock().unwrap();
        reserved.remove(&stem);
        reserved.remove(&second);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
