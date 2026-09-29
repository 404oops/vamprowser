//! Find in page (⌘F): a bar above the page with a field, how many matches
//! there are, and buttons for the next and previous. WebKit finds and
//! highlights; Enter and ⌘G go forward, ⇧Enter and ⇧⌘G back, Escape
//! closes it.

use std::time::Duration;

use block2::RcBlock;
use gpui::{AnyElement, Context, Window, div, prelude::*, px};
use objc2_foundation::NSString;
use objc2_web_kit::{WKFindConfiguration, WKFindResult};
use vampir::{Palette, color};
use wry::WebViewExtMacOS;

use crate::{
    Browser, BrowserEvent, Chrome, Page,
    icons::Icon,
    tool_button,
};

/// The bar's height.
pub(crate) const FIND_HEIGHT: f32 = 40.0;
/// How quickly it opens and closes.
pub(crate) const FIND_MOVE: Duration = Duration::from_millis(160);

/// Counts the page's matches for a query, as `find` can't.
const COUNT_SCRIPT: &str = "(q => { const t = (document.body && document.body.innerText || '').toLocaleLowerCase(); \
    let n = 0, i = 0; while ((i = t.indexOf(q, i)) !== -1) { n++; i += q.length; } return n; })";

/// A DOM change costs almost nothing until it settles. During continuous
/// updates (including generated text), report at most once every 1.5 seconds.
const WATCH_SCRIPT: &str = r#"(serial => {
    window.__vamprowserFindWatch?.stop();
    if (!document.body) return;
    let timer;
    const observer = new MutationObserver(() => {
        if (timer) return;
        timer = setTimeout(() => {
            timer = undefined;
            window.ipc.postMessage('find-dirty:' + serial);
        }, 1500);
    });
    observer.observe(document.body, {subtree: true, childList: true, characterData: true});
    window.__vamprowserFindWatch = {
        stop() { observer.disconnect(); clearTimeout(timer); }
    };
})"#;
const STOP_WATCH_SCRIPT: &str = "window.__vamprowserFindWatch?.stop(); window.__vamprowserFindWatch = undefined";

/// What the bar knows of the last search: which tab and query it was for,
/// numbered so a late answer for an earlier one is ignored.
#[derive(Default)]
pub(crate) struct FindState {
    pub open: bool,
    pub serial: u64,
    /// Whether the last step found anything, once WebKit says.
    pub found: Option<bool>,
    pub count: Option<usize>,
    /// The tab with an active DOM observer, so switching tabs stops it.
    pub watching: Option<u64>,
}

impl Browser {
    /// Opens the bar, or selects its text if it's open, and searches for
    /// what's there.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current().page != Page::Web {
            return;
        }
        self.close_palette(cx);
        self.find.open = true;
        crate::keyboard_to_gpui(self.ns_window, self.ns_view);
        let focus = self.find_input.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        cx.on_next_frame(window, move |_, window, cx| {
            focus.dispatch_action(&vampir::text_input::SelectAll, window, cx);
        });
        self.sync_key_flags(window, cx);
        self.find_step(false, cx);
        cx.notify();
    }

    /// Closes the bar and gives the page the keyboard back.
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        self.find.open = false;
        self.find.found = None;
        self.find.count = None;
        self.stop_find_watch();
        window.blur();
        if let Some(view) = self.current().view.clone()
            && self.current().page == Page::Web
        {
            let _ = view.focus();
        }
        self.sync_key_flags(window, cx);
        cx.notify();
    }

    /// Whether the bar's field has the keyboard.
    pub(crate) fn find_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.find.open && self.find_input.read(cx).focus_handle.is_focused(window)
    }

    /// Finds the next match (or the previous, `backwards`) of what's in
    /// the field; the answer comes back as a `Found` event.
    pub(crate) fn find_step(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let query = self.find_input.read(cx).content.to_string();
        self.find.serial += 1;
        let serial = self.find.serial;
        let tab = self.current().id;
        self.stop_find_watch();
        if query.is_empty() || self.current().page != Page::Web {
            self.find.found = None;
            self.find.count = None;
            cx.notify();
            return;
        }
        let Some(view) = self.current().view.clone() else {
            return;
        };
        if self.find.open {
            let _ = view.evaluate_script(&format!("{WATCH_SCRIPT}({serial})"));
            self.find.watching = Some(tab);
        }
        self.find_webkit(&query, backwards, tab, serial, &view);
        self.count_find_matches(&query, tab, serial, &view);
    }

    fn stop_find_watch(&mut self) {
        if let Some(tab) = self.find.watching.take()
            && let Some(view) = self.tabs.iter().find(|item| item.id == tab).and_then(|item| item.view.as_ref())
        {
            let _ = view.evaluate_script(STOP_WATCH_SCRIPT);
        }
    }

    fn find_webkit(&self, query: &str, backwards: bool, tab: u64, serial: u64, view: &wry::WebView) {
        let webview = view.webview();
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        // SAFETY: a new configuration, set up on the main thread.
        let configuration = unsafe {
            let configuration = WKFindConfiguration::new(mtm);
            configuration.setBackwards(backwards);
            configuration.setWraps(true);
            configuration.setCaseSensitive(false);
            configuration
        };
        let sender = self.sender.clone();
        let handler = RcBlock::new(move |result: std::ptr::NonNull<WKFindResult>| {
            // SAFETY: WebKit passes a live result for the call's duration.
            let found = unsafe { result.as_ref().matchFound() };
            let _ = sender.try_send(BrowserEvent::Found(tab, serial, Some(found), None));
        });
        // SAFETY: a live web view on the main thread, and a block of the
        // documented type.
        unsafe {
            webview.findString_withConfiguration_completionHandler(
                &NSString::from_str(&query),
                Some(&configuration),
                &handler,
            );
        }
    }

    fn count_find_matches(&self, query: &str, tab: u64, serial: u64, view: &wry::WebView) {
        let lowered = serde_json::to_string(&query.to_lowercase()).unwrap_or_default();
        let sender = self.sender.clone();
        let _ = view.evaluate_script_with_callback(&format!("{COUNT_SCRIPT}({lowered})"), move |result| {
            let count = result.trim().parse::<usize>().ok();
            let _ = sender.try_send(BrowserEvent::Found(tab, serial, None, count));
        });
    }

    /// A page mutation has settled. Recount without moving the current match.
    pub(crate) fn find_dirty(&mut self, tab: u64, serial: u64, cx: &mut Context<Self>) {
        if !self.find.open || self.find.serial != serial || self.current().id != tab {
            return;
        }
        let query = self.find_input.read(cx).content.to_string();
        if let Some(view) = self.current().view.as_ref() {
            self.count_find_matches(&query, tab, serial, view);
        }
    }

    /// An answer from [`Browser::find_step`], if it's for the latest.
    pub(crate) fn found(&mut self, tab: u64, serial: u64, found: Option<bool>, count: Option<usize>, cx: &mut Context<Self>) {
        if serial != self.find.serial || tab != self.current().id || !self.find.open {
            return;
        }
        if found.is_some() {
            self.find.found = found;
        }
        if count.is_some() {
            let newly_present = self.find.count == Some(0) && count.is_some_and(|n| n > 0);
            self.find.count = count;
            if newly_present {
                let query = self.find_input.read(cx).content.to_string();
                if let Some(view) = self.current().view.as_ref() {
                    self.find_webkit(&query, false, tab, serial, view);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn find_bar(&mut self, height: f32, palette: Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let chrome = Chrome::new(palette);
        let has_query = !self.find_input.read(cx).content.is_empty();
        let status = match (has_query, self.find.found, self.find.count) {
            (false, _, _) => String::new(),
            (true, Some(false), _) | (true, _, Some(0)) => "No matches".into(),
            (true, _, Some(1)) => "1 match".into(),
            (true, _, Some(n)) => format!("{n} matches"),
            (true, _, None) => String::new(),
        };
        let missing = has_query && (self.find.found == Some(false) || self.find.count == Some(0));
        div()
            .h(px(height))
            .w_full()
            .flex_none()
            .overflow_hidden()
            .flex()
            .flex_col()
            .justify_end()
            .opacity((height / FIND_HEIGHT).min(1.0))
            .child(
                div()
                    .h(px(FIND_HEIGHT))
                    .w_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .bg(chrome.ground)
                    .border_b_1()
                    .border_color(chrome.line)
                    .child(
                        div()
                            .w(px(280.0))
                            .flex_none()
                            .child(vampir::search_field("find-in-page", &self.find_input, palette, window, cx)),
                    )
                    .child(
                        div()
                            .min_w(px(80.0))
                            .text_size(px(12.0))
                            .text_color(if missing {
                                palette.danger_label
                            } else {
                                color::with_alpha(palette.text_secondary, 0.9)
                            })
                            .child(status),
                    )
                    .child(tool_button(
                        "find-previous",
                        Icon::ChevronLeft,
                        vampir::Hint::new("Previous match").shortcut("⇧⌘G"),
                        28.0,
                        false,
                        palette,
                        cx,
                        |this, _, cx| this.find_step(true, cx),
                    ))
                    .child(tool_button(
                        "find-next",
                        Icon::ChevronRight,
                        vampir::Hint::new("Next match").shortcut("⌘G"),
                        28.0,
                        false,
                        palette,
                        cx,
                        |this, _, cx| this.find_step(false, cx),
                    ))
                    .child(div().flex_1())
                    .child(tool_button(
                        "find-done",
                        Icon::Close,
                        vampir::Hint::new("Done").shortcut("Esc"),
                        28.0,
                        false,
                        palette,
                        cx,
                        |this, window, cx| this.close_find(window, cx),
                    )),
            )
            .into_any_element()
    }
}
