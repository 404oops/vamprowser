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
    /// WebKit writes here until it reports that the download finished.
    #[serde(skip)]
    pub temporary_path: Option<PathBuf>,
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
    /// The window whose shelf owns this download for this run.
    #[serde(skip)]
    pub shelf_window: u64,
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
        let path = if self.state == DownloadState::InProgress {
            self.temporary_path.as_ref().unwrap_or(&self.path)
        } else {
            &self.path
        };
        let mut sizes = sizes().lock().ok()?;
        let seen = sizes.entry(path.clone()).or_insert(Seen { looked: None, pending: false, size: None });
        let due = seen.looked.is_none_or(|at| at.elapsed() > RECHECK);
        if due && !seen.pending {
            seen.pending = true;
            look_up(path.clone());
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

/// Looks at `path` on a thread kept for it.
fn look_up(path: PathBuf) {
    static QUEUE: OnceLock<async_channel::Sender<PathBuf>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (sender, receiver) = async_channel::unbounded::<PathBuf>();
        std::thread::spawn(move || {
            while let Ok(path) = receiver.recv_blocking() {
                let size = size_on_disk(&path);
                if let Ok(mut sizes) = sizes().lock() {
                    sizes.insert(path, Seen { looked: Some(Instant::now()), pending: false, size: Some(size) });
                }
            }
        });
        sender
    });
    let _ = queue.try_send(path);
}

/// A bundle's shelf size is its contents, not the directory entry's size.
fn size_on_disk(path: &Path) -> Option<u64> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.is_file() { return Some(metadata.len()); }
    if !metadata.is_dir() { return None; }
    let mut total = 0u64;
    let mut directories = vec![path.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).ok()? {
            let entry = entry.ok()?;
            let kind = entry.file_type().ok()?;
            if kind.is_dir() { directories.push(entry.path()); }
            else if kind.is_file() { total = total.saturating_add(entry.metadata().ok()?.len()); }
        }
    }
    Some(total)
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

/// Reserve both the eventual filename and WebKit's temporary filename.
pub fn unique_download_paths<T>(dir: &Path, name: &str, reserved: &HashMap<PathBuf, T>) -> (PathBuf, PathBuf) {
    let mut occupied = HashMap::<PathBuf, ()>::new();
    for path in reserved.keys() {
        occupied.insert(path.clone(), ());
    }
    loop {
        let final_path = unique_path(dir, name, &occupied);
        let mut temporary_name = final_path.as_os_str().to_os_string();
        temporary_name.push(".download");
        let temporary = PathBuf::from(temporary_name);
        if fs::symlink_metadata(&temporary).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            && !occupied.contains_key(&temporary)
        {
            return (final_path, temporary);
        }
        occupied.insert(final_path, ());
    }
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

    pub fn started(&mut self, url: String, path: PathBuf, private: bool, window: u64) -> u64 {
        self.started_with_temporary(url, path, None, private, window)
    }

    pub fn started_with_temporary(&mut self, url: String, path: PathBuf, temporary_path: Option<PathBuf>, private: bool, window: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.insert(
            0,
            Download {
                id,
                url,
                path,
                temporary_path,
                state: DownloadState::InProgress,
                started: now_secs(),
                private,
                on_shelf: true,
                shelf_window: window,
            },
        );
        id
    }

    /// Marks a download finished: the one saving to `path`, if WebKit says;
    /// otherwise the sole matching temporary file, or the newest matching
    /// URL when WebKit leaves the path out.
    pub fn finished(&mut self, url: &str, path: Option<PathBuf>, success: bool) -> Option<(PathBuf, PathBuf)> {
        let at = path
            .as_ref()
            .and_then(|path| self.items.iter().position(|d| d.state == DownloadState::InProgress && (d.temporary_path.as_ref() == Some(path) || &d.path == path)))
            .or_else(|| {
                if !success { return None; }
                let present: Vec<_> = self.items.iter().enumerate().filter(|(_, d)|
                    d.state == DownloadState::InProgress && d.url == url
                        && d.temporary_path.as_ref().is_some_and(|path| path.exists())
                ).map(|(at, _)| at).collect();
                if present.len() == 1 { present.first().copied() } else { None }
            })
            .or_else(|| self.items.iter().position(|d| d.state == DownloadState::InProgress && d.url == url));
        let reserved: HashMap<_, _> = self.items.iter().filter(|d| d.state == DownloadState::InProgress)
            .flat_map(|d| std::iter::once(d.path.clone()).chain(d.temporary_path.clone()).map(|path| (path, ())))
            .collect();
        let mut release = None;
        if let Some(item) = at.map(|at| &mut self.items[at]) {
            if let Some(temporary) = item.temporary_path.take() {
                release = Some((item.path.clone(), temporary.clone()));
                if success {
                    let name = item.name();
                    let dir = item.path.parent().unwrap_or(Path::new("."));
                    let mut taken = reserved;
                    taken.remove(&item.path);
                    taken.remove(&temporary);
                    let final_path = unique_path(dir, &name, &taken);
                    if fs::rename(&temporary, &final_path).is_ok() {
                        item.path = final_path;
                        item.state = DownloadState::Done;
                    } else {
                        item.path = temporary;
                        item.state = DownloadState::Failed;
                    }
                } else {
                    item.path = temporary;
                    item.state = DownloadState::Failed;
                }
            } else {
                item.state = if success { DownloadState::Done } else { DownloadState::Failed };
                if let Some(path) = path { item.path = path; }
            }
            // Looked at while it was still being written: look again.
            if let Ok(mut sizes) = sizes().lock() {
                sizes.remove(&item.path);
                if let Some((_, temporary)) = &release { sizes.remove(temporary); }
            }
        }
        self.save();
        release
    }

    /// Completes a yt-dlp job, whose final extension is known only after it runs.
    pub fn finished_id(&mut self, id: u64, path: Option<PathBuf>, success: bool) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id && item.state == DownloadState::InProgress) {
            if let Ok(mut sizes) = sizes().lock() {
                sizes.remove(&item.path);
                if let Some(path) = &path { sizes.remove(path); }
            }
            if let Some(path) = path { item.path = path; }
            item.state = if success { DownloadState::Done } else { DownloadState::Failed };
        }
        self.save();
    }

    pub fn all(&self) -> &[Download] {
        &self.items
    }

    pub fn get(&self, id: u64) -> Option<&Download> {
        self.items.iter().find(|d| d.id == id)
    }

    pub fn shelf(&self, window: u64) -> impl Iterator<Item = &Download> {
        self.items.iter().filter(move |d| d.on_shelf && d.shelf_window == window)
    }

    pub fn clear_shelf(&mut self, window: u64) {
        for item in &mut self.items {
            if item.shelf_window == window {
                item.on_shelf = false;
            }
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
    fn web_download_stays_temporary_until_finished() {
        let dir = std::env::temp_dir().join(format!("vamp-temp-dl-{}-{}", std::process::id(), now_secs()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("file.zip.download"), b"old partial").unwrap();
        let (final_path, temporary) = unique_download_paths::<()>(&dir, "file.zip", &HashMap::new());
        assert_eq!(final_path, dir.join("file 2.zip"));
        assert_eq!(temporary, dir.join("file 2.zip.download"));
        fs::write(&temporary, b"complete").unwrap();
        let mut downloads = Downloads::default();
        downloads.started_with_temporary("https://example.test/file".into(), final_path.clone(), Some(temporary.clone()), false, 1);
        assert!(!final_path.exists());
        assert_eq!(downloads.finished("https://example.test/file", None, true), Some((final_path.clone(), temporary.clone())));
        assert_eq!(downloads.all()[0].state, DownloadState::Done);
        assert_eq!(fs::read(final_path).unwrap(), b"complete");
        assert!(!temporary.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_web_download_keeps_partial_file_marked() {
        let dir = std::env::temp_dir().join(format!("vamp-failed-dl-{}-{}", std::process::id(), now_secs()));
        fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("file.zip");
        let temporary = dir.join("file.zip.download");
        fs::write(&temporary, b"partial").unwrap();
        let mut downloads = Downloads::default();
        downloads.started_with_temporary("https://example.test/file".into(), final_path.clone(), Some(temporary.clone()), false, 1);
        downloads.finished("https://example.test/file", None, false);
        assert_eq!(downloads.all()[0].state, DownloadState::Failed);
        assert_eq!(downloads.all()[0].path, temporary);
        assert!(!final_path.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn completion_without_a_saved_file_does_not_stay_in_progress() {
        let mut downloads = Downloads::default();
        let temporary = std::env::temp_dir().join(format!("vamp-missing-{}.download", std::process::id()));
        downloads.started_with_temporary("https://example.test/missing".into(), temporary.with_extension("zip"), Some(temporary), false, 1);
        downloads.finished("https://example.test/missing", None, true);
        assert_eq!(downloads.all()[0].state, DownloadState::Failed);
    }

    #[test]
    fn finishing_marks_the_newest_matching_download() {
        let mut downloads = Downloads::default();
        downloads.started("https://a/x".into(), "/tmp/x".into(), false, 1);
        downloads.items[0].state = DownloadState::Done;
        downloads.started("https://a/x".into(), "/tmp/x 2".into(), false, 2);
        downloads.finished("https://a/x", None, false);
        assert_eq!(downloads.all()[0].state, DownloadState::Failed);
        assert_eq!(downloads.all()[1].state, DownloadState::Done);
        downloads.clear_shelf(2);
        assert_eq!(downloads.shelf(2).count(), 0);
        assert_eq!(downloads.shelf(1).count(), 1);
    }

    #[test]
    fn private_downloads_stay_out_of_saved_history() {
        let mut downloads = Downloads::default();
        downloads.started("https://private.example/file".into(), "/tmp/private".into(), true, 1);
        downloads.started("https://public.example/file".into(), "/tmp/public".into(), false, 1);
        downloads.finished("https://private.example/file", None, true);
        downloads.finished("https://public.example/file", None, true);
        let json = serde_json::to_string(&downloads.saved_items()).unwrap();
        assert!(json.contains("public.example"));
        assert!(!json.contains("private.example"));
    }

    #[test]
    fn media_finish_uses_id_and_final_file_path() {
        let mut downloads = Downloads::default();
        let first = downloads.started("https://a/video".into(), "/tmp/first".into(), false, 1);
        let second = downloads.started("https://a/video".into(), "/tmp/second".into(), false, 1);
        downloads.finished_id(first, Some("/tmp/first.mp4".into()), true);
        assert_eq!(downloads.get(first).unwrap().path, PathBuf::from("/tmp/first.mp4"));
        assert_eq!(downloads.get(first).unwrap().state, DownloadState::Done);
        assert_eq!(downloads.get(second).unwrap().state, DownloadState::InProgress);
    }

    #[test]
    fn bundle_size_is_the_sum_of_its_files() {
        let dir = std::env::temp_dir().join(format!("vamp-bundle-size-{}-{}", std::process::id(), now_secs()));
        fs::create_dir_all(dir.join("subtitles")).unwrap();
        fs::write(dir.join("video.mp4"), b"12345").unwrap();
        fs::write(dir.join("subtitles/en.vtt"), b"123").unwrap();
        assert_eq!(size_on_disk(&dir), Some(8));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn active_web_download_reads_temporary_file_size() {
        let dir = std::env::temp_dir().join(format!("vamp-active-size-{}-{}", std::process::id(), now_secs()));
        fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("file.zip");
        let temporary = dir.join("file.zip.download");
        fs::write(&temporary, b"12345").unwrap();
        let mut downloads = Downloads::default();
        downloads.started_with_temporary("https://example.test/file".into(), final_path, Some(temporary), false, 1);
        let item = &downloads.all()[0];
        for _ in 0..100 {
            if item.file_size() == Some(Some(5)) {
                fs::remove_dir_all(dir).unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("active download size was not read from the temporary file");
    }
}
