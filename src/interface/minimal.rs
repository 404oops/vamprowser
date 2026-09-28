//! Minimal mode (⌘⇧M): only the page, under a hairline bar. Pointing at
//! the top of the window brings the browser back, sliding in over the page
//! without it reflowing, and it slides away again when the pointer leaves.

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Window, div, prelude::*, px};
use objc2_app_kit::{NSWindow, NSWindowButton, NSWindowStyleMask};
use objc2_foundation::NSPoint;
use vampir::{Palette, color};

use crate::{BOOKMARKS_HEIGHT, Browser, Chrome, TAB_STRIP_HEIGHT, TOOLBAR_HEIGHT, TRAFFIC_LIGHTS, tabdrag};

/// The bar left at the top of the window.
pub(crate) const MINIMAL_BAR: f32 = 8.0;
/// How quickly the browser slides in and out.
pub(crate) const REVEAL: Duration = Duration::from_millis(180);
/// How long the pointer can be away before the browser slides out.
const LINGER: Duration = Duration::from_millis(350);
/// How often the pointer is checked while in minimal mode.
const WATCH: Duration = Duration::from_millis(50);

/// Puts the traffic lights back where the window has them, in the toolbar:
/// AppKit resets them to its own place while they're hidden, and GPUI only
/// lays them out again when the window changes. The same layout GPUI does.
fn place_traffic_lights(window: &NSWindow) {
    if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        return;
    }
    let (Some(close), Some(minimize), Some(zoom)) = (
        window.standardWindowButton(NSWindowButton::CloseButton),
        window.standardWindowButton(NSWindowButton::MiniaturizeButton),
        window.standardWindowButton(NSWindowButton::ZoomButton),
    ) else {
        return;
    };
    let (x, y) = (f64::from(TRAFFIC_LIGHTS.0), f64::from(TRAFFIC_LIGHTS.1));
    let frame = close.frame();
    // Already in place: most frames.
    if (frame.origin.x - x).abs() < 0.5 && (frame.origin.y - y).abs() < 0.5 {
        return;
    }
    // SAFETY: the buttons' superviews, live on the main thread.
    let Some(container) = (unsafe { close.superview().and_then(|v| v.superview()) }) else {
        return;
    };
    let spacing = minimize.frame().origin.x - frame.origin.x;
    let height = frame.size.height + y * 2.0;
    let mut titlebar = container.frame();
    titlebar.size.height = height;
    titlebar.origin.y = window.frame().size.height - height;
    container.setFrame(titlebar);
    close.setFrameOrigin(NSPoint::new(x, y));
    minimize.setFrameOrigin(NSPoint::new(x + spacing, y));
    zoom.setFrameOrigin(NSPoint::new(x + spacing * 2.0, y));
}

impl Browser {
    pub(crate) fn toggle_minimal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.compact {
            return;
        }
        self.minimal = !self.minimal;
        self.chrome_revealed = false;
        self.pointer_left = None;
        if self.minimal {
            // The pointer is watched, rather than followed through hover,
            // because once it's over the page, WebKit has it, not GPUI.
            self._minimal_watch = Some(cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(WATCH).await;
                    let alive = this
                        .update_in(cx, |browser, window, cx| browser.watch_pointer(window, cx))
                        .unwrap_or(false);
                    if !alive {
                        break;
                    }
                }
            }));
        } else {
            self._minimal_watch = None;
        }
        cx.notify();
    }

    /// How far the chrome shows: 1 normally, 0 folded away in minimal mode.
    pub(crate) fn chrome_reveal(&self) -> f32 {
        let target = if !self.minimal || self.chrome_revealed { 1.0 } else { 0.0 };
        self.controls.tween("chrome-reveal", target, REVEAL)
    }

    /// Whether something needs the browser shown: typing in it, the tab
    /// switcher, suggestions, a tab being dragged.
    fn chrome_in_use(&self, window: &Window, cx: &Context<Self>) -> bool {
        self.text_field_focused(window, cx)
            || self.palette.is_some()
            || self.suggestions_open()
            || self.tab_drag.is_some()
    }

    /// Reveals the browser while the pointer is at the top of the window
    /// (or over the browser, once shown), and folds it away shortly after
    /// it leaves. Returns false once minimal mode is off.
    fn watch_pointer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.minimal {
            return false;
        }
        let reach = if self.chrome_revealed {
            let mut height = TOOLBAR_HEIGHT + 14.0;
            if self.bookmarks_bar {
                height += BOOKMARKS_HEIGHT;
            }
            if !self.vertical_tabs {
                height += TAB_STRIP_HEIGHT;
            }
            height
        } else {
            MINIMAL_BAR + 4.0
        };
        let sidebar = if self.vertical_tabs && self.chrome_revealed {
            if self.compact_vertical_tabs {
                crate::RAIL_WIDTH + 14.0
            } else {
                crate::SIDEBAR_WIDTH + 14.0
            }
        } else {
            0.0
        };
        let pointed = tabdrag::pointer_in(self.ns_window)
            .is_some_and(|at| f32::from(at.y) <= reach || f32::from(at.x) <= sidebar);
        let wanted = pointed || self.chrome_in_use(window, cx);
        if wanted {
            self.pointer_left = None;
            if !self.chrome_revealed {
                self.chrome_revealed = true;
                cx.notify();
            }
        } else if self.chrome_revealed {
            let since = *self.pointer_left.get_or_insert_with(Instant::now);
            if since.elapsed() >= LINGER {
                self.chrome_revealed = false;
                self.pointer_left = None;
                cx.notify();
            }
        }
        true
    }

    /// Fades the close, minimise and zoom buttons with the browser as it
    /// slides in and out (`reveal`, 0 to 1), so they neither pop up over
    /// the page ahead of the toolbar nor vanish from it before it's gone.
    /// Called as each frame is drawn; checks the buttons themselves, which
    /// GPUI lays out again when the window changes, and only changes them
    /// when they differ.
    pub(crate) fn fade_traffic_lights(&self, reveal: f32) {
        if self.ns_window == 0 {
            return;
        }
        // Eased so they arrive with the toolbar rather than ahead of it.
        let alpha = f64::from(reveal * reveal);
        let hidden = alpha < 0.02;
        // SAFETY: this window's own NSWindow, alive while the browser is.
        let window: &NSWindow = unsafe { &*(self.ns_window as *const NSWindow) };
        for kind in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            let Some(button) = window.standardWindowButton(kind) else {
                continue;
            };
            if (button.alphaValue() - alpha).abs() > 0.01 {
                button.setAlphaValue(alpha);
            }
            // Hidden once faded, so a click can't land on one unseen.
            if button.isHidden() != hidden {
                button.setHidden(hidden);
            }
        }
        if !hidden {
            place_traffic_lights(window);
        }
    }

    /// The hairline along the top in minimal mode, in the page's hue, with
    /// a grip in the middle. It slides up out of the way as the browser
    /// comes in, on hover or leaving minimal mode, and back down as it goes.
    pub(crate) fn minimal_bar(&self, palette: Palette) -> Option<AnyElement> {
        let target = if self.minimal && !self.chrome_revealed { MINIMAL_BAR } else { 0.0 };
        let height = self.controls.tween("minimal-bar", target, REVEAL);
        if height < 0.25 {
            return None;
        }
        let chrome = Chrome::new(palette);
        Some(
            div()
                .id("minimal-bar")
                .h(px(height))
                .w_full()
                .flex_none()
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center()
                .bg(chrome.ground)
                .child(
                    div()
                        .w(px(46.0))
                        .h(px(3.0))
                        .rounded(px(2.0))
                        .bg(color::with_alpha(palette.text_secondary, 0.45)),
                )
                .into_any_element(),
        )
    }
}
