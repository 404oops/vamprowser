//! macOS permission alerts, WebKit page snapshots, and the local clock.

use std::{
    cell::Cell,
    sync::{Arc, Mutex},
};

use block2::RcBlock;
use gpui::RenderImage;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSBitmapImageRep, NSCompositingOperation,
    NSDeviceRGBColorSpace, NSGraphicsContext, NSImage,
};
use objc2_foundation::{
    NSCalendar, NSCalendarUnit, NSDate, NSDateFormatter, NSDateFormatterStyle, NSError, NSNumber,
    NSPoint, NSRect, NSSize, NSString,
};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};


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

/// A page's still held by the browser. Dropping it (closing what it was
/// behind) gives its texture back on the next frame drawn: see
/// [`release_stale`]. Not cloned, so a frame drawing it can't let it go.
pub struct Still(Arc<RenderImage>);

impl Still {
    pub fn new(image: Arc<RenderImage>) -> Self {
        Self(image)
    }

    /// The image, to draw.
    pub fn image(&self) -> Arc<RenderImage> {
        self.0.clone()
    }
}

impl Drop for Still {
    fn drop(&mut self) {
        if let Ok(mut stale) = STALE.lock() {
            stale.push(self.0.clone());
        }
    }
}

/// Stills let go of, until a window frees their textures.
static STALE: Mutex<Vec<Arc<RenderImage>>> = Mutex::new(Vec::new());

/// Frees the textures of stills no longer held. Called as a window draws,
/// since only then is there a window to free them from.
pub fn release_stale(window: &mut gpui::Window, cx: &mut gpui::App) {
    let stale: Vec<_> = match STALE.lock() {
        Ok(mut stale) if !stale.is_empty() => stale.drain(..).collect(),
        _ => return,
    };
    for image in stale {
        cx.drop_image(image, Some(window));
    }
}

/// Asks WebKit for a still of the page, delivered ready to draw, on a
/// thread of its own; `None` if it couldn't. The pixels are copied out on
/// the main thread (the image is WebKit's), but nothing is encoded or
/// decoded: the still is drawn as soon as it arrives, and the main thread
/// gets on with the overlay's opening frames.
pub fn snapshot(
    webview: &WKWebView,
    done: impl FnOnce(Option<Arc<RenderImage>>) + Send + 'static,
) {
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
        match unsafe { image.as_ref() }.and_then(|image| rgba_pixels(image, None)) {
            Some((width, height, pixels)) => {
                std::thread::spawn(move || done(render_image(width, height, pixels)));
            }
            None => done(None),
        }
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

/// A small image (a file icon) drawn `pixels` square, ready to draw, made
/// here and now: a few kilobytes, so not worth a thread.
pub fn render_image_of(image: &NSImage, pixels: u32) -> Option<Arc<RenderImage>> {
    let (width, height, data) = rgba_pixels(image, Some((pixels as isize, pixels as isize)))?;
    render_image(width, height, data)
}

/// The image's pixels as tightly packed 8-bit RGBA, premultiplied, drawn
/// into a bitmap of that one known layout (at the image's own pixel size,
/// or `size`): the image's own layout can be any of several.
fn rgba_pixels(image: &NSImage, size: Option<(isize, isize)>) -> Option<(u32, u32, Vec<u8>)> {
    let (width, height) = match size {
        Some(size) => size,
        None => image
            .representations()
            .iter()
            .map(|rep| (rep.pixelsWide(), rep.pixelsHigh()))
            .max()?,
    };
    if width <= 0 || height <= 0 || width > 1 << 14 || height > 1 << 14 {
        return None;
    }
    // SAFETY: null planes ask AppKit to allocate the bitmap; the rest
    // describes 8-bit RGBA, which it lays out as such.
    let target = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width,
            height,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    if target.bitsPerPixel() != 32 || target.samplesPerPixel() != 4 {
        return None;
    }
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&target)?;
    let whole = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(width as f64, height as f64),
    );
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    // The bitmap's units are its pixels: the image fills it pixel for
    // pixel, from whichever of its representations fits best.
    // SAFETY: drawing into the current context, which is the bitmap's.
    unsafe {
        image.drawInRect_fromRect_operation_fraction(
            whole,
            NSRect::ZERO,
            NSCompositingOperation::Copy,
            1.0,
        )
    };
    NSGraphicsContext::restoreGraphicsState_class();
    let bytes_per_row = usize::try_from(target.bytesPerRow()).ok()?;
    let (width, height) = (width as usize, height as usize);
    let data = target.bitmapData();
    if data.is_null() || bytes_per_row < width * 4 {
        return None;
    }
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        // SAFETY: the bitmap is `height` rows of `bytes_per_row` bytes,
        // each holding `width` 4-byte pixels, and lives as long as `target`.
        let line = unsafe { std::slice::from_raw_parts(data.add(row * bytes_per_row), width * 4) };
        pixels.extend_from_slice(line);
    }
    Some((width as u32, height as u32, pixels))
}

/// A GPUI image from RGBA pixels: it draws BGRA.
fn render_image(width: u32, height: u32, mut pixels: Vec<u8>) -> Option<Arc<RenderImage>> {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, pixels)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
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
