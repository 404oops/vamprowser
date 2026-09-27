//! Site data (cookies and logins, local storage, IndexedDB, extensions'
//! storage) kept in the browser's own folder, `Site Data` beside the rest
//! of the profile, rather than where WebKit puts it: under the app's
//! identifier, which a development build and the installed app don't
//! share, and which reinstalling can leave behind.
//!
//! WebKit's folder is a link into `Site Data`, made before it starts. The
//! cookie jar can't be: the networking process replaces it rather than
//! following a link, starting an empty one. So WebKit keeps its jar where
//! it wants it, and `Site Data` keeps a copy, brought up to date every
//! little while and on quitting; at launch, whichever is newer wins.
//!
//! With everything in one folder, the whole profile also exports as one
//! archive, and an archive imports over it at the next launch.

use std::{
    fs, io,
    io::Read,
    path::{Path, PathBuf},
    sync::{OnceLock, atomic::{AtomicUsize, Ordering}},
    time::SystemTime,
};

use objc2_foundation::NSBundle;

/// The identifiers WebKit has kept this browser's data under: the app's,
/// and a development build's (its executable's name).
const IDENTITIES: [&str; 2] = ["dev.oops404.vamprowser", "vamprowser"];
/// Marks an archive as this browser's, with what made it.
const MANIFEST: &str = "vamprowser-archive.json";
/// Beside the profile: an archive unpacked, waiting for the next launch.
const IMPORTING: &str = "Vamprowser (importing)";
/// Beside the profile: the one an import replaced, until the next import.
const REPLACED: &str = "Vamprowser (before import)";
const MAX_ARCHIVE_FILES: usize = 100_000;
const MAX_ARCHIVE_BYTES: u64 = 8 << 30;
const MAX_ARCHIVE_FILE_BYTES: u64 = 2 << 30;

/// This process's cookie jar, where WebKit keeps it, and its copy in
/// `Site Data`; set by [`adopt`].
static JARS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
static COOKIE_CLEARS: AtomicUsize = AtomicUsize::new(0);

/// Where WebKit keeps one identity's folder and cookie jar.
fn webkit_places(library: &Path, identity: &str) -> (PathBuf, PathBuf) {
    (
        library.join("WebKit").join(identity),
        library.join("HTTPStorages").join(format!("{identity}.binarycookies")),
    )
}

/// What WebKit calls this process: the bundle's identifier, or a bare
/// executable's name.
fn identity() -> String {
    NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|id| id.to_string())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().into_owned()))
        })
        .unwrap_or_else(|| IDENTITIES[0].into())
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::symlink_metadata(path).ok()?.modified().ok()
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

fn cookie_clear_marker() -> Option<PathBuf> {
    crate::state::data_path("Site Data/.clearing-cookies")
}

/// Copies `from` over `to` whole (never half-written), with `from`'s
/// modification time, so the two compare equal until one changes.
fn copy_as_is(from: &Path, to: &Path) -> io::Result<()> {
    let when = fs::metadata(from)?.modified()?;
    let mut partial = to.as_os_str().to_owned();
    partial.push(".tmp");
    let partial = PathBuf::from(partial);
    fs::copy(from, &partial)?;
    fs::File::options().write(true).open(&partial)?.set_modified(when)?;
    fs::rename(partial, to)
}

/// Something of WebKit's own, out of the way: kept, not deleted.
fn set_aside(path: &Path) {
    let mut name = path.as_os_str().to_owned();
    name.push(".before-site-data");
    let aside = PathBuf::from(name);
    if aside.exists() {
        let _ = fs::remove_dir_all(&aside).or_else(|_| fs::remove_file(&aside));
    }
    let _ = fs::rename(path, aside);
}

/// Points WebKit at `Site Data`, first moving in whatever it has (the
/// newest of the builds' copies, the first time), and gives it the newer
/// cookie jar. Call before anything starts WebKit.
pub fn adopt() {
    let (Some(home), Some(site)) = (std::env::var_os("HOME"), crate::state::data_path("Site Data")) else {
        return;
    };
    let library = PathBuf::from(home).join("Library");
    let identity = identity();
    let (folder, jar) = webkit_places(&library, &identity);
    let (kept_folder, kept_jar) = (site.join("WebKit"), site.join("Cookies.binarycookies"));
    if let Some(marker) = cookie_clear_marker().filter(|path| path.exists()) {
        let _ = fs::remove_file(&jar);
        let _ = fs::remove_file(&kept_jar);
        let _ = fs::remove_file(marker);
    }
    if !site.exists() {
        let _ = fs::create_dir_all(&site);
        let mut candidates: Vec<&str> = IDENTITIES.to_vec();
        if !candidates.contains(&identity.as_str()) {
            candidates.push(&identity);
        }
        // Whichever build was used last has the logins that matter.
        let newest = candidates
            .into_iter()
            .filter_map(|id| {
                let (folder, jar) = webkit_places(&library, id);
                let real = folder.is_dir() && !is_link(&folder);
                real.then(|| (modified(&jar).or_else(|| modified(&folder)), id))
            })
            .max_by_key(|(when, _)| *when)
            .map(|(_, id)| id.to_owned());
        if let Some(id) = newest {
            let (from_folder, from_jar) = webkit_places(&library, &id);
            let _ = fs::rename(&from_folder, &kept_folder);
            if from_jar.is_file() {
                let _ = copy_as_is(&from_jar, &kept_jar);
            }
        }
    }
    let _ = fs::create_dir_all(&kept_folder);
    match fs::read_link(&folder) {
        Ok(to) if to == kept_folder => {}
        Ok(_) => {
            let _ = fs::remove_file(&folder);
        }
        // WebKit's own, from before, or made since by another build.
        Err(_) if fs::symlink_metadata(&folder).is_ok() => set_aside(&folder),
        Err(_) => {}
    }
    if !is_link(&folder) {
        if let Some(parent) = folder.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(err) = std::os::unix::fs::symlink(&kept_folder, &folder) {
            eprintln!("Could not keep site data in {}: {err}", kept_folder.display());
        }
    }
    // A link left by an earlier version: WebKit only replaces it.
    if is_link(&jar) {
        let _ = fs::remove_file(&jar);
    }
    let (working, kept) = (modified(&jar), modified(&kept_jar));
    if kept.is_some() && (working.is_none() || kept > working) {
        if let Some(parent) = jar.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = copy_as_is(&kept_jar, &jar);
    }
    let _ = JARS.set((jar, kept_jar));
    keep_cookies();
}

/// Prevents a pending WebKit clear from being undone by the periodic backup.
/// The marker also survives an immediate quit before WebKit's callback.
pub fn begin_cookie_clear() {
    COOKIE_CLEARS.fetch_add(1, Ordering::Relaxed);
    if let Some(marker) = cookie_clear_marker() {
        let _ = fs::write(marker, b"");
    }
    if let Some((_, kept)) = JARS.get() {
        let _ = fs::remove_file(kept);
    }
}

pub fn finish_cookie_clear() {
    if COOKIE_CLEARS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).ok() != Some(1) {
        return;
    }
    if let Some((_, kept)) = JARS.get() {
        let _ = fs::remove_file(kept);
    }
    if let Some(marker) = cookie_clear_marker() {
        let _ = fs::remove_file(marker);
    }
}

/// Brings `Site Data`'s copy of the cookie jar up to date, if WebKit has
/// written its own since. Cheap when nothing changed.
pub fn keep_cookies() {
    if cookie_clear_marker().is_some_and(|path| path.exists()) {
        return;
    }
    let Some((jar, kept)) = JARS.get() else {
        return;
    };
    let (working, copy) = (modified(jar), modified(kept));
    if working.is_some() && working > copy {
        let _ = copy_as_is(jar, kept);
    }
}

fn profile() -> Result<PathBuf, String> {
    crate::state::data_path("")
        // Without the trailing separator the empty name leaves.
        .map(|path| path.components().collect())
        .ok_or_else(|| "HOME is unset".into())
}

/// Left out of an archive: remade on their own, or half-written. Sites'
/// own caches (service workers' `CacheStorage`) are among the first: they
/// fill again as the sites are used, and can run to gigabytes.
fn skipped(relative: &Path) -> bool {
    let text = relative.to_string_lossy();
    relative.components().any(|part| part.as_os_str() == "CacheStorage")
        || text.starts_with("Site Data/WebKit/ContentRuleLists")
        || text == "Site Data/.clearing-cookies"
        || text.contains("/.incoming-")
        || text.ends_with(".tmp")
        || text.ends_with(".DS_Store")
}

/// Writes the whole profile (site data, bookmarks, history, settings,
/// extensions) to a zip archive at `to`. Blocking; call it off the main
/// thread. Returns how many files went in.
pub fn export(to: &Path) -> Result<usize, String> {
    use zip::{CompressionMethod, write::SimpleFileOptions};
    let root = profile()?;
    keep_cookies();
    let mut partial = to.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let file = fs::File::create(&partial).map_err(|err| format!("Could not write {}: {err}", to.display()))?;
    let mut zip = zip::ZipWriter::new(io::BufWriter::new(file));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);
    let written = (|| -> Result<usize, String> {
        let manifest = serde_json::json!({
            "format": 1,
            "version": env!("CARGO_PKG_VERSION"),
            "created": crate::state::now_secs(),
        });
        zip.start_file(MANIFEST, options).map_err(|err| err.to_string())?;
        io::Write::write_all(&mut zip, manifest.to_string().as_bytes()).map_err(|err| err.to_string())?;
        let mut count = 0;
        // Held to what an import takes, so an archive made here always
        // comes back: its entries (the manifest's too), each file's size,
        // and all of them together.
        let mut entries_written = 1usize;
        let mut total = 0u64;
        let mut entry_fits = |size: u64| -> Result<(), String> {
            entries_written += 1;
            total = total.saturating_add(size);
            if entries_written > MAX_ARCHIVE_FILES {
                Err(format!("there are more than {MAX_ARCHIVE_FILES} files"))
            } else if size > MAX_ARCHIVE_FILE_BYTES {
                Err(format!("a file is larger than {} GB", MAX_ARCHIVE_FILE_BYTES >> 30))
            } else if total > MAX_ARCHIVE_BYTES {
                Err(format!("it all comes to more than {} GB", MAX_ARCHIVE_BYTES >> 30))
            } else {
                Ok(())
            }
        };
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(relative) = path.strip_prefix(&root) else {
                    continue;
                };
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if skipped(relative) || kind.is_symlink() {
                    continue;
                }
                let name = relative.to_string_lossy().into_owned();
                if kind.is_dir() {
                    entry_fits(0).map_err(too_large)?;
                    zip.add_directory(format!("{name}/"), options).map_err(|err| err.to_string())?;
                    pending.push(path);
                } else if kind.is_file() {
                    // A file WebKit removes as we go is simply left out.
                    let Ok(source) = fs::File::open(&path) else {
                        continue;
                    };
                    let size = source.metadata().map_or(0, |m| m.len());
                    entry_fits(size).map_err(too_large)?;
                    zip.start_file(name, options).map_err(|err| err.to_string())?;
                    // No more than was counted, should it grow meanwhile.
                    io::copy(&mut io::Read::take(source, size), &mut zip).map_err(|err| err.to_string())?;
                    count += 1;
                }
            }
        }
        zip.finish().map_err(|err| err.to_string())?;
        Ok(count)
    })();
    match written {
        Ok(count) => {
            fs::rename(&partial, to).map_err(|err| format!("Could not write {}: {err}", to.display()))?;
            Ok(count)
        }
        Err(err) => {
            let _ = fs::remove_file(&partial);
            Err(format!("Could not export: {err}"))
        }
    }
}

fn too_large(why: String) -> String {
    // After "Could not export: ".
    format!("{why}, more than importing it again allows")
}

/// Unpacks an archive from [`export`] beside the profile, to replace it
/// at the next launch. Blocking; call it off the main thread.
pub fn stage_import(from: &Path) -> Result<(), String> {
    let root = profile()?;
    let parent = root.parent().ok_or("No profile folder")?;
    let staging = parent.join(IMPORTING);
    let file = fs::File::open(from).map_err(|err| format!("Could not open {}: {err}", from.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| "That isn't a Vamprowser archive")?;
    let manifest: serde_json::Value = {
        let mut entry = archive.by_name(MANIFEST).map_err(|_| "That isn't a Vamprowser archive")?;
        serde_json::from_reader(entry.by_ref().take(4096))
            .map_err(|_| "Invalid Vamprowser archive manifest")?
    };
    if manifest["format"].as_u64() != Some(1) {
        return Err("Unsupported Vamprowser archive format".into());
    }
    let _ = fs::remove_dir_all(&staging);
    let unpacked = unpack_archive(&mut archive, &staging, MAX_ARCHIVE_FILES, MAX_ARCHIVE_BYTES, MAX_ARCHIVE_FILE_BYTES);
    match unpacked {
        Ok(()) => fs::write(staging.join(MANIFEST), b"{}").map_err(|err| err.to_string()),
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            Err(format!("Could not import: {err}"))
        }
    }
}

fn unpack_archive<R: io::Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>, staging: &Path,
    max_files: usize, max_bytes: u64, max_file_bytes: u64,
) -> Result<(), String> {
    if archive.len() > max_files {
        return Err("Archive has too many files".into());
    }
    let mut total = 0u64;
    (|| -> Result<(), String> {
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|err| err.to_string())?;
            let relative = entry.enclosed_name().ok_or("Unsafe path in archive")?;
            if relative == Path::new(MANIFEST) {
                continue;
            }
            if entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
                return Err("Archive contains a symbolic link".into());
            }
            if entry.size() > max_file_bytes ||
                total.checked_add(entry.size()).is_none_or(|size| size > max_bytes) {
                return Err("Archive is too large".into());
            }
            let path = staging.join(relative);
            if entry.is_dir() {
                fs::create_dir_all(&path).map_err(|err| err.to_string())?;
                continue;
            }
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).map_err(|err| err.to_string())?;
            }
            let mut out = fs::File::create(&path).map_err(|err| err.to_string())?;
            let copied = io::copy(&mut entry.by_ref().take(max_file_bytes + 1), &mut out)
                .map_err(|err| err.to_string())?;
            total = total.checked_add(copied).ok_or("Archive is too large")?;
            if copied > max_file_bytes || total > max_bytes {
                return Err("Archive is too large".into());
            }
        }
        let state = fs::read(staging.join("state.json"))
            .map_err(|_| "Archive has no saved browser state")?;
        let state: serde_json::Value = serde_json::from_slice(&state)
            .map_err(|_| "Archive has invalid browser state")?;
        if !state["tabs"].is_array() && !state["windows"].is_array() {
            return Err("Archive has no saved tabs or windows".into());
        }
        Ok(())
    })()
}

/// Moves an unpacked import in place of the profile, keeping the old one
/// beside it. Call at launch, before anything reads the profile.
pub fn finish_import() {
    let Ok(root) = profile() else {
        return;
    };
    let Some(parent) = root.parent() else {
        return;
    };
    let staging = parent.join(IMPORTING);
    let replaced = parent.join(REPLACED);
    if !staging.join(MANIFEST).is_file() {
        if !root.exists() && replaced.exists() {
            let _ = fs::rename(&replaced, &root);
        }
        let _ = fs::remove_dir_all(&staging);
        return;
    }
    if root.exists() {
        let _ = fs::remove_dir_all(&replaced);
        if fs::rename(&root, &replaced).is_err() {
            eprintln!("Could not import: the current profile couldn't be moved aside");
            return;
        }
    }
    if let Err(err) = fs::rename(&staging, &root) {
        eprintln!("Could not import: {err}");
        let _ = fs::rename(&replaced, &root);
    } else {
        let _ = fs::remove_file(root.join(MANIFEST));
        // The archive's cookie jar, not the replaced profile's that WebKit
        // still keeps: `adopt` takes whichever is newer, and WebKit's was
        // written last, on the way out, so the archive's logins were lost.
        if let Some(home) = std::env::var_os("HOME") {
            let (_, jar) = webkit_places(&PathBuf::from(home).join("Library"), &identity());
            let _ = fs::remove_file(jar);
        }
    }
}

/// Opens the browser again once this process has quit.
pub fn relaunch_after_quit() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let text = exe.to_string_lossy().into_owned();
    // The app bundle through Launch Services; a bare build directly.
    let (program, target) = match text.find(".app/Contents/MacOS/") {
        Some(at) => ("open", text[..at + 4].to_owned()),
        None => ("exec", text),
    };
    let script = format!(
        "while kill -0 {} 2>/dev/null; do sleep 0.2; done; {program} \"$0\" >/dev/null 2>&1",
        std::process::id()
    );
    let _ = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn archive(files: &[(&str, &[u8])]) -> zip::ZipArchive<Cursor<Vec<u8>>> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in files {
            writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(bytes).unwrap();
        }
        zip::ZipArchive::new(writer.finish().unwrap()).unwrap()
    }

    fn staging(name: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("vamp-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn archives_leave_out_what_remakes_itself() {
        assert!(skipped(Path::new("Site Data/WebKit/ContentRuleLists/ContentRuleList-x")));
        assert!(skipped(Path::new("Site Data/.clearing-cookies")));
        assert!(skipped(Path::new("Extensions/.incoming-1-2/manifest.json")));
        assert!(skipped(Path::new("state.json.tmp")));
        assert!(!skipped(Path::new("Site Data/Cookies.binarycookies")));
        assert!(skipped(Path::new("Site Data/WebKit/WebsiteData/Default/a/a/CacheStorage/b")));
        assert!(!skipped(Path::new("Site Data/WebKit/WebsiteData/LocalStorage/x.sqlite3")));
    }

    #[test]
    fn import_needs_valid_state_and_stays_within_limits() {
        let dir = staging("import");
        let mut good = archive(&[("state.json", br#"{"tabs":["https://example.org/"]}"#)]);
        assert!(unpack_archive(&mut good, &dir, 10, 1024, 1024).is_ok());
        assert!(dir.join("state.json").is_file());
        let _ = fs::remove_dir_all(&dir);

        let mut missing = archive(&[("settings.json", b"{}")]);
        assert!(unpack_archive(&mut missing, &dir, 10, 1024, 1024).is_err());
        let _ = fs::remove_dir_all(&dir);

        let mut too_large = archive(&[("state.json", br#"{"tabs":["x"]}"#)]);
        assert!(unpack_archive(&mut too_large, &dir, 10, 4, 4).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
