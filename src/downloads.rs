//! Downloads: the list behind the download shelf and the Downloads page,
//! finished ones kept in `~/Library/Application Support/Vamprowser/downloads.json`.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use crate::state::{data_path, now_secs, write_atomic};

const MAX_KEPT: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    InProgress,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    pub id: u64,
    pub url: String,
    pub path: PathBuf,
    pub state: DownloadState,
    /// Seconds since the Unix epoch.
    pub started: u64,
    /// Private downloads are visible for this run but never saved.
    #[serde(skip)]
    pub private: bool,
    /// Shown on the shelf; the shelf's × hides every item without
    /// forgetting it.
    #[serde(skip)]
    pub on_shelf: bool,
}

impl Download {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.url.clone())
    }

    /// Whether the file is still where it was saved.
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// The file's size in bytes, or `None` if it isn't where it was saved;
    /// the outer `None` while that's still being looked up. Looked up off
    /// the main thread, and again every few seconds while it's asked for:
    /// the pages showing it are drawn often, and a download folder on a
    /// sleeping drive or a network share can take seconds to answer.
    pub fn file_size(&self) -> Option<Option<u64>> {
        let mut sizes = sizes().lock().ok()?;
        let seen = sizes.entry(self.path.clone()).or_insert(Seen { looked: None, pending: false, size: None });
        let due = seen.looked.is_none_or(|at| at.elapsed() > RECHECK);
        if due && !seen.pending {
            seen.pending = true;
            look_up(self.path.clone());
        }
        seen.size
    }
}

/// How often a file shown is looked at again, in case it moved.
const RECHECK: Duration = Duration::from_secs(3);

struct Seen {
    /// When it was last looked at.
    looked: Option<Instant>,
    /// A look is under way.
    pending: bool,
    /// What the last look found: its size, or `None` if it wasn't there.
    size: Option<Option<u64>>,
}

fn sizes() -> &'static Mutex<HashMap<PathBuf, Seen>> {
    static SIZES: OnceLock<Mutex<HashMap<PathBuf, Seen>>> = OnceLock::new();
    SIZES.get_or_init(Mutex::default)
}

/// Whether a file's size is still being looked up for the first time, to
/// draw again once it's in.
pub fn sizes_pending() -> bool {
    sizes()
        .lock()
        .is_ok_and(|sizes| sizes.values().any(|seen| seen.pending && seen.size.is_none()))
}

/// Looks at `path` on a thread kept for it.
fn look_up(path: PathBuf) {
    static QUEUE: OnceLock<async_channel::Sender<PathBuf>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (sender, receiver) = async_channel::unbounded::<PathBuf>();
        std::thread::spawn(move || {
            while let Ok(path) = receiver.recv_blocking() {
                let size = fs::metadata(&path).ok().map(|m| m.len());
                if let Ok(mut sizes) = sizes().lock() {
                    sizes.insert(path, Seen { looked: Some(Instant::now()), pending: false, size: Some(size) });
                }
            }
        });
        sender
    });
    let _ = queue.try_send(path);
}

#[derive(Default)]
pub struct Downloads {
    items: Vec<Download>,
    next_id: u64,
}

fn downloads_path() -> Option<PathBuf> {
    data_path("downloads.json")
}

/// A path in `dir` for a file called `name`, numbered like Finder does if
/// taken: by a file there, or by a download still writing its file there
/// (`reserved`, by path).
pub fn unique_path<T>(dir: &Path, name: &str, reserved: &std::collections::HashMap<PathBuf, T>) -> PathBuf {
    // `exists` follows links and calls a dangling symlink free.
    let free = |path: &PathBuf| fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        && !reserved.contains_key(path);
    let candidate = dir.join(name);
    if free(&candidate) {
        return candidate;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} {n}{extension}")))
        .find(free)
        .expect("some number is free")
}

impl Downloads {
    pub fn load() -> Self {
        let items: Vec<Download> = crate::state::load_json(downloads_path()).unwrap_or_default();
        let next_id = items.iter().map(|d| d.id + 1).max().unwrap_or(1);
        Self { items, next_id }
    }

    fn save(&self) {
        // Tests exercise the list, never the user's file.
        if cfg!(test) {
            return;
        }
        let Some(path) = downloads_path() else {
            return;
        };
        let kept = self.saved_items();
        if let Ok(data) = serde_json::to_vec_pretty(&kept) {
            let _ = write_atomic(&path, &data);
        }
    }

    fn saved_items(&self) -> Vec<&Download> {
        self
            .items
            .iter()
            .filter(|d| d.state != DownloadState::InProgress && !d.private)
            .take(MAX_KEPT)
            .collect()
    }

    pub fn started(&mut self, url: String, path: PathBuf, private: bool) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.insert(
            0,
            Download {
                id,
                url,
                path,
                state: DownloadState::InProgress,
                started: now_secs(),
                private,
                on_shelf: true,
            },
        );
        id
    }

    /// Marks a download finished: the one saving to `path`, if WebKit says,
    /// since the same address can be downloading twice; else the newest of
    /// `url`.
    pub fn finished(&mut self, url: &str, path: Option<PathBuf>, success: bool) {
        let running = |d: &&mut Download| d.state == DownloadState::InProgress;
        let at = path
            .as_ref()
            .and_then(|path| self.items.iter().position(|d| d.state == DownloadState::InProgress && &d.path == path))
            .or_else(|| self.items.iter_mut().position(|d| running(&d) && d.url == url));
        if let Some(item) = at.map(|at| &mut self.items[at]) {
            item.state = if success {
                DownloadState::Done
            } else {
                DownloadState::Failed
            };
            if let Some(path) = path {
                item.path = path;
            }
            // Looked at while it was still being written: look again.
            if let Ok(mut sizes) = sizes().lock() {
                sizes.remove(&item.path);
            }
        }
        self.save();
    }

    pub fn all(&self) -> &[Download] {
        &self.items
    }

    pub fn get(&self, id: u64) -> Option<&Download> {
        self.items.iter().find(|d| d.id == id)
    }

    pub fn shelf(&self) -> impl Iterator<Item = &Download> {
        self.items.iter().filter(|d| d.on_shelf)
    }

    pub fn clear_shelf(&mut self) {
        for item in &mut self.items {
            item.on_shelf = false;
        }
    }

    pub fn remove(&mut self, id: u64) {
        self.items.retain(|d| d.id != id);
        self.save();
    }

    /// Forgets finished and failed downloads; files stay on disk.
    pub fn clear(&mut self) {
        self.items.retain(|d| d.state == DownloadState::InProgress);
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_paths_number_like_finder() {
        let dir = std::env::temp_dir().join(format!("vamp-dl-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path::<()>(&dir, "a.zip", &Default::default()), dir.join("a.zip"));
        fs::write(dir.join("a.zip"), b"").unwrap();
        assert_eq!(unique_path::<()>(&dir, "a.zip", &Default::default()), dir.join("a 2.zip"));
        // A download still writing counts as taken.
        let reserved = std::iter::once((dir.join("a 2.zip"), ())).collect();
        assert_eq!(unique_path(&dir, "a.zip", &reserved), dir.join("a 3.zip"));
        fs::write(dir.join("README"), b"").unwrap();
        assert_eq!(unique_path::<()>(&dir, "README", &Default::default()), dir.join("README 2"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("missing"), dir.join("link")).unwrap();
            assert_eq!(unique_path::<()>(&dir, "link", &Default::default()), dir.join("link 2"));
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn finishing_marks_the_newest_matching_download() {
        let mut downloads = Downloads::default();
        downloads.started("https://a/x".into(), "/tmp/x".into(), false);
        downloads.items[0].state = DownloadState::Done;
        downloads.started("https://a/x".into(), "/tmp/x 2".into(), false);
        downloads.finished("https://a/x", None, false);
        assert_eq!(downloads.all()[0].state, DownloadState::Failed);
        assert_eq!(downloads.all()[1].state, DownloadState::Done);
        downloads.clear_shelf();
        assert_eq!(downloads.shelf().count(), 0);
    }

    #[test]
    fn private_downloads_stay_out_of_saved_history() {
        let mut downloads = Downloads::default();
        downloads.started("https://private.example/file".into(), "/tmp/private".into(), true);
        downloads.started("https://public.example/file".into(), "/tmp/public".into(), false);
        downloads.finished("https://private.example/file", None, true);
        downloads.finished("https://public.example/file", None, true);
        let json = serde_json::to_string(&downloads.saved_items()).unwrap();
        assert!(json.contains("public.example"));
        assert!(!json.contains("private.example"));
    }
}
