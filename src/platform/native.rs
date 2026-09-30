//! macOS file pickers, extension permission alerts, and WebKit page snapshots.

use std::cell::Cell;

use block2::RcBlock;
use objc2::{AnyThread, MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSBitmapImageFileType, NSBitmapImageRep, NSImage,
};
use objc2_foundation::{
    NSCalendar, NSCalendarUnit, NSDate, NSDateFormatter, NSDateFormatterStyle, NSDictionary,
    NSError, NSNumber, NSString,
};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};

use crate::icons::Icon;

/// WebKit's default media prompt forgets its answer when a view is rebuilt.
/// Ask here so an explicit lasting choice can be stored for this host.
pub fn ask_site_permission(
    host: &str,
    slot: usize,
    private: bool,
) -> (
    wry::PermissionResponse,
    Option<crate::settings::SitePermission>,
) {
    use crate::settings::SitePermission;
    use wry::PermissionResponse;
    let Some(mtm) = MainThreadMarker::new() else {
        return (PermissionResponse::Default, None);
    };
    let name = ["camera", "microphone", "screen"][slot];
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!(
        "Allow {host} to use your {name}?"
    )));
    let detail = if private {
        "This private tab forgets its choices when closed."
    } else {
        "Change a saved choice from the site's icon in the address bar."
    };
    alert.setInformativeText(&NSString::from_str(detail));
    for label in [
        if private { "Allow for Tab" } else { "Allow Always" },
        "Allow Once",
        if private { "Block for Tab" } else { "Block Always" },
        "Block Once",
    ] {
        alert.addButtonWithTitle(&NSString::from_str(label));
    }
    match alert.runModal() - NSAlertFirstButtonReturn {
        0 => (PermissionResponse::Allow, Some(SitePermission::Allow)),
        1 => (PermissionResponse::Allow, None),
        2 => (PermissionResponse::Deny, Some(SitePermission::Block)),
        _ => (PermissionResponse::Deny, None),
    }
}

/// One row of a context menu.
#[derive(Clone)]
pub enum MenuEntry {
    Item {
        label: String,
        enabled: bool,
        checked: bool,
        /// Drawn ahead of the label, so a row can be found by its shape.
        icon: Option<Icon>,
    },
    Separator,
    /// A row that opens another menu.
    Submenu {
        label: String,
        entries: Vec<MenuEntry>,
        icon: Option<Icon>,
    },
}

impl MenuEntry {
    pub fn item(label: impl Into<String>) -> Self {
        MenuEntry::Item {
            label: label.into(),
            enabled: true,
            checked: false,
            icon: None,
        }
    }

    pub fn disabled(label: impl Into<String>) -> Self {
        MenuEntry::Item {
            label: label.into(),
            enabled: false,
            checked: false,
            icon: None,
        }
    }

    pub fn checked(label: impl Into<String>, checked: bool) -> Self {
        MenuEntry::Item {
            label: label.into(),
            enabled: true,
            checked,
            icon: None,
        }
    }

    /// The same row with `icon` ahead of its label.
    pub fn with_icon(mut self, with: Icon) -> Self {
        match &mut self {
            MenuEntry::Item { icon, .. } | MenuEntry::Submenu { icon, .. } => *icon = Some(with),
            MenuEntry::Separator => {}
        }
        self
    }

    pub fn icon(&self) -> Option<Icon> {
        match self {
            MenuEntry::Item { icon, .. } | MenuEntry::Submenu { icon, .. } => *icon,
            MenuEntry::Separator => None,
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
        let types: Vec<Retained<NSString>> =
            extensions.iter().map(|e| NSString::from_str(e)).collect();
        let types = objc2_foundation::NSArray::from_retained_slice(&types);
        panel.setAllowedFileTypes(Some(&types));
    }
    if panel.runModal() != objc2_app_kit::NSModalResponseOK {
        return None;
    }
    panel
        .URL()?
        .path()
        .map(|p| std::path::PathBuf::from(p.to_string()))
}

/// Asks where to save a file, suggesting `name`. Blocks until chosen.
pub fn choose_save_path(name: &str) -> Option<std::path::PathBuf> {
    let mtm = MainThreadMarker::new()?;
    let panel = objc2_app_kit::NSSavePanel::savePanel(mtm);
    panel.setNameFieldStringValue(&NSString::from_str(name));
    if panel.runModal() != objc2_app_kit::NSModalResponseOK {
        return None;
    }
    panel
        .URL()?
        .path()
        .map(|p| std::path::PathBuf::from(p.to_string()))
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
        let bounds = webview.bounds();
        let scale = webview
            .window()
            .map_or(1.0, |window| window.backingScaleFactor());
        if let Some(width) = snapshot_width(bounds.size.width, bounds.size.height, scale) {
            configuration.setSnapshotWidth(Some(&NSNumber::new_f64(width)));
        }
        // These images temporarily stand in for the already-painted page
        // under browser overlays. Waiting for another page paint adds latency.
        configuration.setAfterScreenUpdates(false);
        webview.takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &handler);
    }
}

/// Bound transient overlay images to approximately one 1080p frame. A
/// Retina or very large window otherwise multiplies JPEG work and GPU memory.
fn snapshot_width(width: f64, height: f64, scale: f64) -> Option<f64> {
    if !width.is_finite()
        || !height.is_finite()
        || !scale.is_finite()
        || width <= 0.0
        || height <= 0.0
        || scale <= 0.0
    {
        return None;
    }
    let pixels_wide = width * scale;
    let pixels_high = height * scale;
    let ratio = (2048.0 / pixels_wide.max(pixels_high))
        .min((2_097_152.0 / (pixels_wide * pixels_high)).sqrt())
        .min(1.0);
    Some(width * ratio)
}

#[cfg(test)]
mod snapshot_tests {
    use super::snapshot_width;

    #[test]
    fn bounds_retina_and_large_overlay_images_without_upscaling() {
        for (width, height, scale) in [
            (900.0, 600.0, 1.0),
            (900.0, 600.0, 2.0),
            (2560.0, 1440.0, 2.0),
            (900.0, 2400.0, 2.0),
        ] {
            let snapshot = snapshot_width(width, height, scale).unwrap();
            let pixels_wide = snapshot * scale;
            let pixels_high = pixels_wide * height / width;
            assert!(snapshot <= width);
            assert!(pixels_wide.max(pixels_high) <= 2048.0001);
            assert!(pixels_wide * pixels_high <= 2_097_152.001);
        }
        assert_eq!(snapshot_width(900.0, 600.0, 1.0), Some(900.0));
        assert_eq!(snapshot_width(800.0, 600.0, 2.0), Some(800.0));
        assert_eq!(snapshot_width(0.0, 600.0, 2.0), None);
        assert_eq!(snapshot_width(900.0, f64::NAN, 2.0), None);
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
