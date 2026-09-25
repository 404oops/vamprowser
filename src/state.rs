use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const HOME: &str = "https://duckduckgo.com/";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedState {
    pub vertical_tabs: bool,
    pub compact_vertical_tabs: bool,
    pub bookmarks_bar: bool,
    /// The bookmark tree; see [`crate::bookmarks`].
    pub bookmarks: Vec<crate::bookmarks::Node>,
    pub tabs: Vec<String>,
    pub selected: usize,
    /// Every window's tabs, oldest window first; `tabs` and `selected`
    /// repeat the first, as older versions read them.
    pub windows: Vec<SavedWindow>,
    /// Windows closed by hand, oldest first, for ⇧⌘T to reopen. They
    /// never come back on their own at launch.
    pub closed_windows: Vec<SavedWindow>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedWindow {
    pub tabs: Vec<String>,
    /// Their titles, shown until a restored tab loads.
    pub titles: Vec<String>,
    pub selected: usize,
    /// Where the window was: x, y, width and height, in points.
    pub bounds: Option<[f32; 4]>,
}

impl Default for SavedState {
    fn default() -> Self {
        Self {
            vertical_tabs: false,
            compact_vertical_tabs: true,
            bookmarks_bar: false,
            bookmarks: Vec::new(),
            tabs: Vec::new(),
            selected: 0,
            windows: Vec::new(),
            closed_windows: Vec::new(),
        }
    }
}

/// `name` in the browser's data folder,
/// `~/Library/Application Support/Vamprowser/`.
pub fn data_path(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/Vamprowser")
            .join(name),
    )
}

/// Writes `bytes` beside `path` and renames them over it, so a crash
/// mid-write leaves the old file rather than half a new one. Makes the
/// folder if it isn't there yet.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

/// The JSON at `path`, if it's there. A file that's there but can't be
/// read as what's expected (written by another version, say) is moved
/// aside rather than left to be overwritten with defaults: bookmarks live
/// in one.
pub fn load_json<T: serde::de::DeserializeOwned>(path: Option<PathBuf>) -> Option<T> {
    let path = path?;
    let data = fs::read(&path).ok()?;
    match serde_json::from_slice(&data) {
        Ok(value) => Some(value),
        Err(err) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(format!(".unreadable-{}", now_secs()));
            eprintln!("Couldn't read {}: {err}; kept as {}", path.display(), PathBuf::from(&aside).display());
            let _ = fs::rename(&path, aside);
            None
        }
    }
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn state_path() -> Option<PathBuf> {
    data_path("state.json")
}

impl SavedState {
    pub fn load() -> Self {
        load_json(state_path()).unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = state_path().ok_or_else(|| io::Error::other("HOME is unset"))?;
        write_atomic(&path, &serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn saved_state_reads_bookmark_folders() {
        let text = r#"{"bookmarks_bar":true,"bookmarks":[
            {"title":"Wikipedia","url":"https://en.wikipedia.org/"},
            {"title":"Work","children":[{"title":"GitHub","url":"https://github.com/"},
                {"title":"Docs","children":[{"title":"Rust","url":"https://doc.rust-lang.org/"}]}]}],
            "windows":[{"tabs":["vamp://start"],"titles":[],"selected":0,"bounds":null}]}"#;
        let state: SavedState = serde_json::from_str(text).unwrap();
        assert_eq!(state.bookmarks.len(), 2);
        assert!(state.bookmarks[1].is_folder());
        assert_eq!(state.bookmarks[1].children[1].children[0].title, "Rust");
    }

    use super::*;

    #[test]
    fn saved_state_defaults_on_missing_fields() {
        let state: SavedState = serde_json::from_str("{\"vertical_tabs\":true}").unwrap();
        assert!(state.vertical_tabs);
        assert!(state.compact_vertical_tabs);
        assert!(state.tabs.is_empty());
        assert!(state.closed_windows.is_empty());
    }

    #[test]
    fn atomic_writes_replace_the_file_and_leave_no_temporary() {
        let dir = std::env::temp_dir().join(format!("vamp-state-{}", std::process::id()));
        let path = dir.join("nested/a.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!dir.join("nested/a.json.tmp").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
