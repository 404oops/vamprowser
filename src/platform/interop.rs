//! How the browser meets the rest of macOS: being the default browser, and
//! file icons for the downloads list. Opening URLs and files handed over
//! by other apps arrives through GPUI's `on_open_urls`; the Info.plist
//! declares the schemes and document types.

use std::{collections::HashMap, path::Path, sync::{Arc, LazyLock, Mutex}};

use objc2::{MainThreadMarker, Message};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSBundle, NSString, NSURL};

/// Safari-style confirmation for a page handing a URL to another app.
/// The original NSURL is passed intact to Launch Services: callback query
/// parameters can contain authentication codes and must not be rewritten.
pub fn ask_open_app(webview: &objc2_web_kit::WKWebView, url: &NSURL, host: Option<&str>) {
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn};
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(window) = webview.window() else { return };
    let workspace = NSWorkspace::sharedWorkspace();
    let alert = NSAlert::new(mtm);
    let Some(app_url) = workspace.URLForApplicationToOpenURL(url) else {
        alert.setMessageText(&NSString::from_str("No application can open this link."));
        alert.setInformativeText(&NSString::from_str(
            "Install an application that supports this type of link, then try again."
        ));
        alert.addButtonWithTitle(&NSString::from_str("OK"));
        let keep_alert = alert.clone();
        let done = block2::RcBlock::new(move |_| { let _ = &keep_alert; });
        alert.beginSheetModalForWindow_completionHandler(&window, Some(&done));
        return;
    };
    let bundle = NSBundle::bundleWithURL(&app_url);
    let name = ["CFBundleDisplayName", "CFBundleName"].into_iter().find_map(|key| {
        bundle.as_ref()?.objectForInfoDictionaryKey(&NSString::from_str(key))?
            .downcast::<NSString>().ok().map(|name| name.to_string())
    }).or_else(|| app_url.lastPathComponent().map(|name| {
        name.to_string().trim_end_matches(".app").to_owned()
    })).unwrap_or_else(|| "another application".into());
    alert.setMessageText(&NSString::from_str(&format!(
        "Allow this page to open “{name}”?"
    )));
    let page = host.map_or_else(|| "This page".to_owned(), |host| format!("The page at {host}"));
    alert.setInformativeText(&NSString::from_str(&format!(
        "{page} wants to open “{name}”."
    )));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.addButtonWithTitle(&NSString::from_str("Allow"));
    let url = url.retain();
    let keep_alert = alert.clone();
    let done = block2::RcBlock::new(move |response| {
        let _ = &keep_alert;
        if response == NSAlertFirstButtonReturn + 1 {
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
    });
    alert.beginSheetModalForWindow_completionHandler(&window, Some(&done));
}

/// A shared icon for the filename's type. Download rows may include hundreds
/// of files, so resolving and decoding an icon for every path stalls drawing.
#[allow(deprecated)]
/// The icon macOS shows for files like `path`, by extension, drawn once at
/// the size the downloads list and shelf show it (30 points, at Retina).
/// Rasterised straight from the system's icon, rather than through a TIFF
/// of all its sizes up to 1024 pixels and a PNG of the largest: that took
/// long enough per kind of file to hold the first frame of the shelf.
pub fn download_icon(path: &Path) -> Option<Arc<gpui::RenderImage>> {
    const ICON_PIXELS: u32 = 64;
    static ICONS: LazyLock<Mutex<HashMap<String, Option<Arc<gpui::RenderImage>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let kind = path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase();
    if let Some(icon) = ICONS.lock().ok()?.get(&kind).cloned() {
        return icon;
    }
    let image = NSWorkspace::sharedWorkspace().iconForFileType(&NSString::from_str(&kind));
    let icon = crate::native::render_image_of(&image, ICON_PIXELS);
    ICONS.lock().ok()?.insert(kind, icon.clone());
    icon
}

/// This app's bundle, if it is running from one. An unbundled binary can't
/// be registered with Launch Services.
fn bundle_url() -> Option<objc2::rc::Retained<NSURL>> {
    let bundle = NSBundle::mainBundle();
    let url = bundle.bundleURL();
    url.path()
        .is_some_and(|path| path.to_string().ends_with(".app"))
        .then_some(url)
}

/// Whether web links open here.
pub fn is_default_browser() -> bool {
    let (Some(ours), Some(probe)) = (
        bundle_url(),
        NSURL::URLWithString(&NSString::from_str("https://example.org/")),
    ) else {
        return false;
    };
    let workspace = NSWorkspace::sharedWorkspace();
    workspace
        .URLForApplicationToOpenURL(&probe)
        .and_then(|url| url.path())
        .zip(ours.path())
        .is_some_and(|(theirs, ours)| theirs.to_string() == ours.to_string())
}

/// Asks macOS to open web links here. It shows its own confirmation.
pub fn make_default_browser() {
    let Some(ours) = bundle_url() else {
        return;
    };
    if MainThreadMarker::new().is_none() {
        return;
    }
    let workspace = NSWorkspace::sharedWorkspace();
    for scheme in ["http", "https"] {
        workspace.setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(
            &ours,
            &NSString::from_str(scheme),
            None,
        );
    }
}
