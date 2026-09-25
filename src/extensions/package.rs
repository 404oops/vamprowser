//! Extension packages on disk: unpacking `.xpi`/`.zip` archives and folders
//! into the extensions directory, reading what their manifests say, and
//! fetching add-ons from addons.mozilla.org.

use std::{
    fs,
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use url::Url;

const MAX_PACKAGE_FILES: usize = 20_000;
const MAX_PACKAGE_BYTES: u64 = 512 << 20;
const MAX_PACKAGE_FILE_BYTES: u64 = 128 << 20;

/// Where installed extensions live, one unpacked folder per extension id.
pub fn extensions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Vamprowser/Extensions"))
}

/// What the browser shows about an extension before (or without) WebKit
/// having loaded it, read straight from `manifest.json`.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub homepage: Option<String>,
    pub has_action: bool,
    pub icon_png: Option<Vec<u8>>,
}

/// Reads `dir/manifest.json`, resolving `__MSG_…__` names from the default
/// locale.
pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let bytes = fs::read(dir.join("manifest.json"))
        .map_err(|err| format!("No readable manifest.json: {err}"))?;
    // Some packers write a BOM, which serde_json rejects.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let json: Value =
        serde_json::from_slice(bytes).map_err(|err| format!("Invalid manifest.json: {err}"))?;
    let messages = locale_messages(dir, json["default_locale"].as_str());
    let text = |key: &str| localize(json[key].as_str().unwrap_or_default(), &messages);
    let name = text("name");
    if name.trim().is_empty() {
        return Err("manifest.json has no name".into());
    }
    let gecko = |root: &str| json[root]["gecko"]["id"].as_str().map(str::to_owned);
    let id = gecko("browser_specific_settings")
        .or_else(|| gecko("applications"))
        .unwrap_or_else(|| name.to_lowercase());
    Ok(Manifest {
        id: sanitize_id(&id),
        version: text("version"),
        description: text("description"),
        homepage: json["homepage_url"]
            .as_str()
            .or_else(|| json["developer"]["url"].as_str())
            .map(str::to_owned),
        has_action: ["action", "browser_action", "page_action"]
            .iter()
            .any(|key| json[key].is_object()),
        icon_png: manifest_icon(dir, &json["icons"]),
        name,
    })
}

/// An extension id usable as a folder name: gecko ids like
/// `uBlock0@raymondhill.net` stay as they are, anything else is tamed.
fn sanitize_id(id: &str) -> String {
    let mut clean: String = id
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '@' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned();
    // No hidden folders, and never `.` or `..`.
    if clean.starts_with('.') {
        clean.insert(0, 'x');
    }
    if clean.is_empty() {
        clean = "extension".into();
    }
    clean
}

/// The default locale's `messages.json`, lowercased keys to messages.
fn locale_messages(dir: &Path, default_locale: Option<&str>) -> Vec<(String, String)> {
    let Some(locale) = default_locale else {
        return Vec::new();
    };
    let Ok(bytes) = fs::read(dir.join("_locales").join(locale).join("messages.json")) else {
        return Vec::new();
    };
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(bytes) else {
        return Vec::new();
    };
    map.into_iter()
        .filter_map(|(key, value)| {
            Some((key.to_lowercase(), value["message"].as_str()?.to_owned()))
        })
        .collect()
}

/// Replaces a whole-string `__MSG_key__` with its message; message keys are
/// case-insensitive.
fn localize(text: &str, messages: &[(String, String)]) -> String {
    text.strip_prefix("__MSG_")
        .and_then(|rest| rest.strip_suffix("__"))
        .and_then(|key| {
            let key = key.to_lowercase();
            messages
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, m)| m.clone())
        })
        .unwrap_or_else(|| text.to_owned())
}

/// The manifest icon nearest 64px, as PNG. SVG icons are left to WebKit,
/// whose icon replaces this one once the extension has loaded.
fn manifest_icon(dir: &Path, icons: &Value) -> Option<Vec<u8>> {
    let icons = icons.as_object()?;
    let (_, path) = icons
        .iter()
        .filter_map(|(size, path)| Some((size.parse::<i64>().ok()?, path.as_str()?)))
        .min_by_key(|(size, _)| (size - 64).abs())?;
    let path = dir.join(path.trim_start_matches('/'));
    // An icon path is the extension's own; don't read outside it.
    if !path.starts_with(dir) || path.components().any(|c| c.as_os_str() == "..") {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.starts_with(b"\x89PNG") {
        return Some(bytes);
    }
    let image = image::load_from_memory(&bytes).ok()?;
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// A fresh folder name beside the installed extensions to unpack into.
pub fn staging_dir(root: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    root.join(format!(".incoming-{}-{nanos}", std::process::id()))
}

/// Puts the extension at `source` — an `.xpi`/`.zip` archive or a folder
/// with a `manifest.json` — into `into`, with its manifest at the top.
pub fn unpack(source: &Path, into: &Path) -> Result<(), String> {
    if source.is_dir() {
        if !source.join("manifest.json").is_file() {
            return Err("That folder has no manifest.json".into());
        }
        let mut budget = (0, 0);
        return copy_dir(source, into, &mut budget).map_err(|err| format!("Could not copy extension: {err}"));
    }
    let file = fs::File::open(source).map_err(|err| format!("Could not open package: {err}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|err| format!("Not an extension package: {err}"))?;
    extract_zip(&mut archive, into, MAX_PACKAGE_FILES, MAX_PACKAGE_BYTES, MAX_PACKAGE_FILE_BYTES)?;
    if into.join("manifest.json").is_file() {
        return Ok(());
    }
    // Source archives (e.g. from GitHub) wrap everything in one folder.
    let inner = fs::read_dir(into)
        .map_err(|err| err.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    match inner.as_slice() {
        [only] if only.join("manifest.json").is_file() => {
            let lifted = into.with_extension("lifted");
            fs::rename(only, &lifted).map_err(|err| err.to_string())?;
            fs::remove_dir_all(into).map_err(|err| err.to_string())?;
            fs::rename(&lifted, into).map_err(|err| err.to_string())
        }
        _ => Err("The package has no manifest.json".into()),
    }
}

fn extract_zip<R: io::Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>, into: &Path,
    max_files: usize, max_bytes: u64, max_file_bytes: u64,
) -> Result<(), String> {
    if archive.len() > max_files {
        return Err("Extension has too many files".into());
    }
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|err| err.to_string())?;
        let relative = entry.enclosed_name().ok_or("Unsafe path in extension package")?;
        if entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err("Extension package contains a symbolic link".into());
        }
        if entry.size() > max_file_bytes ||
            total.checked_add(entry.size()).is_none_or(|size| size > max_bytes) {
            return Err("Extension package is too large".into());
        }
        let path = into.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(path).map_err(|err| err.to_string())?;
            continue;
        }
        fs::create_dir_all(path.parent().ok_or("Invalid extension path")?).map_err(|err| err.to_string())?;
        let mut out = fs::File::create(path).map_err(|err| err.to_string())?;
        let copied = io::copy(&mut entry.by_ref().take(max_file_bytes + 1), &mut out)
            .map_err(|err| err.to_string())?;
        total = total.checked_add(copied).ok_or("Extension package is too large")?;
        if copied > max_file_bytes || total > max_bytes {
            return Err("Extension package is too large".into());
        }
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path, budget: &mut (usize, u64)) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&entry.path(), &to.join(&name), budget)?;
        } else if kind.is_file() {
            budget.0 += 1;
            budget.1 = budget.1.saturating_add(entry.metadata()?.len());
            if budget.0 > MAX_PACKAGE_FILES || budget.1 > MAX_PACKAGE_BYTES {
                return Err(io::Error::other("Extension folder is too large"));
            }
            fs::copy(entry.path(), to.join(&name))?;
        } else {
            return Err(io::Error::other("Extension folder contains a link or special file"));
        }
    }
    Ok(())
}

/// The add-on an addons.mozilla.org address or bare slug/id names.
fn amo_slug(query: &str) -> Option<String> {
    let query = query.trim();
    if let Ok(url) = Url::parse(query) {
        if url.host_str() != Some("addons.mozilla.org") {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.collect();
        let at = segments.iter().position(|s| *s == "addon")?;
        return segments
            .get(at + 1)
            .filter(|s| !s.is_empty())
            .map(|s| (*s).to_owned());
    }
    (!query.is_empty() && !query.contains(['/', '?', '#', ' '])).then(|| query.to_lowercase())
}

fn addon_details(slug: &str) -> Result<Value, String> {
    let mut api =
        Url::parse("https://addons.mozilla.org/api/v5/addons/addon/").expect("valid AMO API base");
    // Pushing the segment percent-encodes ids like `{…}` or `a@b`.
    api.path_segments_mut()
        .expect("AMO API base has a path")
        .pop_if_empty()
        .push(slug)
        .push("");
    get_json(&api)
}

/// The most relevant extension on addons.mozilla.org matching a name, like
/// "Proton Pass" or "uBlock Origin".
fn search_amo(name: &str) -> Result<String, String> {
    let mut api = Url::parse("https://addons.mozilla.org/api/v5/addons/search/")
        .expect("valid AMO search URL");
    api.query_pairs_mut()
        .append_pair("q", name.trim())
        .append_pair("type", "extension")
        .append_pair("app", "firefox")
        .append_pair("sort", "relevance")
        .append_pair("page_size", "1");
    let results = get_json(&api)?;
    results["results"][0]["slug"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("No add-on called “{}” on addons.mozilla.org", name.trim()))
}

/// One agent for every request, so connections to addons.mozilla.org are
/// reused.
static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent("Vamprowser")
        .build()
        .into()
});

/// An addons.mozilla.org API answer.
fn get_json(url: &Url) -> Result<Value, String> {
    let body = AGENT
        .get(url.as_str())
        .call()
        .map_err(|err| format!("addons.mozilla.org: {err}"))?
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("addons.mozilla.org: {err}"))?;
    serde_json::from_str(&body).map_err(|err| format!("addons.mozilla.org: {err}"))
}

/// The add-on best matching `name`, and its details.
fn search_and_details(name: &str) -> Result<(String, Value), String> {
    let slug = search_amo(name)?;
    let details = addon_details(&slug)?;
    Ok((slug, details))
}

/// Downloads the current version of an addons.mozilla.org add-on to a
/// temporary `.xpi`. Blocking; meant for a background thread.
pub fn download_from_amo(query: &str) -> Result<PathBuf, String> {
    let is_link = Url::parse(query.trim()).is_ok();
    // A link or an exact slug first; anything else, or a slug that isn't
    // one, is searched for by name.
    let (slug, details) = match amo_slug(query) {
        Some(slug) => match addon_details(&slug) {
            Ok(details) => (slug, details),
            Err(err) if is_link => return Err(err),
            Err(_) => search_and_details(query)?,
        },
        None if is_link => return Err("Not an addons.mozilla.org add-on address".into()),
        None => search_and_details(query)?,
    };
    download_current(&slug, &details)
}

/// The newer version of add-on `id` on addons.mozilla.org, downloaded to a
/// temporary `.xpi`, if there is one newer than `installed`. Blocking.
pub fn newer_on_amo(id: &str, installed: &str) -> Result<Option<(String, PathBuf)>, String> {
    let details = addon_details(id)?;
    let Some(latest) = details["current_version"]["version"].as_str() else {
        return Ok(None);
    };
    if !is_newer(latest, installed) {
        return Ok(None);
    }
    download_current(id, &details).map(|path| Some((latest.to_owned(), path)))
}

/// Whether version `a` comes after `b`, comparing dotted parts as numbers
/// (a part's trailing letters, as in `2.0b3`, only break ties).
fn is_newer(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<(u64, String)> {
        v.split('.')
            .map(|part| {
                let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
                (digits.parse().unwrap_or(0), part[digits.len()..].to_owned())
            })
            .collect()
    }
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).cloned().unwrap_or((0, String::new()));
        let y = b.get(i).cloned().unwrap_or((0, String::new()));
        if x.0 != y.0 {
            return x.0 > y.0;
        }
        if x.1 != y.1 {
            // A pre-release (with letters) comes before the plain release.
            return match (x.1.is_empty(), y.1.is_empty()) {
                (true, false) => true,
                (false, true) => false,
                _ => x.1 > y.1,
            };
        }
    }
    false
}

fn download_current(slug: &str, details: &Value) -> Result<PathBuf, String> {
    let file = details["current_version"]["file"]["url"]
        .as_str()
        .ok_or("That add-on has no downloadable version")?;
    if !file.starts_with("https://") {
        return Err("addons.mozilla.org offered an insecure download".into());
    }
    // Its own file, so two downloads of one add-on at once don't write
    // into each other.
    static DOWNLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = DOWNLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "vamprowser-{}-{}-{serial}.xpi",
        sanitize_id(slug),
        std::process::id()
    ));
    let mut response = AGENT
        .get(file)
        .call()
        .map_err(|err| format!("Download failed: {err}"))?;
    let mut out =
        fs::File::create(&path).map_err(|err| format!("Could not save download: {err}"))?;
    // Streamed rather than read into memory: ureq caps in-memory bodies.
    if let Err(err) = io::copy(&mut response.body_mut().as_reader(), &mut out) {
        let _ = fs::remove_file(&path);
        return Err(format!("Download failed: {err}"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn version_order() {
        assert!(is_newer("1.76.0", "1.75.0"));
        assert!(is_newer("1.10", "1.9.9"));
        assert!(!is_newer("1.75.0", "1.75.0"));
        assert!(is_newer("2.0", "2.0b3"));
        assert!(!is_newer("2.0b3", "2.0"));
        assert!(is_newer("1.0.1", "1.0"));
    }

    #[test]
    fn amo_slugs() {
        assert_eq!(
            amo_slug("https://addons.mozilla.org/en-US/firefox/addon/ublock-origin/?src=search"),
            Some("ublock-origin".into())
        );
        assert_eq!(amo_slug("dark-reader"), Some("dark-reader".into()));
        assert_eq!(amo_slug("https://example.com/addon/x/"), None);
    }

    #[test]
    fn ids_are_folder_safe() {
        assert_eq!(
            sanitize_id("uBlock0@raymondhill.net"),
            "uBlock0@raymondhill.net"
        );
        assert_eq!(sanitize_id("{abc-123}"), "abc-123");
        assert_eq!(sanitize_id(".."), "x..");
        assert_eq!(sanitize_id("My Extension"), "My_Extension");
    }

    #[test]
    fn zip_extraction_enforces_byte_limit() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file("manifest.json", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(br#"{"name":"Example"}"#).unwrap();
        let mut archive = zip::ZipArchive::new(writer.finish().unwrap()).unwrap();
        let root = std::env::temp_dir().join(format!("vamp-zip-limit-{}", std::process::id()));
        assert!(extract_zip(&mut archive, &root, 10, 4, 4).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
