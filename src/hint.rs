//! Hints: a button's name (and shortcut), or a tab's full title in the
//! icon rail. Drawn by AppKit in a little window of their own, because GPUI
//! can't draw over a page, which would cut them off; and set beside or
//! below what they name, never under the pointer.

use std::{cell::RefCell, collections::HashMap, time::Duration};

use gpui::{Bounds, Context, IntoElement, Pixels, Styled, Window, canvas};
use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont, NSPanel, NSTextField, NSTitlePosition,
    NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use vampir::Palette;

use crate::Browser;

/// How long the pointer rests on something before its hint shows.
const DELAY: Duration = Duration::from_millis(450);

/// Where a hint goes relative to what it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Below,
    Right,
}

/// The hint's window, its rounded frame and its text, made once.
type HintPanel = (Retained<NSPanel>, Retained<NSBox>, Retained<NSTextField>);

thread_local! {
    /// Where each hinted thing was drawn, window-relative, by key (which
    /// names the window).
    static ANCHORS: RefCell<HashMap<String, Bounds<Pixels>>> = RefCell::default();
    static PANEL: RefCell<Option<HintPanel>> = const { RefCell::new(None) };
    /// A passing message's own window, so a hint doesn't take it away.
    static TOAST: RefCell<Option<HintPanel>> = const { RefCell::new(None) };
}

/// Records where the thing with hint `key` is drawn: add as a child of it.
pub(crate) fn anchor(key: String) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            ANCHORS.with(|anchors| anchors.borrow_mut().insert(key.clone(), bounds));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn ns_color(color: gpui::Rgba) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        f64::from(color.red),
        f64::from(color.green),
        f64::from(color.blue),
        f64::from(color.alpha),
    )
}

/// Shows `text` beside `anchor` (window-relative, top-left origin) in the
/// window at `ns_window`.
fn show(ns_window: usize, text: &str, anchor: Bounds<Pixels>, side: Side, palette: Palette) {
    show_in(&PANEL, ns_window, text, anchor, side, palette);
}

/// Shows a passing message under `anchor` (the address field) for a few
/// seconds: for what an action says when the settings page, where such
/// messages sit, isn't showing.
pub(crate) fn toast(ns_window: usize, text: &str, anchor: Bounds<Pixels>, palette: Palette) {
    show_in(&TOAST, ns_window, text, anchor, Side::Below, palette);
}

/// Takes the passing message away.
pub(crate) fn hide_toast() {
    TOAST.with(|panel| {
        if let Some((toast, _, _)) = panel.borrow().as_ref()
            && toast.isVisible()
        {
            toast.orderOut(None);
        }
    });
}

fn show_in(
    slot: &'static std::thread::LocalKey<RefCell<Option<HintPanel>>>,
    ns_window: usize,
    text: &str,
    anchor: Bounds<Pixels>,
    side: Side,
    palette: Palette,
) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if ns_window == 0 {
        return;
    }
    // SAFETY: the browser's own window, alive while it is.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    slot.with(|panel| {
        let mut panel = panel.borrow_mut();
        let (hint, frame_box, label) = panel.get_or_insert_with(|| {
            let hint = NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0)),
                NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                NSBackingStoreType::Buffered,
                false,
            );
            hint.setOpaque(false);
            hint.setBackgroundColor(Some(&NSColor::clearColor()));
            hint.setHasShadow(true);
            hint.setIgnoresMouseEvents(true);
            // SAFETY: plain property setter on a panel we own.
            unsafe { hint.setReleasedWhenClosed(false) };
            let frame_box = NSBox::new(mtm);
            frame_box.setBoxType(NSBoxType::Custom);
            frame_box.setTitlePosition(NSTitlePosition::NoTitle);
            frame_box.setCornerRadius(6.0);
            frame_box.setBorderWidth(0.5);
            frame_box.setContentViewMargins(NSSize::new(0.0, 0.0));
            let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
            label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            frame_box.addSubview(&label);
            hint.setContentView(Some(&frame_box));
            (hint, frame_box, label)
        });
        frame_box.setFillColor(&ns_color(crate::Chrome::new(palette).raised));
        frame_box.setBorderColor(&ns_color(palette.field_border_strong));
        label.setTextColor(Some(&ns_color(palette.text_primary)));
        label.setStringValue(&NSString::from_str(text));
        label.sizeToFit();
        let size = label.frame().size;
        let (pad_x, pad_y) = (8.0, 4.0);
        let (width, height) = (size.width + pad_x * 2.0, size.height + pad_y * 2.0);
        label.setFrameOrigin(NSPoint::new(pad_x, pad_y));
        // Window-relative, top-left, to the screen's bottom-left.
        let frame = window.frame();
        let (ax, ay) = (f64::from(f32::from(anchor.origin.x)), f64::from(f32::from(anchor.origin.y)));
        let (aw, ah) = (
            f64::from(f32::from(anchor.size.width)),
            f64::from(f32::from(anchor.size.height)),
        );
        let top = frame.origin.y + frame.size.height - ay;
        let (mut x, y) = match side {
            Side::Below => (frame.origin.x + ax + aw / 2.0 - width / 2.0, top - ah - 6.0 - height),
            Side::Right => (frame.origin.x + ax + aw + 8.0, top - ah / 2.0 - height / 2.0),
        };
        // Kept on the screen.
        if let Some(screen) = window.screen() {
            let visible = screen.visibleFrame();
            x = x.clamp(visible.origin.x + 4.0, visible.origin.x + visible.size.width - width - 4.0);
        }
        hint.setFrame_display(NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)), true);
        if hint.parentWindow().is_none_or(|parent| !std::ptr::eq(&*parent, window)) {
            if let Some(parent) = hint.parentWindow() {
                parent.removeChildWindow(hint);
            }
            // SAFETY: both windows are live; the panel floats above its parent.
            unsafe { window.addChildWindow_ordered(hint, NSWindowOrderingMode::Above) };
        }
        hint.orderFront(None);
    });
}

/// Hides the hint, if one is showing.
pub(crate) fn hide() {
    PANEL.with(|panel| {
        if let Some((hint, _, _)) = panel.borrow().as_ref()
            && hint.isVisible()
        {
            hint.orderOut(None);
        }
    });
}

impl Browser {
    /// The pointer came onto (or left) something with a hint: shows it after
    /// a moment's rest, or takes it away.
    pub(crate) fn hover_hint(
        &mut self,
        key: String,
        text: String,
        side: Side,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !hovered {
            if self.hint_key.as_deref() == Some(key.as_str()) {
                self.hint_key = None;
                self._hint_later = None;
                hide();
            }
            return;
        }
        self.hint_key = Some(key.clone());
        let ns_window = self.ns_window;
        let palette = self.controls.palette();
        self._hint_later = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DELAY).await;
            let _ = this.update(cx, |browser, _| {
                if browser.hint_key.as_deref() != Some(key.as_str()) {
                    return;
                }
                if let Some(bounds) = ANCHORS.with(|anchors| anchors.borrow().get(&key).copied()) {
                    show(ns_window, &text, bounds, side, palette);
                }
            });
        }));
    }

    /// A click, a key or leaving the window: no hint.
    pub(crate) fn dismiss_hint(&mut self) {
        self.hint_key = None;
        self._hint_later = None;
        hide();
    }
}
