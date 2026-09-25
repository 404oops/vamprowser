//! macOS file pickers, extension permission alerts, and WebKit page snapshots.

use std::cell::Cell;

use block2::RcBlock;
use objc2::{AnyThread, MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
use objc2_foundation::{
    NSCalendar, NSCalendarUnit, NSDate, NSDateFormatter, NSDateFormatterStyle, NSDictionary,
    NSError, NSString,
};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};

/// One row of a context menu.
#[derive(Clone)]
pub enum MenuEntry {
    Item {
        label: String,
        enabled: bool,
        checked: bool,
    },
    Separator,
    /// A row that opens another menu.
    Submenu { label: String, entries: Vec<MenuEntry> },
}

impl MenuEntry {
    pub fn item(label: impl Into<String>) -> Self {
        MenuEntry::Item {
            label: label.into(),
            enabled: true,
            checked: false,
        }
    }

    pub fn disabled(label: impl Into<String>) -> Self {
        MenuEntry::Item {
            label: label.into(),
            enabled: false,
            checked: false,
        }
    }

    pub fn checked(label: impl Into<String>, checked: bool) -> Self {
        MenuEntry::Item {
            label: label.into(),
            enabled: true,
            checked,
        }
    }
}

/// Asks for a file to open, of the given extensions. Blocks until chosen.
pub fn choose_file(extensions: &[&str]) -> Option<std::path::PathBuf> {
    let mtm = MainThreadMarker::new()?;
    let panel = objc2_app_kit::NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    #[allow(deprecated)]
    {
        let types: Vec<Retained<NSString>> = extensions.iter().map(|e| NSString::from_str(e)).collect();
        let types = objc2_foundation::NSArray::from_retained_slice(&types);
        panel.setAllowedFileTypes(Some(&types));
    }
    if panel.runModal() != objc2_app_kit::NSModalResponseOK {
        return None;
    }
    panel.URL()?.path().map(|p| std::path::PathBuf::from(p.to_string()))
}

/// Asks where to save a file, suggesting `name`. Blocks until chosen.
pub fn choose_save_path(name: &str) -> Option<std::path::PathBuf> {
    let mtm = MainThreadMarker::new()?;
    let panel = objc2_app_kit::NSSavePanel::savePanel(mtm);
    panel.setNameFieldStringValue(&NSString::from_str(name));
    if panel.runModal() != objc2_app_kit::NSModalResponseOK {
        return None;
    }
    panel.URL()?.path().map(|p| std::path::PathBuf::from(p.to_string()))
}

/// Asks WebKit for a still of the page as JPEG, delivered on the main
/// thread; `None` if it couldn't.
pub fn snapshot(webview: &WKWebView, done: impl FnOnce(Option<Vec<u8>>) + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        done(None);
        return;
    };
    let done = Cell::new(Some(done));
    let handler = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
        let Some(done) = done.take() else {
            return;
        };
        // SAFETY: WebKit passes a valid image or null for the callback's
        // duration.
        let bytes = unsafe { image.as_ref() }.and_then(jpeg);
        done(bytes);
    });
    // SAFETY: a fresh configuration and a block of the documented type;
    // WebKit calls it once on the main thread.
    unsafe {
        let configuration = WKSnapshotConfiguration::new(mtm);
        webview.takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &handler);
    }
}

/// Straight from the image's bitmap, rather than through a TIFF encoding
/// and back.
fn jpeg(image: &NSImage) -> Option<Vec<u8>> {
    // SAFETY: a null rect asks for the image at its own size, and no
    // context or hints are needed.
    let cg_image =
        unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg_image);
    let properties = NSDictionary::new();
    // SAFETY: an empty properties dictionary is valid for any file type.
    let data = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::JPEG, &properties)
    }?;
    Some(data.to_vec())
}

/// The hour of the day where the user is, 0 to 23.
pub fn local_hour() -> u32 {
    let calendar = NSCalendar::currentCalendar();
    calendar.component_fromDate(NSCalendarUnit::Hour, &NSDate::now()) as u32
}

/// Today's date the way the user's locale writes it in full.
pub fn long_date() -> String {
    NSDateFormatter::localizedStringFromDate_dateStyle_timeStyle(
        &NSDate::now(),
        NSDateFormatterStyle::FullStyle,
        NSDateFormatterStyle::NoStyle,
    )
    .to_string()
}
