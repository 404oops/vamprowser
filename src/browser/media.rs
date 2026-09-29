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
    Bundle,
    Best,
    Format(String),
    VideoWithAudio(String),
    Subtitle(String, bool),
    Description,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Format {
    pub format_id: String,
    #[serde(default)]
    pub ext: String,
    pub format_note: Option<String>,
    pub height: Option<u32>,
    pub abr: Option<f64>,
    pub fps: Option<f64>,
    pub tbr: Option<f64>,
    #[serde(default)]
    pub vcodec: String,
    #[serde(default)]
    pub acodec: String,
}

impl Format {
    pub fn label(&self) -> String {
        let kind = if self.vcodec != "none" && !self.vcodec.is_empty() {
            self.height
                .map(|h| format!("{h}p"))
                .unwrap_or_else(|| "Video".into())
        } else {
            self.abr
                .map(|rate| format!("{} kbps", rate.round()))
                .unwrap_or_else(|| "Audio".into())
        };
        let note = if self.format_note.as_deref().unwrap_or("").is_empty() {
            String::new()
        } else {
            format!(" · {}", self.format_note.as_deref().unwrap_or(""))
        };
        format!("{kind} · {} · {}{note}", self.ext, self.format_id)
    }

    pub fn is_video(&self) -> bool {
        self.vcodec != "none" && !self.vcodec.is_empty()
    }
    pub fn is_audio(&self) -> bool {
        !self.is_video() && self.acodec != "none" && !self.acodec.is_empty()
    }

    pub fn needs_audio(&self) -> bool {
        self.acodec == "none" || self.acodec.is_empty()
    }
}

pub fn video_formats(info: &Info) -> Vec<&Format> {
    let mut formats: Vec<_> = info.formats.iter().filter(|format| format.is_video()).collect();
    // yt-dlp supplies equally ranked formats from worst to best.
    formats.reverse();
    formats.sort_by(|a, b| {
        b.height.cmp(&a.height)
            .then_with(|| b.fps.unwrap_or(0.0).total_cmp(&a.fps.unwrap_or(0.0)))
            .then_with(|| b.tbr.unwrap_or(0.0).total_cmp(&a.tbr.unwrap_or(0.0)))
    });
    formats
}

pub fn audio_formats(info: &Info) -> Vec<&Format> {
    let mut formats: Vec<_> = info.formats.iter().filter(|format| format.is_audio()).collect();
    formats.reverse();
    formats.sort_by(|a, b| {
        b.abr.or(b.tbr).unwrap_or(0.0).total_cmp(&a.abr.or(a.tbr).unwrap_or(0.0))
    });
    formats
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

pub fn destination(dir: &Path, info: &Info, choice: &Choice) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let base = folder_name(info);
    let Choice::Bundle = choice else {
        let tag = match choice {
            Choice::Best => "best video and audio".into(),
            Choice::Format(id) | Choice::VideoWithAudio(id) => format!("format {id}"),
            Choice::Subtitle(lang, automatic) => {
                format!("subtitle {lang}{}", if *automatic { " auto" } else { "" })
            }
            Choice::Description => "description".into(),
            Choice::Bundle => unreachable!(),
        };
        let tag: String = tag
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
    };
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
}

pub fn download(url: &str, destination: &Path, choice: &Choice) -> Result<PathBuf, String> {
    let result = download_inner(url, destination, choice);
    if !matches!(choice, Choice::Bundle) {
        if let Ok(mut reserved) = reservations().lock() {
            reserved.remove(destination);
        }
    }
    result
}

fn reservations() -> &'static Mutex<HashSet<PathBuf>> {
    static RESERVED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    RESERVED.get_or_init(Mutex::default)
}

fn download_inner(url: &str, destination: &Path, choice: &Choice) -> Result<PathBuf, String> {
    let exe = executable().ok_or("yt-dlp is not installed")?;
    let template = if matches!(choice, Choice::Bundle) {
        destination.join("%(title).200B [%(id)s] · %(format_id)s.%(ext)s")
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
    match choice {
        Choice::Bundle => {
            command.args([
                "-f",
                "bestvideo,bestaudio/best",
                "--write-description",
                "--write-subs",
                "--sub-langs",
                "all",
            ]);
        }
        Choice::Best => {
            command.args(["-f", "bestvideo+bestaudio/best"]);
        }
        Choice::Format(id) => {
            command.args(["-f", id]);
        }
        Choice::VideoWithAudio(id) => {
            command.args(["-f", &format!("{id}+bestaudio/{id}")]);
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
    let output = command
        .arg("--")
        .arg(url)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(error_message(&output.stderr));
    }
    if matches!(choice, Choice::Bundle) {
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
                {"format_id":"v1","ext":"mp4","format_note":null,"height":1080,"vcodec":"avc1","acodec":"none"},
                {"format_id":"a1","ext":"m4a","abr":128.0,"vcodec":"none","acodec":"mp4a"}
            ],
            "subtitles":{"en":[{"ext":"vtt"}]}
        }"#).unwrap();
        assert!(info.formats[0].is_video());
        assert!(info.formats[1].is_audio());
        assert_eq!(info.formats[0].label(), "1080p · mp4 · v1");
        assert_eq!(folder_name(&info), "A video [abc]");
        assert!(info.subtitles.contains_key("en"));
    }

    #[test]
    fn formats_are_ranked_highest_first() {
        let info: Info = serde_json::from_str(r#"{
            "formats":[
                {"format_id":"v720","height":720,"fps":30,"tbr":2500,"vcodec":"avc1","acodec":"none"},
                {"format_id":"a128","abr":128,"vcodec":"none","acodec":"mp4a"},
                {"format_id":"v1080","height":1080,"fps":30,"tbr":4000,"vcodec":"avc1","acodec":"none"},
                {"format_id":"a256","abr":256,"vcodec":"none","acodec":"mp4a"},
                {"format_id":"v1080_60","height":1080,"fps":60,"tbr":5000,"vcodec":"avc1","acodec":"none"}
            ]
        }"#).unwrap();
        let videos: Vec<_> = video_formats(&info).iter().map(|format| format.format_id.as_str()).collect();
        let audios: Vec<_> = audio_formats(&info).iter().map(|format| format.format_id.as_str()).collect();
        assert_eq!(videos, ["v1080_60", "v1080", "v720"]);
        assert_eq!(audios, ["a256", "a128"]);
        assert!(video_formats(&info)[0].needs_audio());
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
        let stem = destination(&dir, &info, &Choice::Format("602".into())).unwrap();
        assert_eq!(stem.parent(), Some(dir.as_path()));
        assert!(!stem.exists());
        let second = destination(&dir, &info, &Choice::Format("602".into())).unwrap();
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
