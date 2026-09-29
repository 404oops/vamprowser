//! How the browser meets the rest of macOS: being the default browser, and
//! showing downloaded files in Finder. Opening URLs and files handed over
//! by other apps arrives through GPUI's `on_open_urls`; the Info.plist
//! declares the schemes and document types.

use std::{collections::HashMap, path::Path, sync::{Arc, LazyLock, Mutex}};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSString, NSURL};

/// A shared icon for the filename's type. Download rows may include hundreds
/// of files, so resolving and decoding an icon for every path stalls drawing.
#[allow(deprecated)]
pub fn download_icon(path: &Path) -> Option<Arc<gpui::Image>> {
    static ICONS: LazyLock<Mutex<HashMap<String, Option<Arc<gpui::Image>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let kind = path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase();
    if let Some(icon) = ICONS.lock().ok()?.get(&kind).cloned() {
        return icon;
    }
    let image = NSWorkspace::sharedWorkspace().iconForFileType(&NSString::from_str(&kind));
    let icon = image.TIFFRepresentation().and_then(|tiff| {
        let bitmap = NSBitmapImageRep::imageRepsWithData(&tiff)
            .iter()
            .filter_map(|rep| rep.downcast::<NSBitmapImageRep>().ok())
            .max_by_key(|rep| rep.pixelsWide())?;
        // SAFETY: an empty properties dictionary is valid for PNG output.
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }?;
        Some(Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png.to_vec())))
    });
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

/// Reveals a file in Finder, selected.
pub fn show_in_finder(path: &std::path::Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSWorkspace::sharedWorkspace()
        .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
}

/// Opens a file with its default app.
pub fn open_file(path: &std::path::Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSWorkspace::sharedWorkspace().openURL(&url);
}
