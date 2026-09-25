#![cfg(target_os = "macos")]

mod bookmarks;
mod app_dialog;
mod app_menu;
mod audio;
mod bookmarks_view;
mod bookmark_menu;
mod cache;
mod commands;
mod downloads;
mod extensions;
mod favicon;
mod find;
mod filters;
mod history;
mod http_auth;
mod icons;
mod interop;
mod legacy_http;
mod native;
mod navigation;
mod pages;
mod page_cursor;
mod pointer_lock;
mod palette;
mod privacy;
mod reader;
mod settings;
mod sitedata;
mod state;
mod suggest;
mod tabdrag;
mod minimal;
mod marquee;
mod updates;
mod handoff;
mod hint;
mod tint;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::PathBuf,
    ptr::NonNull,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use async_channel::{Receiver, Sender};
use block2::RcBlock;
use commands::{Command, Place, Section};
use downloads::Downloads;
use extensions::{ExtensionEvent, Extensions};
use favicon::{Favicon, Favicons};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Bounds, Context, Div, ElementId, Entity, FontWeight,
    KeyBinding, KeyDownEvent, Menu, MenuItem, MouseButton, MouseDownEvent, Rgba, ScrollHandle,
    Stateful, SystemMenuType, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, actions,
    canvas, div, img, linear_color_stop, linear_gradient, point, prelude::*, px, size,
};
use gpui_ce_platform::application;
use history::History;
use icons::{Icon, icon};
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};
use objc2_web_kit::WKFullscreenState;
use privacy::ContentRules;
use settings::{
    NewTabPage, PopupPolicy, SchemeChoice, Settings, SitePermission, Startup, TabPlacement,
    TintMode, ToolbarItem,
};
use bookmarks::Bookmarks;
use state::SavedState;
use tint::Tint;
use vampir::{
    ControlHost, ControlState, Hint, InputStyle, Key, MOVE, Palette, SWITCH_SLIDE, ScrollAxis,
    TextInput, bind_keys, color, lighting, ui_font,
};
use wry::{
    NewWindowResponse, PageLoadEvent, PermissionKind, PermissionResponse, Rect, WebView,
    WebViewBuilder, WebViewBuilderExtMacos, WebViewExtMacOS, dpi,
};

#[derive(Debug)]
enum BrowserEvent {
    Address(String),
    Title(u64, String),
    Loaded(u64, String),
    /// A same-document navigation changed the address without a page load.
    UrlChanged(u64),
    /// A page asked for a new window.
    Popup(u64, String),
    /// A link clicked with ⌘ or the middle button, for a new tab: in front
    /// if ⇧ was held too.
    LinkInNewTab(u64, String, bool),
    /// A plain-HTTP link HTTPS-only mode stopped, to load over HTTPS instead.
    Upgrade(u64, String),
    /// The colour a site's page asked for, or `None` if it has none.
    Tint(String, Option<Tint>),
    /// The icon links a tab's page reported, as JSON.
    Icons(u64, String),
    /// A site's icon arrived from the network, as PNG, or it has none.
    Favicon(String, Option<Vec<u8>>),
    /// A download began, from tab (first).
    DownloadStarted(String, PathBuf, bool),
    DownloadFinished(String, Option<PathBuf>, bool),
    /// A still of the page, for behind the tab switcher.
    /// Each still says which tab it's of.
    Snapshot(u64, Option<Vec<u8>>),
    Extension(ExtensionEvent),
    /// uBlock Origin's network filters, compiled for WebKit.
    /// uBlock Origin's rules, from the build numbered here: WebKit rule
    /// lists, their fingerprints, and how many rules they hold.
    UblockRules(u64, Vec<String>, Vec<String>, usize),
    /// A click landed in a page.
    PageClicked,
    /// A page began loading.
    LoadStarted(u64),
    /// A page couldn't load.
    LoadFailed(u64, navigation::LoadFailure),
    AuthChallenge(u64, navigation::AuthChallenge),
    AuthSubmitted(u64, String, String),
    /// A page's text field took the keyboard, or gave it up.
    PageEditing(u64, bool),
    /// Enter pressed in a settings text field.
    SaveField(pages::Field),
    /// An add-on fetched from addons.mozilla.org, ready to install.
    ExtensionPrepared(Result<extensions::Prepared, String>),
    /// A still of the page, for behind a bookmarks folder's menu.
    /// A background tab went unused long enough to unload.
    SleepTab(u64),
    /// A check for extension updates finished.
    ExtensionUpdates(updates::UpdateReport),
    Command(Command),
    /// The address field's text, as typed (not as we set it).
    AddressEdited(String),
    /// The search engine's suggestions for what was typed.
    Suggestions(String, Vec<String>),
    SuggestSnapshot(u64, Option<Vec<u8>>),
    /// Something to say on the settings pages.
    Notice(String),
    /// What's in the find bar changed.
    FindEdited,
    /// A search in tab (first) numbered (second): whether it found a
    /// match, or how many there are.
    Found(u64, u64, Option<bool>, Option<usize>),
    /// The profile exported to an archive, or why not.
    Exported(Result<PathBuf, String>),
    /// An archive unpacked to import at the next launch, or why not.
    ImportStaged(Result<(), String>),
    /// A profile archive chosen in the system file picker, before approval.
    ImportChosen(PathBuf),
}

impl BrowserEvent {
    /// The tab whose page this event is about, for events that follow a
    /// tab to whichever window holds it.
    fn page_tab(&self) -> Option<u64> {
        match self {
            BrowserEvent::Title(id, _)
            | BrowserEvent::Loaded(id, _)
            | BrowserEvent::UrlChanged(id)
            | BrowserEvent::LoadStarted(id)
            | BrowserEvent::PageEditing(id, _)
            | BrowserEvent::Popup(id, _)
            | BrowserEvent::LinkInNewTab(id, ..)
            | BrowserEvent::Upgrade(id, _)
            | BrowserEvent::Icons(id, _)
            | BrowserEvent::SleepTab(id)
            | BrowserEvent::LoadFailed(id, _) => Some(*id),
            BrowserEvent::AuthChallenge(id, _) | BrowserEvent::AuthSubmitted(id, ..) => Some(*id),
            _ => None,
        }
    }
}

/// The browser's keyboard shortcuts, caught before WebKit sees them.
/// Tells the browser when a page's text field (or a frame, which may hold
/// one) takes the keyboard or gives it up, so ⌘← and ⌘→ move its caret
/// instead of leaving the page.
const EDITING_SCRIPT: &str = r#"
(() => {
  const textual = /^(text|search|url|tel|email|password|number|date|datetime-local|month|time|week)$/;
  const editing = () => {
    let element = document.activeElement;
    while (element && element.shadowRoot && element.shadowRoot.activeElement) {
      element = element.shadowRoot.activeElement;
    }
    if (!element) return false;
    if (element.isContentEditable || /^(TEXTAREA|IFRAME|FRAME)$/.test(element.tagName)) return true;
    return element.tagName === 'INPUT' && textual.test(element.type);
  };
  let last = false;
  const report = () => {
    const now = editing();
    if (now !== last) {
      last = now;
      window.ipc.postMessage(now ? 'editing:1' : 'editing:0');
    }
  };
  addEventListener('focusin', report, true);
  addEventListener('focusout', () => setTimeout(report, 0), true);
})();
"#;

/// WebKit's page-load callback misses History API and fragment navigation.
/// Ask the native view for its URL after those changes, so pages cannot
/// supply an address different from the one WebKit is showing.
const URL_CHANGE_SCRIPT: &str = r#"
(() => {
  if (window !== window.top) return;
  const report = () => window.ipc.postMessage('url-changed');
  for (const name of ['pushState', 'replaceState']) {
    const original = history[name];
    history[name] = function (...args) {
      const result = original.apply(this, args);
      report();
      return result;
    };
  }
  addEventListener('popstate', report);
  addEventListener('hashchange', report);
})();
"#;

/// X's Draft editor inserts the first character itself, then WebKit sends
/// another native insertText for the same key. Only suppress that second
/// insertion when the editor was empty at keydown and already contains the
/// exact character by beforeinput.
const X_EDITOR_SCRIPT: &str = r#"
(() => {
  if (!['x.com', 'www.x.com', 'twitter.com', 'www.twitter.com'].includes(location.hostname)) return;
  const editor = target => target instanceof Element
    ? target.closest('[data-testid="tweetTextarea_0"]') : null;
  const text = element => element.innerText.replace(/\n/g, '');
  let pending = null;
  document.addEventListener('keydown', event => {
    const field = editor(event.target);
    pending = field && !event.isComposing && !event.metaKey && !event.ctrlKey
      && !event.altKey && event.key.length === 1 && text(field) === ''
      ? { field, key: event.key } : null;
  }, true);
  document.addEventListener('beforeinput', event => {
    const first = pending;
    pending = null;
    if (first && event.inputType === 'insertText' && event.data === first.key
        && editor(event.target) === first.field && text(first.field) === first.key) {
      event.preventDefault();
    }
  }, true);
})();
"#;

/// What the browser's shortcuts depend on besides the key.
#[derive(Clone, Copy, Default)]
struct KeyState {
    palette_open: bool,
    suggesting: bool,
    /// A history row picked with the arrow keys, which ⇧⌫ removes.
    removable: bool,
    menu_open: bool,
    finding: bool,
    /// Text is being edited, in one of our fields or a page's: ⌘← and ⌘→
    /// move the caret there rather than going back and forward.
    editing: bool,
}

fn shortcut(key: &str, code: u16, flags: NSEventModifierFlags, state: KeyState) -> Option<Command> {
    let KeyState {
        palette_open,
        suggesting,
        removable,
        menu_open,
        finding,
        editing,
    } = state;
    let command = flags.contains(NSEventModifierFlags::Command);
    let control = flags.contains(NSEventModifierFlags::Control);
    let option = flags.contains(NSEventModifierFlags::Option);
    let shift = flags.contains(NSEventModifierFlags::Shift);
    if palette_open && !command && !control {
        match code {
            53 => return Some(Command::ClosePalette),
            125 => return Some(Command::PaletteMove(1)),
            126 => return Some(Command::PaletteMove(-1)),
            36 | 76 => return Some(Command::PaletteChoose),
            _ => {}
        }
    }
    if menu_open && code == 53 {
        return Some(Command::CloseBookmarkMenu);
    }
    if finding && !command && !control {
        match code {
            53 => return Some(Command::CloseFind),
            36 | 76 if shift => return Some(Command::FindPrevious),
            _ => {}
        }
    }
    if suggesting && !command && !control {
        match code {
            // ⇧⌫ takes the page picked out of history; otherwise it's a
            // Delete like any other.
            51 | 117 if removable && flags.contains(NSEventModifierFlags::Shift) => {
                return Some(Command::RemoveSuggestion);
            }
            53 => return Some(Command::DismissSuggestions),
            125 => return Some(Command::SuggestMove(1)),
            126 => return Some(Command::SuggestMove(-1)),
            _ => {}
        }
    }
    if control && !command && !option && code == 48 {
        return Some(if shift {
            Command::PreviousTab
        } else {
            Command::NextTab
        });
    }
    if !command || control {
        return None;
    }
    if !option && !shift && !editing {
        match code {
            123 => return Some(Command::Back),
            124 => return Some(Command::Forward),
            _ => {}
        }
    }
    Some(match (key, shift, option) {
        ("t", false, false) => Command::NewTab,
        ("n", false, false) => Command::NewWindow,
        ("n", true, false) => Command::NewPrivateWindow,
        ("w", true, false) => Command::CloseWindow,
        ("t", true, false) => Command::ReopenClosedTab,
        ("w", false, false) => Command::CloseTab,
        ("l", false, false) => Command::FocusAddress,
        ("[", false, false) => Command::Back,
        ("]", false, false) => Command::Forward,
        ("}", true, false) | ("]", true, false) => Command::NextTab,
        ("{", true, false) | ("[", true, false) => Command::PreviousTab,
        ("r", false, false) => Command::Reload,
        ("r", true, false) => Command::EraseCacheAndReload,
        ("r", false, true) => Command::ToggleReaderMode,
        ("d", false, false) => Command::BookmarkPage,
        ("c", true, false) => Command::CopyLink,
        ("b", true, false) => Command::ToggleBookmarksBar,
        ("l", true, false) => Command::ToggleVerticalTabs,
        ("m", true, false) => Command::ToggleMinimalMode,
        ("b", false, true) => Command::ShowBookmarks,
        ("k", false, false) => Command::SwitchTabs,
        (",", false, false) => Command::Settings(Section::General),
        ("y", false, false) => Command::Settings(Section::History),
        ("l", false, true) => Command::ShowDownloads,
        ("h", true, false) => Command::Home,
        ("p", false, false) => Command::Print,
        ("f", false, false) => Command::Find,
        ("g", false, false) => Command::FindNext,
        ("g", true, false) => Command::FindPrevious,
        ("m", false, false) => Command::Minimize,
        (".", false, false) => Command::Stop,
        ("i", false, true) => Command::ShowWebInspector,
        ("=" | "+", _, false) => Command::ZoomIn,
        ("-", false, false) => Command::ZoomOut,
        ("0", false, false) => Command::ZoomReset,
        ("9", false, false) => Command::SelectLastTab,
        (digit, false, false) if digit.len() == 1 && ("1"..="8").contains(&digit) => {
            Command::SelectTab(digit.parse::<usize>().ok()? - 1)
        }
        _ => return None,
    })
}

/// The AppKit window behind a GPUI window, and GPUI's own view in it, as
/// addresses: the window to tell events apart by, the view to give the
/// keyboard to.
fn ns_window_of(window: &Window) -> (usize, usize) {
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return (0, 0);
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return (0, 0);
    };
    // SAFETY: GPUI's view, alive while the window is.
    let view: &objc2_app_kit::NSView = unsafe { handle.ns_view.cast().as_ref() };
    let window = view.window().map_or(0, |w| Retained::as_ptr(&w) as usize);
    (window, handle.ns_view.as_ptr() as usize)
}

/// Gives the keyboard to GPUI's view in its window, from whichever web view
/// had it.
fn keyboard_to_gpui(ns_window: usize, ns_view: usize) {
    if ns_window == 0 || ns_view == 0 {
        return;
    }
    // SAFETY: the browser's own window and GPUI's view in it, both alive
    // while the browser is.
    let (window, view) = unsafe {
        (
            &*(ns_window as *const objc2_app_kit::NSWindow),
            &*(ns_view as *const objc2_app_kit::NSView),
        )
    };
    window.makeFirstResponder(Some(view));
}

/// Whether `event` happened in the window at `ns_window`: each window's
/// monitors see every event in the app.
fn event_in(event: &NSEvent, ns_window: usize) -> bool {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return false;
    };
    event
        .window(mtm)
        .is_some_and(|w| Retained::as_ptr(&w) as usize == ns_window)
}

fn mouse_navigation(button: isize) -> Option<Command> {
    match button {
        3 => Some(Command::Back),
        4 => Some(Command::Forward),
        _ => None,
    }
}

/// Notices clicks that land in a page. WebKit's views sit on top of GPUI's,
/// so GPUI never hears about them and would keep the keyboard focus (and
/// the address field's selection) where it was.
fn install_page_click_monitor(
    sender: Sender<BrowserEvent>,
    ns_window: usize,
    typing: Rc<Cell<bool>>,
) -> Option<Retained<AnyObject>> {
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit owns the event for the callback's duration.
        let mouse = unsafe { event.as_ref() };
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return event.as_ptr();
        };
        if !event_in(mouse, ns_window) {
            return event.as_ptr();
        }
        if matches!(mouse.r#type(), NSEventType::OtherMouseDown | NSEventType::OtherMouseUp) {
            if let Some(command) = mouse_navigation(mouse.buttonNumber()) {
                if mouse.r#type() == NSEventType::OtherMouseDown {
                    let _ = sender.try_send(BrowserEvent::Command(command));
                }
                return std::ptr::null_mut();
            }
            return event.as_ptr();
        }
        let in_page = mouse
            .window(mtm)
            .and_then(|window| window.contentView())
            .and_then(|content| {
                content.hitTest(content.convertPoint_fromView(mouse.locationInWindow(), None))
            })
            .is_some_and(in_web_view);
        if in_page {
            // The page has the keyboard from this moment; the key monitor
            // mustn't wait for the next frame to know it.
            typing.set(false);
            let _ = sender.try_send(BrowserEvent::PageClicked);
        }
        event.as_ptr()
    });
    // SAFETY: the block returns the live event, as AppKit requires.
    unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown
                | NSEventMask::RightMouseDown
                | NSEventMask::OtherMouseDown
                | NSEventMask::OtherMouseUp,
            &handler,
        )
    }
}

/// Whether `view` is a web view or inside one.
fn in_web_view(view: Retained<objc2_app_kit::NSView>) -> bool {
    let mut view = Some(view);
    while let Some(current) = view {
        let name = current.class().name();
        if name.to_str().is_ok_and(|n| n.contains("WKWebView") || n.contains("WryWebView")) {
            return true;
        }
        // SAFETY: walking up a live view hierarchy on the main thread.
        view = unsafe { current.superview() };
    }
    false
}

/// Whether the first responder is a web view, or something inside one.
fn page_has_keyboard(window: &objc2_app_kit::NSWindow) -> bool {
    window
        .firstResponder()
        .and_then(|responder| responder.downcast::<objc2_app_kit::NSView>().ok())
        .is_some_and(in_web_view)
}

/// What the key monitor needs to know of the browser's state, which it
/// can't ask GPUI for: kept up to date by the browser.
struct KeyFlags {
    palette_open: Rc<Cell<bool>>,
    typing: Rc<Cell<bool>>,
    suggesting: Rc<Cell<bool>>,
    removable: Rc<Cell<bool>>,
    menu_open: Rc<Cell<bool>>,
    finding: Rc<Cell<bool>>,
    /// The page in front has a text field focused.
    page_editing: Rc<Cell<bool>>,
}

fn install_shortcut_monitor(
    sender: Sender<BrowserEvent>,
    ns_window: usize,
    ns_view: usize,
    flags: KeyFlags,
) -> Option<Retained<AnyObject>> {
    let KeyFlags {
        palette_open,
        typing,
        suggesting,
        removable,
        menu_open,
        finding,
        page_editing,
    } = flags;
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // AppKit owns the event for the duration of this callback. Returning
        // its pointer passes it on; null consumes browser shortcuts before
        // WKWebView can handle them as page input.
        let key_event = unsafe { event.as_ref() };
        // Another window's; its own monitor has it.
        if !event_in(key_event, ns_window) {
            return event.as_ptr();
        }
        let key = key_event
            .charactersIgnoringModifiers()
            .map(|s| s.to_string().to_lowercase())
            .unwrap_or_default();
        // Editing keys in a page: GPUI's own bindings for them would take
        // them first, so they go straight down the responder chain from the
        // page, which selects, copies and pastes as a browser should.
        if !typing.get()
            && let Some(mtm) = objc2::MainThreadMarker::new()
            && let Some(window) = key_event.window(mtm)
            && page_has_keyboard(&window)
        {
            let flags = key_event.modifierFlags();
            let plain_command = flags.contains(NSEventModifierFlags::Command)
                && !flags.contains(NSEventModifierFlags::Control)
                && !flags.contains(NSEventModifierFlags::Option);
            let shift = flags.contains(NSEventModifierFlags::Shift);
            let action = match (key.as_str(), shift) {
                ("a", false) => Some(objc2::sel!(selectAll:)),
                ("c", false) => Some(objc2::sel!(copy:)),
                ("v", false) => Some(objc2::sel!(paste:)),
                ("x", false) => Some(objc2::sel!(cut:)),
                ("z", false) => Some(objc2::sel!(undo:)),
                ("z", true) => Some(objc2::sel!(redo:)),
                _ => None,
            };
            if plain_command && let Some(action) = action {
                // Select All goes to the page as a key press too: editors in
                // pages (uBlock Origin's own, say) take it that way.
                if key == "a"
                    && let Some(responder) = window.firstResponder()
                {
                    responder.keyDown(key_event);
                }
                let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
                // SAFETY: a standard editing action, to the first responder,
                // as the Edit menu would send it.
                unsafe { app.sendAction_to_from(action, None, None) };
                return std::ptr::null_mut();
            }
        }
        let in_page = objc2::MainThreadMarker::new()
            .and_then(|mtm| key_event.window(mtm))
            .is_some_and(|window| page_has_keyboard(&window));
        let state = KeyState {
            palette_open: palette_open.get(),
            suggesting: suggesting.get(),
            removable: removable.get(),
            menu_open: menu_open.get(),
            finding: finding.get(),
            editing: if in_page { page_editing.get() } else { typing.get() },
        };
        match shortcut(&key, key_event.keyCode(), key_event.modifierFlags(), state) {
            Some(command) => {
                let _ = sender.try_send(BrowserEvent::Command(command));
                std::ptr::null_mut()
            }
            None => {
                // A page can call focus() on its own search box while it
                // loads, taking the keyboard from the address field that
                // still shows a caret. Keys typed into one of our fields go
                // to it: the window's content view gets them back first.
                if typing.get()
                    && let Some(mtm) = objc2::MainThreadMarker::new()
                    && let Some(window) = key_event.window(mtm)
                    && page_has_keyboard(&window)
                {
                    keyboard_to_gpui(ns_window, ns_view);
                }
                event.as_ptr()
            }
        }
    });
    // SAFETY: the block returns either the original live event or null, as
    // required by AppKit's local monitor contract.
    unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) }
}

const TOOLBAR_HEIGHT: f32 = 52.0;
/// Where the close, minimise and zoom buttons sit: the close button's left
/// and top, in points, inside the toolbar.
const TRAFFIC_LIGHTS: (f32, f32) = (18.0, 19.0);
/// How long the caret stays shown, and then hidden, as it blinks.
const CARET_BLINK: Duration = Duration::from_millis(530);

/// An animation's length at the slow-motion scale (`VAMPIR_SLOW_MOTION`),
/// for those GPUI runs rather than the controls' tweens.
fn slowed(duration: Duration) -> Duration {
    static SCALE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    let scale = *SCALE.get_or_init(|| {
        std::env::var("VAMPIR_SLOW_MOTION")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(1.0)
    });
    duration.mul_f64(scale)
}
/// Theme saturation for a site with no colour of its own: nearly grey. A
/// site with one sets saturation by how vivid it is; see [`Tint`].
const NEUTRAL_SATURATION: f64 = 0.2;
/// Room on the toolbar's left for the traffic lights, which sit inside it.
const TRAFFIC_LIGHT_INSET: f32 = 86.0;
const OMNIBOX_HEIGHT: f32 = 36.0;
const BUTTON_SIZE: f32 = 32.0;
const RADIUS: f32 = 8.0;
const SIDEBAR_WIDTH: f32 = 240.0;
const TAB_STRIP_HEIGHT: f32 = 40.0;
const TAB_MIN_WIDTH: f32 = 110.0;
const TAB_MAX_WIDTH: f32 = 210.0;
/// The + for a new tab in the horizontal strip, and the room it takes:
/// in the strip after the last tab (with a tab's gap before it), or pinned
/// at the strip's end (with the strip's gap) once the tabs overflow.
const NEW_TAB_BUTTON: f32 = 28.0;
const NEW_TAB_IN_STRIP_ROOM: f32 = NEW_TAB_BUTTON + TAB_GAP;
const NEW_TAB_PINNED_ROOM: f32 = NEW_TAB_BUTTON + 8.0;
/// Space between horizontal tabs, between sidebar rows, and between rail
/// buttons.
const TAB_GAP: f32 = 5.0;
const ROW_GAP: f32 = 4.0;
const RAIL_GAP: f32 = 6.0;
/// How far the scroll-edge fades reach into a strip or a list.
const FADE_REACH_HORIZONTAL: f32 = 56.0;
const FADE_REACH_VERTICAL: f32 = 36.0;
const BOOKMARKS_HEIGHT: f32 = 32.0;
/// How long the sidebar, tab strip and bookmarks bar take to open or close.
const LAYOUT_MOVE: Duration = Duration::from_millis(260);
const RAIL_WIDTH: f32 = 56.0;
/// The download shelf along the bottom, old-Chrome style.
const SHELF_HEIGHT: f32 = 56.0;

/// The browser's own surfaces, derived from the palette so they follow its
/// hue as the active tab changes.
#[derive(Clone, Copy)]
struct Chrome {
    /// Toolbar, tab strip, sidebar and bookmarks bar: the window itself.
    ground: Rgba,
    /// The active tab and the address field, lifted off the ground.
    raised: Rgba,
    /// Behind inactive tabs, so each reads as its own item.
    rest: Rgba,
    /// Hover behind inactive rows and buttons.
    wash: Rgba,
    /// Where the chrome meets the page.
    line: Rgba,
    /// Text on an accent fill.
    on_accent: Rgba,
}

impl Chrome {
    fn new(palette: Palette) -> Self {
        let raised = if palette.is_dark {
            color::lerp(palette.soft_fill, palette.soft_fill_hover, 0.6)
        } else {
            palette.field_surface
        };
        Self {
            ground: color::lerp(palette.backdrop, palette.soft_fill, 0.45),
            raised,
            rest: color::with_alpha(raised, if palette.is_dark { 0.3 } else { 0.42 }),
            wash: color::with_alpha(raised, if palette.is_dark { 0.55 } else { 0.75 }),
            line: palette.area_border,
            on_accent: if palette.is_dark {
                palette.field_surface
            } else {
                color::WHITE
            },
        }
    }
}

fn site_name(url: &str) -> String {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    host.strip_prefix("www.").unwrap_or(&host).to_owned()
}

fn site_initial(url: &str) -> String {
    site_name(url)
        .chars()
        .next()
        .unwrap_or('•')
        .to_uppercase()
        .to_string()
}

/// The site's icon, or a letter standing in for it until it arrives.
fn site_icon(
    favicon: Option<Favicon>,
    url: &str,
    size: f32,
    active: bool,
    palette: Palette,
) -> AnyElement {
    let Some(favicon) = favicon else {
        return letter_badge(url, size, active, palette).into_any_element();
    };
    let frame = div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    // Black-on-transparent icons vanish on a dark theme; give them a plate.
    if palette.is_dark && favicon.dark {
        frame
            .rounded(px((size * 0.25).round()))
            .bg(color::with_alpha(color::WHITE, 0.88))
            .child(img(favicon.image.clone()).size(px((size * 0.75).round())))
            .into_any_element()
    } else {
        frame
            .child(img(favicon.image.clone()).size_full())
            .into_any_element()
    }
}

/// A letter standing in for the site's icon.
fn letter_badge(url: &str, size: f32, active: bool, palette: Palette) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px((size * 0.3).round()))
        .bg(if active {
            palette.accent
        } else {
            palette.soft_fill
        })
        .text_color(if active {
            Chrome::new(palette).on_accent
        } else {
            palette.soft_label
        })
        .text_size(px((size * 0.55).round()))
        .font_weight(FontWeight::SEMIBOLD)
        .child(site_initial(url))
}

/// The address as shown while the field isn't in use: the site in full
/// colour, the scheme and everything after the site greyed, so where you
/// are reads at a glance.
fn address_label(url: &str, palette: Palette) -> AnyElement {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, path) = rest.split_at(host_end);
    let dim = color::with_alpha(palette.text_secondary, 0.75);
    let path = if path == "/" { "" } else { path };
    div()
        .flex()
        .items_center()
        .min_w(px(0.0))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(14.0))
        .when(!scheme.is_empty(), |el| {
            el.child(div().flex_none().text_color(dim).child(format!("{scheme}://")))
        })
        .child(div().flex_none().text_color(palette.text_primary).child(host.to_owned()))
        .child(div().min_w(px(0.0)).truncate().text_color(dim).child(path.to_owned()))
        .into_any_element()
}

/// A borderless toolbar button: just its icon until hovered or switched on.
/// It occludes what is behind it, so pressing it never drags the window.
#[allow(clippy::too_many_arguments)]
fn tool_button(
    id: impl Into<ElementId>,
    glyph: Icon,
    label: impl Into<Hint>,
    size: f32,
    active: bool,
    palette: Palette,
    cx: &mut Context<Browser>,
    on_click: impl Fn(&mut Browser, &mut Window, &mut Context<Browser>) + 'static,
) -> Stateful<Div> {
    let ink = if active {
        palette.soft_label
    } else {
        palette.text_secondary
    };
    tool_button_inked(id, glyph, ink, label, size, active, true, palette, cx, on_click)
}

/// [`tool_button`] with its icon in a colour of the caller's choosing.
#[allow(clippy::too_many_arguments)]
fn tool_button_inked(
    id: impl Into<ElementId>,
    glyph: Icon,
    ink: Rgba,
    label: impl Into<Hint>,
    size: f32,
    active: bool,
    enabled: bool,
    palette: Palette,
    cx: &mut Context<Browser>,
    on_click: impl Fn(&mut Browser, &mut Window, &mut Context<Browser>) + 'static,
) -> Stateful<Div> {
    let chrome = Chrome::new(palette);
    let on_click = Rc::new(on_click);
    let pressed = on_click.clone();
    div()
        .id(id)
        .occlude()
        .relative()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS))
        .when(active, |el| el.bg(palette.soft_fill))
        .when(enabled, |el| {
            el.cursor_pointer().hover(move |style| {
                style.bg(if active {
                    palette.soft_fill_hover
                } else {
                    chrome.wash
                })
            })
        })
        .when(!enabled, |el| el.opacity(0.45))
        .when(enabled, |el| {
            el.on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
        })
        .child(icon(glyph, 16.0, ink))
        .when(enabled, |el| el.child(
            vampir::ring("ring", RADIUS, palette).on_key_down(cx.listener(
                move |this, event: &KeyDownEvent, window, cx| {
                    if vampir::key(event) == Some(Key::Activate) {
                        cx.stop_propagation();
                        pressed(this, window, cx);
                        cx.notify();
                    }
                },
            )),
        ))
        .with_hint(label.into(), hint::Side::Below, cx)
}

/// Gives an element a hint that AppKit draws, so it isn't cut off by the
/// page, shown below or beside it rather than under the pointer.
trait WithHint: Sized {
    fn with_hint(self, hint: Hint, side: hint::Side, cx: &mut Context<Browser>) -> Self;
}

impl WithHint for Stateful<Div> {
    fn with_hint(self, hint: Hint, side: hint::Side, cx: &mut Context<Browser>) -> Self {
        let text = match &hint.shortcut {
            Some(shortcut) => format!("{}   {shortcut}", hint.text),
            None => hint.text.to_string(),
        };
        // By window too: each draws its own.
        let key = format!("{}:{side:?}:{text}", cx.entity_id());
        let anchor_key = key.clone();
        self.on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            this.hover_hint(key.clone(), text.clone(), side, *hovered, window, cx)
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, _| this.dismiss_hint()),
        )
        .child(hint::anchor(anchor_key))
    }
}

/// A tab in a vertical list growing in (or, departing, shrinking away) as
/// `t` goes from 0 to 1: revealed by a clip at its full size, so its shape
/// never squashes, and fading with it.
fn revealed(row: impl IntoElement, height: f32, gap: f32, t: f32) -> AnyElement {
    div()
        .h(px(height * t))
        // Take the gap with it, so nothing jumps at the end.
        .mb(px(-gap * (1.0 - t)))
        .flex_none()
        .overflow_hidden()
        .opacity(t)
        .child(row)
        .into_any_element()
}

/// The × on a tab: shown on the active tab and on whichever tab is hovered.
fn close_button(
    id: impl Into<ElementId>,
    tab: u64,
    shown: bool,
    palette: Palette,
    cx: &mut Context<Browser>,
) -> Stateful<Div> {
    // Away from the pointer it takes no room, so the title has it all.
    div()
        .id(id)
        .h(px(18.0))
        .w(px(if shown { 18.0 } else { 0.0 }))
        .flex_none()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .opacity(if shown { 1.0 } else { 0.0 })
        .group_hover("tab", |style| style.opacity(1.0).w(px(18.0)))
        .hover(move |style| style.bg(palette.row_hover))
        .cursor_pointer()
        .child(icon(Icon::Close, 11.0, palette.text_secondary))
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.close_tab_by_pointer(tab, window, cx);
        }))
}

/// A speaker on tabs that are playing sound or have been muted.
fn tab_sound_button(
    tab: &BrowserTab,
    palette: Palette,
    side: hint::Side,
    cx: &mut Context<Browser>,
) -> Stateful<Div> {
    let id = tab.id;
    let muted = tab.muted;
    div()
        .id(("tab-sound", id))
        .size(px(18.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .hover(move |style| style.bg(palette.row_hover))
        .cursor_pointer()
        .child(icon(if muted { Icon::SoundMuted } else { Icon::Sound }, 14.0, palette.text_secondary))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_tab_mute(id, cx);
        }))
        .with_hint(Hint::new(if muted { "Unmute tab" } else { "Mute tab" }), side, cx)
}

/// A closed tab still shrinking out of the tab list.
#[derive(Clone)]
struct ClosingTab {
    id: u64,
    /// The tab to its left when it closed, which it stays beside.
    prev: Option<u64>,
    title: String,
    url: String,
    /// Its width in the horizontal strip when it closed.
    width: f32,
}

/// One place in a tab list: a live tab by index, or a departing ghost.
#[derive(Clone, Copy)]
enum Slot {
    Tab(usize),
    Ghost(usize),
}

/// What a tab shows: a web page, or one of the browser's own pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Web,
    Start,
    Settings,
    Downloads,
    Bookmarks,
}

impl Page {
    /// The address an internal page is saved under, so restored sessions
    /// bring it back.
    fn internal_url(self) -> &'static str {
        match self {
            Page::Web => "",
            Page::Start => "vamp://start",
            Page::Settings => "vamp://settings",
            Page::Downloads => "vamp://downloads",
            Page::Bookmarks => "vamp://bookmarks",
        }
    }

    fn from_internal_url(url: &str) -> Option<Page> {
        parse_internal(url).map(|(page, _)| page)
    }

    fn title(self) -> &'static str {
        match self {
            Page::Web => "New Tab",
            Page::Start => "Start Page",
            Page::Settings => "Settings",
            Page::Downloads => "Downloads",
            Page::Bookmarks => "Bookmarks",
        }
    }
}

/// A `vamp://` address as a page, and for settings, the section:
/// `vamp://settings/privacy`, and the shorthands `vamp://history`,
/// `vamp://extensions` and `vamp://newtab`.
fn parse_internal(url: &str) -> Option<(Page, Option<Section>)> {
    let rest = url.trim().strip_prefix("vamp://")?.trim_end_matches('/');
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    match host.to_ascii_lowercase().as_str() {
        "start" | "newtab" => Some((Page::Start, None)),
        "downloads" => Some((Page::Downloads, None)),
        "bookmarks" => Some((Page::Bookmarks, None)),
        "history" => Some((Page::Settings, Some(Section::History))),
        "extensions" | "addons" => Some((Page::Settings, Some(Section::Extensions))),
        "settings" | "preferences" => Some((Page::Settings, Section::from_slug(path))),
        _ => None,
    }
}

/// What a tab shows when its page couldn't load: what went wrong, where, and
/// a way to try again.
fn error_page(failure: &navigation::LoadFailure) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    };
    let site = site_name(&failure.url);
    let place = if site.is_empty() { failure.url.clone() } else { site };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Can’t open {place}</title>
<meta name="color-scheme" content="light dark"><style>
body {{ font: 14px -apple-system, system-ui, sans-serif; margin: 0; min-height: 100vh; display: grid; place-items: center; color: CanvasText; background: Canvas; }}
main {{ max-width: 520px; padding: 32px; }}
h1 {{ font-size: 22px; margin: 0 0 10px; }}
p {{ color: color-mix(in srgb, CanvasText 70%, transparent); line-height: 1.5; margin: 0 0 8px; }}
code {{ font: 12px ui-monospace, monospace; word-break: break-all; }}
a {{ display: inline-block; margin-top: 16px; padding: 7px 16px; border-radius: 8px; background: AccentColor; color: AccentColorText; text-decoration: none; font-weight: 500; }}
</style></head><body><main>
<h1>Can’t open {place}</h1>
<p>{description}</p>
<p><code>{url}</code></p>
<a href="{url}">Try Again</a>
</main></body></html>"#,
        place = escape(&place),
        description = escape(&failure.description),
        url = escape(&failure.url),
    )
}

/// A saved window's tabs as (address, title), and which to select: every
/// tab (they only load once shown), but nothing a page can't be reopened
/// from; the selection follows the tab it was on.
fn restorable(window: state::SavedWindow) -> (Vec<(String, String)>, usize) {
    let mut titles = window.titles.into_iter();
    let mut selected = window.selected;
    let mut kept = Vec::new();
    for (index, url) in window.tabs.into_iter().enumerate() {
        let title = titles.next().unwrap_or_default();
        let reopenable = Page::from_internal_url(&url).is_some()
            || matches!(url::Url::parse(&url), Ok(parsed) if matches!(
                parsed.scheme(),
                "http" | "https" | "file" | "about" | "webkit-extension"
            ));
        if reopenable {
            kept.push((url, title));
        } else if index < window.selected {
            selected = selected.saturating_sub(1);
        }
    }
    (kept, selected)
}

/// What a new tab opens to.
enum TabTarget {
    Url(String),
    Page(Page),
}

impl TabTarget {
    /// What a tab showing `page` at `url` shows again: the page itself, or
    /// for a web page, its address.
    fn of(page: Page, url: &str) -> Self {
        match page {
            Page::Web => TabTarget::Url(url.to_owned()),
            page => TabTarget::Page(page),
        }
    }
}

struct BrowserTab {
    id: u64,
    title: String,
    /// The page's address; for an internal page, its `vamp://` address.
    url: String,
    page: Page,
    /// Keeps nothing past the tab: no history, cookies or cache on disk,
    /// and not restored at launch.
    private: bool,
    zoom: f64,
    /// While a load's progress bar is showing: when it began, and when it
    /// finished, for the bar's fade.
    loading: Option<(std::time::Instant, Option<std::time::Instant>)>,
    /// Created when the tab first shows a web page; `None` for a web tab
    /// restored but not yet shown, or unloaded after going unused.
    view: Option<Rc<WebView>>,
    /// Whether WebKit currently reports audible media in this page.
    playing_audio: bool,
    /// The tab's requested mute state, kept while its web view is asleep.
    muted: bool,
    /// A newly opened background page stays silent until first selected.
    media_suspended: bool,
    /// When its page last committed (began to arrive), which is when
    /// extensions' content scripts go into it.
    committed: Option<std::time::Instant>,
    /// Whether its page has a text field focused, as the page last said.
    editing: bool,
    /// When it was last the tab in front, for unloading unused tabs.
    last_active: std::time::Instant,
}

/// A closed tab, for Reopen Closed Tab.
struct ClosedTab {
    url: String,
    page: Page,
    private: bool,
}

/// The tab switcher while it is open.
struct PaletteState {
    input: Entity<TextInput>,
    highlight: usize,
    /// A still of the page it covers; the live page is hidden meanwhile,
    /// as GPUI can't paint over it.
    snapshot: Option<Arc<gpui::Image>>,
    /// When it opened and, once dismissed, when it started to fade out.
    opened: std::time::Instant,
    closing: Option<std::time::Instant>,
    _changes: gpui::Subscription,
}

/// Text fields on the settings page, kept for the life of the window.
struct SettingsInputs {
    home_page: Entity<TextInput>,
    custom_search: Entity<TextInput>,
    instance: Entity<TextInput>,
    download_dir: Entity<TextInput>,
    user_agent: Entity<TextInput>,
    history_search: Entity<TextInput>,
    extension_source: Entity<TextInput>,
    bookmark_search: Entity<TextInput>,
}

/// Where a tab's page sends its events: the window holding the tab.
#[derive(Clone)]
struct TabRoute(Arc<std::sync::Mutex<Sender<BrowserEvent>>>);

impl TabRoute {
    fn send(&self, event: BrowserEvent) {
        if let Ok(sender) = self.0.lock() {
            let _ = sender.try_send(event);
        }
    }

    /// Whether it leads to the window receiving from `sender`'s channel.
    fn leads_to(&self, sender: &Sender<BrowserEvent>) -> bool {
        self.0.lock().is_ok_and(|current| current.same_channel(sender))
    }

    fn set(&self, sender: Sender<BrowserEvent>) {
        if let Ok(mut current) = self.0.lock() {
            *current = sender;
        }
    }
}

unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

/// Hands memory freed after a big job back to the system: macOS's allocator
/// otherwise keeps it, and the app looks that much bigger.
fn return_freed_memory() {
    // SAFETY: a null zone means every zone; a goal of 0, as much as it can.
    unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
}

/// Tab ids are unique across windows: extensions see every tab at once.
static NEXT_TAB_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// What every window shares: one history, one download list, one set of
/// bookmarks and icons, one set of content rules and one extension
/// controller. Each window keeps its own tabs and layout.
type DownloadKeepers = Rc<RefCell<HashMap<u64, (Rc<WebView>, usize)>>>;

struct Common {
    history: RefCell<History>,
    downloads: RefCell<Downloads>,
    bookmarks: RefCell<Bookmarks>,
    favicons: RefCell<Favicons>,
    rules: RefCell<ContentRules>,
    /// `None` where WebKit has no extension support (before macOS 15.4).
    extensions: RefCell<Option<Extensions>>,
    /// Every open window's browser, oldest first.
    windows: RefCell<Vec<gpui::WeakEntity<Browser>>>,
    /// Each tab's route for its page's events, by tab id.
    routes: RefCell<HashMap<u64, TabRoute>>,
    /// Each ordinary window's tabs to restore, oldest window first, keyed
    /// by the window's serial number.
    sessions: RefCell<Vec<(u64, state::SavedWindow)>>,
    /// The state file as last written, to rewrite with a window gone.
    saved: RefCell<SavedState>,
    /// Settings pages read as they load (HTTPS-only, where downloads go,
    /// site permissions), shared by every page in every window, so a tab
    /// dragged to another window keeps following them.
    live: Live,
    /// The page offered to other devices through Handoff.
    handoff: RefCell<handoff::Handoff>,
    /// When a check for extension updates began, while one is under way
    /// (from any window), so a second doesn't install the same updates
    /// again. If its answer never comes, the check counts as over after a
    /// while.
    checking_updates: Cell<Option<std::time::Instant>>,
    /// Set on Quit, so windows closing on the way out stay in the session.
    quitting: Cell<bool>,
    /// Keep a web view alive while any of its downloads are running.
    download_keepers: DownloadKeepers,
    /// Where pages on old devices' servers found this run load, until the
    /// next launch gives the shared store them too (see [`legacy_http`]).
    legacy_store: RefCell<Option<Retained<objc2_web_kit::WKWebsiteDataStore>>>,
    /// Pages made at launch before the content rules were ready, with the
    /// address each is to load once they are (see `release_waiting_loads`).
    waiting_loads: RefCell<Vec<(std::rc::Weak<WebView>, String)>>,
    next_view: Cell<u64>,
    /// Windows closed by hand, and at a launch that doesn't restore them,
    /// the last session's; oldest first, for ⌘⇧T to reopen once a window's
    /// own closed tabs run out. Kept in the state file.
    closed_windows: RefCell<Vec<state::SavedWindow>>,
    /// Where background work that concerns no window in particular reports
    /// back: whichever window is in front (or else the oldest) when the
    /// answer arrives, so closing the window that asked doesn't lose it.
    anywhere: Sender<BrowserEvent>,
    /// Answers for any window that came while none was open, for the
    /// next one to open.
    stranded: RefCell<Vec<BrowserEvent>>,
    /// uBlock Origin's last report of its lists and settings, while it
    /// runs. Its rules are everyone's, so this is too.
    ublock_state: RefCell<Option<filters::UblockState>>,
    /// Numbers each build of uBlock Origin's rules, so only the latest
    /// takes effect.
    ublock_build: Cell<u64>,
    next_window: Cell<u64>,
}

impl Common {
    fn new(extension_events: Sender<ExtensionEvent>, anywhere: Sender<BrowserEvent>) -> Rc<Self> {
        let saved = SavedState::load();
        let closed_windows = saved.closed_windows.clone();
        // Old devices found in earlier runs, before anything uses the shared
        // store: WebKit takes its proxies then, not later.
        legacy_http::load_saved();
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            // SAFETY: the shared store, on the main thread.
            let store = unsafe { objc2_web_kit::WKWebsiteDataStore::defaultDataStore(mtm) };
            legacy_http::route(&store);
        }
        // Before any tab exists: every tab's web view is made from the
        // extension controller's configuration.
        let extensions = Extensions::supported().then(|| {
            let background = extension_events.clone();
            Extensions::new(
                move |event| {
                    let _ = extension_events.try_send(event);
                },
                background,
            )
        });
        Rc::new(Self {
            history: RefCell::new(History::load()),
            downloads: RefCell::new(Downloads::load()),
            bookmarks: RefCell::new(Bookmarks::new(saved.bookmarks.clone())),
            favicons: RefCell::new(Favicons::new()),
            rules: RefCell::new(ContentRules::default()),
            extensions: RefCell::new(extensions),
            windows: RefCell::new(Vec::new()),
            routes: RefCell::new(HashMap::new()),
            sessions: RefCell::new(Vec::new()),
            saved: RefCell::new(saved),
            live: Live::new(&Settings::load()),
            handoff: RefCell::default(),
            checking_updates: Cell::new(None),
            quitting: Cell::new(false),
            closed_windows: RefCell::new(closed_windows),
            download_keepers: Rc::default(),
            waiting_loads: RefCell::default(),
            legacy_store: RefCell::default(),
            next_view: Cell::new(0),
            anywhere,
            stranded: RefCell::default(),
            ublock_state: RefCell::new(None),
            ublock_build: Cell::new(0),
            next_window: Cell::new(1),
        })
    }

    /// Writes the state file with every ordinary window's tabs.
    fn save_state(&self) {
        let mut state = self.saved.borrow_mut();
        state.bookmarks = self.bookmarks.borrow().root().to_vec();
        state.windows = self.sessions.borrow().iter().map(|(_, w)| w.clone()).collect();
        // Windows closed by hand come back only with ⌘⇧T, never at launch.
        state.closed_windows = self.closed_windows.borrow().clone();
        let (tabs, selected) = state
            .windows
            .first()
            .map_or((Vec::new(), 0), |first| (first.tabs.clone(), first.selected));
        state.tabs = tabs;
        state.selected = selected;
        if let Err(err) = state.save() {
            eprintln!("Could not save state: {err}");
        }
    }

    /// Keeps a window for ⌘⇧T to reopen, the ten closed last; not one
    /// that held only new-tab pages (`home` among them), which there's
    /// nothing to reopen of.
    fn remember_closed(&self, window: state::SavedWindow, home: &str) {
        let home = site_name(home);
        let blank = |url: &String| {
            Page::from_internal_url(url) == Some(Page::Start)
                || url == "about:blank"
                || (!home.is_empty() && site_name(url) == home)
        };
        if window.tabs.iter().all(blank) {
            return;
        }
        let mut closed = self.closed_windows.borrow_mut();
        closed.push(window);
        if closed.len() > 10 {
            closed.remove(0);
        }
    }

    /// The live browsers, oldest first.
    fn browsers(&self) -> Vec<Entity<Browser>> {
        self.windows
            .borrow()
            .iter()
            .filter_map(|weak| weak.upgrade())
            .collect()
    }
}

/// Settings read from WebKit callbacks, which outlive any borrow of the
/// browser.
struct Live {
    https_only: Rc<Cell<bool>>,
    download_dir: Rc<RefCell<PathBuf>>,
    /// Where downloads under way will write, before their files exist, so
    /// two of the same name started together don't share one; with the
    /// address each came from.
    reserved_downloads: Rc<RefCell<HashMap<PathBuf, String>>>,
    /// Camera, microphone, screen: 0 ask, 1 allow, 2 block.
    permissions: Arc<[AtomicU8; 3]>,
}

impl Live {
    fn new(settings: &Settings) -> Self {
        let live = Self {
            https_only: Rc::new(Cell::new(settings.https_only)),
            download_dir: Rc::new(RefCell::new(settings.download_dir())),
            reserved_downloads: Rc::default(),
            permissions: Arc::new([AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0)]),
        };
        live.update(settings);
        live
    }

    fn update(&self, settings: &Settings) {
        self.https_only.set(settings.https_only);
        *self.download_dir.borrow_mut() = settings.download_dir();
        let code = |p: SitePermission| match p {
            SitePermission::Ask => 0,
            SitePermission::Allow => 1,
            SitePermission::Block => 2,
        };
        for (slot, permission) in self.permissions.iter().zip([
            settings.camera,
            settings.microphone,
            settings.screen_capture,
        ]) {
            slot.store(code(permission), Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PingTarget {
    Toolbar(ToolbarItem),
    Omnibox,
    CopyLink,
    BookmarkPage,
}

impl PingTarget {
    fn toolbar(self, item: ToolbarItem) -> bool {
        self == Self::Toolbar(item) || (self == Self::CopyLink && item == ToolbarItem::CopyLink)
    }

    fn omnibox(self) -> bool {
        matches!(self, Self::Omnibox | Self::CopyLink)
    }
}

struct Browser {
    controls: ControlState,
    address: Entity<TextInput>,
    tab_scroll: ScrollHandle,
    /// The selected tab should be scrolled into view once it has been laid
    /// out.
    reveal_selected: bool,
    /// Tab ids (live and departing) in the horizontal strip's child order,
    /// as last laid out, to find a tab's bounds by.
    strip_children: Vec<u64>,
    /// The horizontal tab width held after a close, until the pointer
    /// leaves the strip.
    tab_freeze: Option<f32>,
    /// The tabs overflow the horizontal strip, so its + is pinned at the
    /// end rather than after the last tab; as of the last frame.
    new_tab_pinned: bool,
    /// Tabs opened this session, still growing in.
    arriving: HashSet<u64>,
    closing: Vec<ClosingTab>,
    tabs: Vec<BrowserTab>,
    /// Sign-in form tab id -> tab whose HTTP challenge is waiting.
    auth_forms: HashMap<u64, u64>,
    auth_tabs: HashSet<u64>,
    selected: usize,
    selection_generation: u64,
    /// The tab selection whose page was last offered keyboard focus.
    focused_selection_generation: u64,
    vertical_tabs: bool,
    compact_vertical_tabs: bool,
    bookmarks_bar: bool,
    /// What each site's page said its colour was when last sampled.
    site_tints: HashMap<String, Option<Tint>>,
    settings: Settings,
    /// What every window shares.
    common: Rc<Common>,
    /// A private window: every tab in it is private.
    private: bool,
    /// Where this window's private tabs keep cookies and storage, in
    /// memory, until it closes: shared, so a login in one private tab holds
    /// in the next, and in pop-ups.
    private_data: std::cell::OnceCell<Retained<objc2_web_kit::WKWebsiteDataStore>>,
    /// This window's AppKit window, to tell its events from other windows'.
    ns_window: usize,
    /// GPUI's view in that window, which our own fields take keys through.
    ns_view: usize,
    /// Numbers windows in the order they opened, for the session.
    serial: u64,
    /// A tab being pressed or dragged.
    tab_drag: Option<tabdrag::TabDrag>,
    /// Where each tab was drawn, for drags to find their place by.
    tab_bounds: tabdrag::TabBounds,
    /// Its last tab was dragged away: the window closes.
    close_when_drawn: bool,
    /// The tab or bookmark under the pointer, whose label glides if cut.
    hovered_label: Option<u64>,
    label_widths: marquee::Widths,
    /// How many frames running the horizontal strip has been drawn.
    strip_frames: u32,
    /// The page's size, held while the browser's parts slide.
    page_hold: Rc<Cell<Option<(f32, f32)>>>,
    /// The hint the pointer rests on, and the wait before it shows.
    hint_key: Option<String>,
    _hint_later: Option<Task<()>>,
    /// A bookmarks folder's menu, while open, and whether it is (for the
    /// key monitor's Escape).
    bookmark_menu: Option<bookmark_menu::BookmarkMenu>,
    bookmark_menu_open: Rc<Cell<bool>>,
    /// Where each bookmarks folder's button or row was drawn.
    menu_anchors: bookmark_menu::Anchors,
    /// The folder ⌘D last saved into, or a bookmark was last filed in.
    last_bookmark_folder: Option<u64>,
    /// The folder the bookmark manager shows; `None` for the bar.
    bookmark_folder: Option<u64>,
    /// What's selected in the manager's list.
    bookmark_selection: bookmarks_view::BookmarkSelection,
    /// A line on the bookmark manager after importing or exporting.
    bookmark_notice: Option<String>,
    /// Minimal mode: only the page, the browser showing on demand.
    minimal: bool,
    chrome_revealed: bool,
    /// When the pointer left the revealed browser, to fold it away after.
    pointer_left: Option<std::time::Instant>,
    _minimal_watch: Option<Task<()>>,
    /// A tab carried from another window would land here, at this index.
    drop_hint: Option<(usize, String)>,
    /// Where the window was when last drawn, for the session.
    window_bounds: Option<[f32; 4]>,
    /// A session save waiting for a burst of changes to settle.
    _persist_later: Option<Task<()>>,
    /// A redraw on its way, and when it's due; see `redraw_soon`.
    redraw_later: Option<(std::time::Instant, Task<()>)>,
    recently_closed: Vec<ClosedTab>,
    palette: Option<PaletteState>,
    palette_open: Rc<Cell<bool>>,
    /// Whether one of our text fields has the keyboard, for the key monitor.
    typing: Rc<Cell<bool>>,
    /// Whether the address field's suggestions are showing, for the key
    /// monitor's arrows and Escape.
    suggesting: Rc<Cell<bool>>,
    /// Whether a history row picked with the arrows is highlighted, which
    /// ⇧⌫ removes, and whether the page in front is editing text; for the
    /// key monitor.
    removable: Rc<Cell<bool>>,
    page_editing: Rc<Cell<bool>>,
    /// Whether the find bar's field has the keyboard, for the key
    /// monitor's Escape and ⇧Enter.
    finding: Rc<Cell<bool>>,
    find: find::FindState,
    find_input: Entity<TextInput>,
    suggest: Option<suggest::SuggestState>,
    /// Where the address field is, for the suggestions to hang from.
    omnibox_bounds: Rc<Cell<Option<Bounds<gpui::Pixels>>>>,
    settings_section: Section,
    inputs: SettingsInputs,
    /// A one-line message on the settings page after an action.
    notice: Option<String>,
    /// Takes the message shown away from the settings page away again.
    _toast_later: Option<Task<()>>,
    is_default_browser: bool,
    /// The control that last acted, and a count that restarts its pulse.
    ping: (Option<PingTarget>, u64),
    /// This frame's load progress and bar opacity; see `load_progress`.
    progress: Option<(f32, f32)>,
    /// Sites drawn this frame with no icon at hand, to look up after it:
    /// on disk, else from the site itself.
    favicon_wants: std::cell::RefCell<HashSet<String>>,
    /// Resets the address field when it loses the keyboard.
    _address_blur: Option<gpui::Subscription>,
    /// When the caret last moved or the text changed; the caret blinks
    /// from here, solid while typing.
    caret_epoch: std::time::Instant,
    _caret_blink: Option<Task<()>>,
    _caret_reset: Option<gpui::Subscription>,
    /// Where each extension's toolbar button was drawn, to anchor its popup.
    action_bounds: Rc<RefCell<HashMap<String, Bounds<gpui::Pixels>>>>,
    launched: std::time::Instant,
    /// Bumped by each extension load at launch; the reload below waits for
    /// them to stop.
    extension_loads: u64,
    reloaded_for_extensions: bool,
    logged_extension_errors: HashSet<String>,
    events: Receiver<BrowserEvent>,
    sender: Sender<BrowserEvent>,
    shortcut_monitor: Option<Retained<AnyObject>>,
    click_monitor: Option<Retained<AnyObject>>,
    /// Lets pages set the pointer; see [`page_cursor`].
    cursor_monitor: Option<Retained<AnyObject>>,
    _poll: Task<()>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        if self.private {
            navigation::forget_private_auth(self.serial);
        }
        // A window closed by hand leaves the session for the closed ones,
        // to reopen; any closed by quitting is restored next launch.
        if !self.common.quitting.get() {
            if !self.private && !self.tabs.is_empty() {
                // As it is now, not as last saved.
                self.persist();
            }
            let serial = self.serial;
            let mut sessions = self.common.sessions.borrow_mut();
            if let Some(at) = sessions.iter().position(|(s, _)| *s == serial) {
                let (_, window) = sessions.remove(at);
                drop(sessions);
                self.common.remember_closed(window, &self.settings.home_page);
            } else {
                drop(sessions);
            }
            self.common.save_state();
        }
        // Its tabs are gone: extensions let go of their pages (which would
        // otherwise play on, unseen), and nothing routes to them.
        for tab in &self.tabs {
            if let Some(view) = &tab.view {
                navigation::forget(&view.webview());
            }
            if let Ok(mut extensions) = self.common.extensions.try_borrow_mut()
                && let Some(extensions) = extensions.as_mut()
            {
                extensions.tab_closed(tab.id);
            }
            self.common.routes.borrow_mut().remove(&tab.id);
        }
        for monitor in [self.click_monitor.take(), self.cursor_monitor.take()].into_iter().flatten() {
            // SAFETY: this is the monitor token returned by AppKit.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        if let Some(monitor) = self.shortcut_monitor.take() {
            // SAFETY: this is the monitor token returned by AppKit above.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        self.history().save();
    }
}

impl Browser {
    fn history(&self) -> std::cell::RefMut<'_, History> {
        self.common.history.borrow_mut()
    }

    fn downloads(&self) -> std::cell::RefMut<'_, Downloads> {
        self.common.downloads.borrow_mut()
    }

    fn bookmarks(&self) -> std::cell::Ref<'_, Bookmarks> {
        self.common.bookmarks.borrow()
    }

    fn bookmarks_mut(&self) -> std::cell::RefMut<'_, Bookmarks> {
        self.common.bookmarks.borrow_mut()
    }

    fn favicons(&self) -> std::cell::RefMut<'_, Favicons> {
        self.common.favicons.borrow_mut()
    }

    fn rules(&self) -> std::cell::RefMut<'_, ContentRules> {
        self.common.rules.borrow_mut()
    }

    /// Shows the front tab's address in the field again, dropping whatever
    /// was typed or selected there.
    fn reset_address(&mut self, cx: &mut Context<Self>) {
        let text = self.address_text(self.selected);
        self.address.update(cx, |input, cx| input.set_text(&text, cx));
    }

    fn address_focused(&self, window: &Window, cx: &App) -> bool {
        self.address.read(cx).focus_handle.is_focused(window)
    }

    /// Offers the front tab's page to other devices, unless it's private.
    fn offer_handoff(&self) {
        let Some(tab) = self.tabs.get(self.selected) else {
            return;
        };
        let mut handoff = self.common.handoff.borrow_mut();
        if tab.private || tab.page != Page::Web {
            handoff.withdraw();
        } else {
            handoff.offer(&tab.url, &tab.title);
        }
    }

    /// Tab `id`'s event route, pointing at this window.
    fn route(&self, id: u64) -> TabRoute {
        self.common
            .routes
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| TabRoute(Arc::new(std::sync::Mutex::new(self.sender.clone()))))
            .clone()
    }

    /// The pointer came onto (or left) a tab or bookmark, whose label
    /// glides while it's there.
    fn hover_label(&mut self, key: u64, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered {
            Some(key)
        } else if self.hovered_label == Some(key) {
            None
        } else {
            self.hovered_label
        };
        if next != self.hovered_label {
            self.hovered_label = next;
            cx.notify();
        }
    }

    /// Does something to every other window's browser.
    fn for_other_windows(
        &self,
        cx: &mut Context<Self>,
        mut change: impl FnMut(&mut Browser, &mut Context<Browser>),
    ) {
        let me = cx.entity_id();
        for browser in self.common.browsers() {
            if browser.entity_id() != me {
                browser.update(cx, |browser, cx| change(browser, cx));
            }
        }
    }

    /// Takes this window out of the ones the others see and change, as
    /// it's about to close: none of them reach it, tabless, meanwhile, and
    /// the last one standing knows it is.
    fn leave_windows(&mut self, cx: &mut Context<Self>) {
        self.close_when_drawn = true;
        let me = cx.entity_id();
        self.common
            .windows
            .borrow_mut()
            .retain(|weak| weak.entity_id() != me);
        cx.notify();
    }

    /// Redraws the other windows after something they show changed.
    fn refresh_other_windows(&self, cx: &mut Context<Self>) {
        self.for_other_windows(cx, |_, cx| cx.notify());
    }
}

impl ControlHost for Browser {
    fn control_state(&self) -> &ControlState {
        &self.controls
    }

    fn control_state_mut(&mut self) -> &mut ControlState {
        &mut self.controls
    }
}

/// A plain single-line text field whose contents are read when needed.
fn text_input(cx: &mut App, placeholder: &str, text: &str) -> Entity<TextInput> {
    let palette = ControlState::default().palette();
    cx.new(|cx| {
        let mut input = TextInput::new(
            cx,
            placeholder,
            false,
            InputStyle::from_palette(palette, 13.0),
        );
        input.set_text(text, cx);
        input
    })
}

impl Browser {
    /// A browser window. `restore` holds the tabs it had last session, to
    /// open as the startup setting says; without it the window opens on a
    /// new tab.
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        common: Rc<Common>,
        private: bool,
        restore: Option<state::SavedWindow>,
        launch: bool,
        carry: Option<BrowserTab>,
    ) -> Self {
        // The layout last saved; the rest of the saved state is for main().
        let (vertical_tabs, compact_vertical_tabs, bookmarks_bar) = {
            let saved = common.saved.borrow();
            (saved.vertical_tabs, saved.compact_vertical_tabs, saved.bookmarks_bar)
        };
        let settings = Settings::load();
        let first_window = common.browsers().is_empty();
        common.windows.borrow_mut().push(cx.weak_entity());
        let serial = common.next_window.get();
        common.next_window.set(serial + 1);
        let (ns_window, ns_view) = ns_window_of(window);
        let (sender, events) = async_channel::unbounded();
        for event in common.stranded.take() {
            let _ = sender.try_send(event);
        }
        let submit = sender.clone();
        let palette = ControlState::default().palette();
        let address = cx.new(|cx| {
            let mut input = TextInput::new(
                cx,
                "Search or enter an address",
                false,
                InputStyle::from_palette(palette, 14.0),
            );
            let edited = submit.clone();
            input.on_submit = Some(Box::new(move |text, _| {
                let _ = submit.try_send(BrowserEvent::Address(text.to_owned()));
            }));
            input.on_change = Some(Box::new(move |text, _| {
                let _ = edited.try_send(BrowserEvent::AddressEdited(text.to_string()));
            }));
            input
        });
        let next_event = events.clone();
        let poll = cx.spawn_in(window, async move |this, cx| {
            while let Ok(first) = next_event.recv().await {
                if cx
                    .update(|window, app| {
                        this.update(app, |browser, cx| browser.drain_events(first, window, cx))
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let inputs = SettingsInputs {
            home_page: text_input(cx, "https://…", &settings.home_page),
            custom_search: text_input(
                cx,
                "https://example.org/search?q=%s",
                &settings.custom_search,
            ),
            instance: text_input(cx, "https://searx.example.org", ""),
            download_dir: text_input(cx, "~/Downloads", &settings.download_dir),
            user_agent: text_input(cx, "Mozilla/5.0 …", &settings.custom_user_agent),
            history_search: text_input(cx, "Search history", ""),
            bookmark_search: text_input(cx, "Search bookmarks", ""),
            extension_source: text_input(
                cx,
                "addons.mozilla.org link, add-on name, or file path",
                "",
            ),
        };
        cx.observe(&inputs.history_search, |_, _, cx| cx.notify())
            .detach();
        cx.observe(&inputs.bookmark_search, |_, _, cx| cx.notify())
            .detach();
        // Enter in a settings field saves it, like its Set button.
        for (input, field) in [
            (&inputs.home_page, pages::Field::HomePage),
            (&inputs.custom_search, pages::Field::CustomSearch),
            (&inputs.instance, pages::Field::Instance),
            (&inputs.download_dir, pages::Field::DownloadDir),
            (&inputs.user_agent, pages::Field::UserAgent),
            (&inputs.extension_source, pages::Field::ExtensionSource),
        ] {
            let sender = sender.clone();
            input.update(cx, |input, _| {
                input.on_submit = Some(Box::new(move |_, _| {
                    let _ = sender.try_send(BrowserEvent::SaveField(field));
                }));
            });
        }
        let palette_open = Rc::new(Cell::new(false));
        let typing = Rc::new(Cell::new(false));
        let suggesting = Rc::new(Cell::new(false));
        let removable = Rc::new(Cell::new(false));
        let page_editing = Rc::new(Cell::new(false));
        let finding = Rc::new(Cell::new(false));
        let find_input = text_input(cx, "Find in page", "");
        find_input.update(cx, |input, _| {
            let edited = sender.clone();
            let next = sender.clone();
            input.on_change = Some(Box::new(move |_, _| {
                let _ = edited.try_send(BrowserEvent::FindEdited);
            }));
            input.on_submit = Some(Box::new(move |_, _| {
                let _ = next.try_send(BrowserEvent::Command(Command::FindNext));
            }));
        });
        let bookmark_menu_open = Rc::new(Cell::new(false));
        let mut controls = ControlState::default();
        controls.theme.saturation = NEUTRAL_SATURATION;
        // `VAMPIR_SLOW_MOTION=8` slows every animation, for looking at them.
        controls.slow_motion_from_env();
        let mut browser = Self {
            controls,
            address,
            tab_scroll: ScrollHandle::new(),
            reveal_selected: true,
            strip_children: Vec::new(),
            tab_freeze: None,
            new_tab_pinned: false,
            arriving: HashSet::new(),
            closing: Vec::new(),
            tabs: Vec::new(),
            auth_forms: HashMap::new(),
            auth_tabs: HashSet::new(),
            selected: 0,
            selection_generation: 0,
            focused_selection_generation: 0,
            vertical_tabs,
            compact_vertical_tabs,
            bookmarks_bar,
            site_tints: HashMap::new(),
            settings,
            common: common.clone(),
            private,
            private_data: std::cell::OnceCell::new(),
            ns_window,
            ns_view,
            serial,
            tab_drag: None,
            tab_bounds: Default::default(),
            close_when_drawn: false,
            drop_hint: None,
            hovered_label: None,
            strip_frames: 0,
            page_hold: Rc::new(Cell::new(None)),
            hint_key: None,
            _hint_later: None,
            bookmark_menu: None,
            bookmark_menu_open: bookmark_menu_open.clone(),
            menu_anchors: Default::default(),
            last_bookmark_folder: None,
            bookmark_folder: None,
            bookmark_selection: Default::default(),
            bookmark_notice: None,
            label_widths: Default::default(),
            minimal: false,
            chrome_revealed: false,
            pointer_left: None,
            _minimal_watch: None,
            window_bounds: None,
            _persist_later: None,
            redraw_later: None,
            recently_closed: Vec::new(),
            palette: None,
            palette_open: palette_open.clone(),
            typing: typing.clone(),
            suggesting: suggesting.clone(),
            removable: removable.clone(),
            page_editing: page_editing.clone(),
            finding: finding.clone(),
            find: find::FindState::default(),
            find_input,
            suggest: None,
            omnibox_bounds: Rc::new(Cell::new(None)),
            settings_section: Section::General,
            inputs,
            notice: None,
            _toast_later: None,
            is_default_browser: interop::is_default_browser(),
            _address_blur: None,
            ping: (None, 0),
            progress: None,
            favicon_wants: Default::default(),
            caret_epoch: std::time::Instant::now(),
            _caret_blink: None,
            _caret_reset: None,
            action_bounds: Rc::new(RefCell::new(HashMap::new())),
            launched: std::time::Instant::now(),
            extension_loads: 0,
            reloaded_for_extensions: false,
            logged_extension_errors: HashSet::new(),
            events,
            shortcut_monitor: install_shortcut_monitor(
                sender.clone(),
                ns_window,
                ns_view,
                KeyFlags {
                    palette_open,
                    typing: typing.clone(),
                    suggesting,
                    removable: removable.clone(),
                    menu_open: bookmark_menu_open,
                    finding,
                    page_editing: page_editing.clone(),
                },
            ),
            click_monitor: install_page_click_monitor(sender.clone(), ns_window, typing),
            cursor_monitor: page_cursor::watch(ns_window, ns_view),
            sender,
            _poll: poll,
        };
        browser.controls.observe_appearance(window, cx);
        // The caret blinks: a redraw every half blink while a field has the
        // keyboard, nothing otherwise.
        browser._caret_blink = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(CARET_BLINK).await;
                let alive = this
                    .update_in(cx, |browser, window, cx| {
                        if browser.text_field_focused(window, cx) {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        }));
        // WebKit's audio state changes without a navigation or page event.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                if this.update(cx, |browser, cx| browser.refresh_tab_audio(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Once a minute, unload background tabs that have gone unused.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                if this.update(cx, |browser, _| browser.sleep_unused_tabs()).is_err() {
                    break;
                }
            }
        })
        .detach();
        let address = browser.address.clone();
        browser._caret_reset = Some(cx.observe(&address, |this, _, _| {
            this.caret_epoch = std::time::Instant::now();
        }));
        // Leaving the address field drops the selection and whatever was
        // half-typed, showing the page's address again.
        let focus = browser.address.read(cx).focus_handle.clone();
        browser._address_blur = Some(cx.on_blur(&focus, window, |this, _, cx| {
            this.close_suggestions(cx);
            this.reset_address(cx);
            cx.notify();
        }));
        // The content rules are shared: compiled once, for the first window.
        // Pages wait for them (and uBlock Origin's from last time) before
        // loading, so the first isn't loaded unfiltered; not for long.
        if first_window {
            let ublock_on = browser
                .common
                .extensions
                .borrow()
                .as_ref()
                .is_some_and(|e| e.is_enabled(filters::UBLOCK_ID));
            if ublock_on {
                let sender = browser.common.anywhere.clone();
                browser.rules().restore_ublock(filters::enforced(), move || {
                    let _ = sender.try_send(BrowserEvent::Command(Command::ApplyRules));
                });
            }
            browser.update_rules();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let _ = this.update(cx, |browser, _| {
                    browser.rules().give_up_waiting();
                    browser.release_waiting_loads();
                });
            })
            .detach();
        }
        // What the settings say to start with applies at launch; a window
        // reopened, or opened for links, has exactly its tabs.
        let startup = if launch { browser.settings.startup } else { Startup::Restore };
        // Restored at launch, before extensions have loaded: see
        // `reload_after_extensions_load`.
        browser.reloaded_for_extensions = !launch;
        // (address, title) of each tab to restore.
        let (restored, selected): (Vec<(String, String)>, usize) = match (restore, startup) {
            (None, _) => (Vec::new(), 0),
            (Some(window), Startup::Restore) => restorable(window),
            (Some(_), Startup::Home) => (vec![(browser.settings.home_page.clone(), String::new())], 0),
            (Some(_), Startup::StartPage) => {
                (vec![(Page::Start.internal_url().to_owned(), String::new())], 0)
            }
        };
        if let Some(tab) = carry {
            // A tab dragged out of another window.
            browser.adopt_tab(tab, 0, cx);
        } else if restored.is_empty() {
            let target = browser.new_tab_target();
            browser.open_tab(target, private, true, window, cx);
        }
        // Restored tabs wait to load until they're shown, so a big session
        // doesn't start every page (and its memory) at once.
        for (url, title) in restored {
            browser.push_unloaded_tab(url, title, private);
        }
        if !browser.tabs.is_empty() {
            browser.select(selected.min(browser.tabs.len() - 1), cx);
        }
        if first_window {
            // Bookmarks show their icons without their pages being open.
            let urls: Vec<String> = browser
                .bookmarks()
                .links()
                .into_iter()
                .filter_map(|b| b.url.clone())
                .collect();
            for url in urls {
                browser.want_favicon(&url, None);
            }
        }
        if !private {
            browser.persist();
        }
        browser.apply_theme();
        browser
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Where tab `id` is, if it's showing a web page: one switched to one
    /// of ours keeps its page running hidden, whose events mustn't reach
    /// the tab.
    fn web_index_of(&self, id: u64) -> Option<usize> {
        self.index_of(id).filter(|&index| self.tabs[index].page == Page::Web)
    }

    /// A configuration for a private tab: this window's private store.
    fn private_configuration(&self) -> Option<Retained<objc2_web_kit::WKWebViewConfiguration>> {
        let mtm = objc2::MainThreadMarker::new()?;
        // SAFETY: plain constructors and a setter, on the main thread.
        unsafe {
            let store = self.private_data.get_or_init(|| {
                let store = objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm);
                // Before it's used: see `legacy_http`.
                legacy_http::route(&store);
                store
            });
            let configuration = objc2_web_kit::WKWebViewConfiguration::new(mtm);
            configuration.setWebsiteDataStore(store);
            Some(configuration)
        }
    }

    /// A WebKit view for tab `id`, configured from the current settings.
    fn create_webview(
        &self,
        id: u64,
        url: &str,
        private: bool,
        suspend_media: bool,
        window: &Window,
    ) -> Result<Rc<WebView>, wry::Error> {
        self.create_webview_in(id, url, private, None, suspend_media, window)
    }

    /// [`Self::create_webview`], keeping its data in `store` if given.
    fn create_webview_in(
        &self,
        id: u64,
        url: &str,
        private: bool,
        store: Option<&objc2_web_kit::WKWebsiteDataStore>,
        suspend_media: bool,
        window: &Window,
    ) -> Result<Rc<WebView>, wry::Error> {
        let settings = &self.settings;
        // The page's events go to whichever window holds the tab, which
        // changes when it's dragged to another.
        let route = self.route(id);
        let title_sender = route.clone();
        let load_sender = route.clone();
        let popup_sender = route.clone();
        let upgrade_sender = route.clone();
        let editing_sender = route.clone();
        let auth_sender = route.clone();
        // The HTTPS address last tried instead of an HTTP one, in this tab.
        // Forgotten once a page loads, so a later visit to the same HTTP
        // address isn't taken for a redirect loop.
        let upgraded: Rc<RefCell<Option<String>>> = Rc::default();
        let upgrade_done = upgraded.clone();
        let started_sender = self.common.anywhere.clone();
        // Its end may come after the tab, or its window, has gone.
        let finished_sender = self.common.anywhere.clone();
        let view_token = self.common.next_view.get() + 1;
        self.common.next_view.set(view_token);
        let weak_view: Rc<RefCell<std::rc::Weak<WebView>>> = Rc::default();
        let started_view = weak_view.clone();
        let keep_on_start = self.common.download_keepers.clone();
        let release_on_finish = self.common.download_keepers.clone();
        let live = &self.common.live;
        let https_only = live.https_only.clone();
        let download_dir = live.download_dir.clone();
        let reserved = live.reserved_downloads.clone();
        let released = live.reserved_downloads.clone();
        let permissions = live.permissions.clone();
        // Extensions don't run in private tabs, as in Firefox by default.
        let extensions = self.common.extensions.borrow();
        let own_store = store.and_then(|store| {
            let mtm = objc2::MainThreadMarker::new()?;
            // SAFETY: a plain constructor and setter, on the main thread.
            unsafe {
                let configuration = objc2_web_kit::WKWebViewConfiguration::new(mtm);
                configuration.setWebsiteDataStore(store);
                Some(configuration)
            }
        });
        let builder = match (extensions.as_ref(), private) {
            _ if own_store.is_some() => {
                WebViewBuilder::new().with_webview_configuration(own_store.expect("checked"))
            }
            (Some(extensions), false) => WebViewBuilder::new()
                .with_webview_configuration(extensions.webview_configuration_for(url)),
            (_, true) => match self.private_configuration() {
                Some(configuration) => WebViewBuilder::new().with_webview_configuration(configuration),
                None => WebViewBuilder::new(),
            },
            _ => WebViewBuilder::new(),
        };
        // Wry starts its initial navigation while building the view. Install
        // our WebKit response policy and content rules before the real URL;
        // otherwise the first load can be treated as a download (notably on
        // YouTube) and leave an empty page until Reload.
        let mut builder = builder
            .with_visible(false)
            .with_back_forward_navigation_gestures(settings.swipe_navigation)
            .with_accept_first_mouse(true)
            .with_incognito(private)
            .with_autoplay(!settings.block_autoplay)
            .with_devtools(settings.web_inspector)
            .with_initialization_script(EDITING_SCRIPT)
            .with_initialization_script(URL_CHANGE_SCRIPT)
            .with_initialization_script(X_EDITOR_SCRIPT)
            .with_ipc_handler(move |request| {
                if let Some(state) = request.body().strip_prefix("editing:") {
                    editing_sender.send(BrowserEvent::PageEditing(id, state == "1"));
                } else if let Some(payload) = request.body().strip_prefix("http-auth:")
                    && let Ok([user, password]) = serde_json::from_str::<[String; 2]>(payload)
                {
                    editing_sender.send(BrowserEvent::AuthSubmitted(id, user, password));
                } else if request.body() == "url-changed" {
                    editing_sender.send(BrowserEvent::UrlChanged(id));
                }
            })
            .with_document_title_changed_handler(move |title| {
                title_sender.send(BrowserEvent::Title(id, title));
            })
            .with_on_page_load_handler(move |event, url| {
                if matches!(event, PageLoadEvent::Finished) {
                    upgrade_done.borrow_mut().take();
                }
                load_sender.send(match event {
                    PageLoadEvent::Started => BrowserEvent::LoadStarted(id),
                    PageLoadEvent::Finished => BrowserEvent::Loaded(id, url),
                });
            })
            .with_new_window_req_handler(move |url, _| {
                popup_sender.send(BrowserEvent::Popup(id, url));
                NewWindowResponse::Deny
            })
            .with_navigation_handler(move |url| {
                if https_only.get() {
                    match navigation::https_only_action(
                        &url, navigation::main_frame_load(), navigation::page_load(),
                        &mut upgraded.borrow_mut(),
                    ) {
                        navigation::HttpsAction::Allow => {}
                        navigation::HttpsAction::Upgrade(secure) => {
                            upgrade_sender.send(BrowserEvent::Upgrade(id, secure));
                            return false;
                        }
                        navigation::HttpsAction::Block(reason) => {
                            upgrade_sender.send(BrowserEvent::Notice(reason.into()));
                            return false;
                        }
                    }
                }
                true
            })
            .with_download_started_handler(move |url, path| {
                let dir = download_dir.borrow().clone();
                let _ = std::fs::create_dir_all(&dir);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "download".into());
                *path = downloads::unique_path(&dir, &name, &reserved.borrow());
                reserved.borrow_mut().insert(path.clone(), url.clone());
                if let Some(view) = started_view.borrow().upgrade() {
                    let mut keepers = keep_on_start.borrow_mut();
                    let keeper = keepers.entry(view_token).or_insert((view, 0));
                    keeper.1 += 1;
                }
                let _ = started_sender.try_send(BrowserEvent::DownloadStarted(url, path.clone(), private));
                true
            })
            .with_download_completed_handler(move |url, path, success| {
                let mut keepers = release_on_finish.borrow_mut();
                if let Some((_, count)) = keepers.get_mut(&view_token) {
                    *count -= 1;
                    if *count == 0 {
                        keepers.remove(&view_token);
                    }
                }
                drop(keepers);
                let mut released = released.borrow_mut();
                if let Some(path) = &path {
                    released.remove(path);
                }
                // Wry reports no path on macOS even for successful downloads.
                // Keep ambiguous reservations until this run ends; releasing
                // one by URL could reuse another in-progress download's path.
                drop(released);
                let _ = finished_sender.try_send(BrowserEvent::DownloadFinished(url, path, success));
            })
            .with_permission_handler(move |kind| {
                let slot = match kind {
                    PermissionKind::Camera => 0,
                    PermissionKind::Microphone => 1,
                    PermissionKind::DisplayCapture => 2,
                    _ => return PermissionResponse::Default,
                };
                match permissions[slot].load(Ordering::Relaxed) {
                    1 => PermissionResponse::Allow,
                    2 => PermissionResponse::Deny,
                    _ => PermissionResponse::Default,
                }
            });
        if !settings.javascript && url != "about:blank#http-auth" {
            builder = builder.with_javascript_disabled();
        }
        if let Some(agent) = settings.user_agent.string(&settings.custom_user_agent) {
            builder = builder.with_user_agent(agent);
        }
        if settings.global_privacy_control {
            builder = builder.with_initialization_script(privacy::GPC_SCRIPT);
        }
        let view = Rc::new(builder.build_as_child(window)?);
        *weak_view.borrow_mut() = Rc::downgrade(&view);
        // Wry's classes exist from the first web view on.
        navigation::install();
        pointer_lock::enable(&view.webview());
        let route = self.route(id);
        let failed_route = route.clone();
        navigation::open_new_tabs_with(&view.webview(), move |url, front| {
            route.send(BrowserEvent::LinkInNewTab(id, url, front));
        });
        navigation::on_failure(&view.webview(), move |failure| {
            failed_route.send(BrowserEvent::LoadFailed(id, failure));
        });
        navigation::on_authentication(&view.webview(), if private { self.serial } else { 0 }, move |challenge| {
            auth_sender.send(BrowserEvent::AuthChallenge(id, challenge));
        });
        self.rules().apply(&view.webview());
        // A sleeping muted tab must be quiet before its page starts loading.
        if self.tabs.iter().any(|tab| tab.id == id && tab.muted) {
            audio::set_muted(&view.webview(), true);
        }
        if (settings.page_zoom - 1.0).abs() > 0.001 {
            let _ = view.zoom(settings.page_zoom);
        }
        if suspend_media {
            // Do this before navigation: hidden WKWebViews can still autoplay.
            audio::set_suspended(&view.webview(), true);
        }
        if self.common.rules.borrow().ready() {
            view.load_url(url)?;
        } else {
            self.common.waiting_loads.borrow_mut().push((Rc::downgrade(&view), url.to_owned()));
        }
        Ok(view)
    }

    /// Opens a tab. Background tabs don't take the selection.
    fn open_tab(
        &mut self,
        target: TabTarget,
        private: bool,
        background: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.add_tab(target, private, background, window, cx);
    }

    /// [`Browser::open_tab`], returning the new tab's id.
    fn add_tab(
        &mut self,
        target: TabTarget,
        private: bool,
        background: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        // Everything in a private window is private.
        let private = private || self.private;
        let id = NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed);
        let media_suspended = background && !self.tabs.is_empty();
        let (page, url, view) = match target {
            TabTarget::Page(page) => (page, page.internal_url().to_owned(), None),
            TabTarget::Url(url) => {
                self.favicons().load(&url);
                match self.create_webview(id, &url, private, media_suspended, window) {
                    Ok(view) => {
                        self.report_opened(id, &view, private);
                        (Page::Web, url, Some(view))
                    }
                    Err(err) => {
                        eprintln!("Could not open WebKit tab: {err}");
                        return None;
                    }
                }
            }
        };
        let title = match page {
            Page::Web => site_name(&url),
            page => page.title().to_owned(),
        };
        // Tabs restored at launch are simply there; new ones grow in.
        if !self.tabs.is_empty() {
            self.arriving.insert(id);
        }
        let at = match self.settings.tab_placement {
            TabPlacement::AfterCurrent if !self.tabs.is_empty() => self.selected + 1,
            _ => self.tabs.len(),
        };
        self.tabs.insert(
            at,
            BrowserTab {
                id,
                title,
                url,
                page,
                private,
                zoom: self.settings.page_zoom,
                loading: None,
                view,
                playing_audio: false,
                muted: false,
                media_suspended: media_suspended && page == Page::Web,
                committed: None,
                editing: false,
                last_active: std::time::Instant::now(),
            },
        );
        if background && self.tabs.len() > 1 {
            if at <= self.selected {
                self.selected += 1;
            }
            self.persist_soon(cx);
            cx.notify();
        } else {
            self.select(at, cx);
        }
        Some(id)
    }

    /// Adds a tab at the end without loading it: a web page loads when the
    /// tab is first shown (see `wake_current`).
    fn push_unloaded_tab(&mut self, url: String, title: String, private: bool) {
        let private = private || self.private;
        let page = Page::from_internal_url(&url).unwrap_or(Page::Web);
        let url = if page == Page::Web { url } else { page.internal_url().to_owned() };
        let title = match page {
            Page::Web if !title.trim().is_empty() => title,
            Page::Web => site_name(&url),
            page => page.title().to_owned(),
        };
        self.favicons().load(&url);
        self.tabs.push(BrowserTab {
            id: NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed),
            title,
            url,
            page,
            private,
            zoom: self.settings.page_zoom,
            loading: None,
            view: None,
            playing_audio: false,
            muted: false,
            media_suspended: false,
            committed: None,
            editing: false,
            last_active: std::time::Instant::now(),
        });
    }

    /// Loads the tab in front if it's a web page that isn't loaded: one
    /// restored and not yet shown, or unloaded after going unused.
    fn wake_current(&mut self, window: &Window) {
        let index = self.selected;
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.page != Page::Web || tab.view.is_some() || tab.url.is_empty() {
            return;
        }
        let (id, url, private, zoom) = (tab.id, tab.url.clone(), tab.private, tab.zoom);
        match self.create_webview(id, &url, private, false, window) {
            Ok(view) => {
                self.report_opened(id, &view, private);
                let _ = view.set_visible(!self.palette_open.get());
                let _ = view.zoom(zoom);
                let tab = &mut self.tabs[index];
                tab.view = Some(view);
                tab.loading = Some((std::time::Instant::now(), None));
                self.report_updated(index);
            }
            Err(err) => eprintln!("Could not open WebKit tab: {err}"),
        }
    }

    /// Unloads background tabs left unused past the setting, unless they're
    /// playing media; each reloads when shown again.
    fn sleep_unused_tabs(&mut self) {
        let minutes = self.settings.sleep_tabs_after;
        if minutes == 0 {
            return;
        }
        let limit = Duration::from_secs(u64::from(minutes) * 60);
        for (index, tab) in self.tabs.iter().enumerate() {
            if index == self.selected || tab.last_active.elapsed() < limit {
                continue;
            }
            let Some(view) = &tab.view else {
                continue;
            };
            let id = tab.id;
            let route = self.route(id);
            let answer = RcBlock::new(move |state: objc2_web_kit::WKMediaPlaybackState| {
                if state != objc2_web_kit::WKMediaPlaybackState::Playing {
                    route.send(BrowserEvent::SleepTab(id));
                }
            });
            // SAFETY: a live web view, and a block of the documented type.
            unsafe { view.webview().requestMediaPlaybackStateWithCompletionHandler(&answer) };
        }
    }

    fn refresh_tab_audio(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for tab in &mut self.tabs {
            let playing = tab.page == Page::Web
                && tab.view.as_ref().is_some_and(|view| audio::playing(&view.webview()));
            if tab.playing_audio != playing {
                tab.playing_audio = playing;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn toggle_tab_mute(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id && tab.page == Page::Web) else {
            return;
        };
        tab.muted = !tab.muted;
        if let Some(view) = &tab.view {
            audio::set_muted(&view.webview(), tab.muted);
        }
        cx.notify();
    }

    /// Opens another window, once this one has finished what it's doing.
    fn open_window(&self, private: bool, cx: &mut Context<Self>) {
        let common = self.common.clone();
        cx.defer(move |cx| {
            open_browser_window(cx, common, private, None, false, None);
        });
    }

    /// Reopens the window closed last, if there's one to: in this window if
    /// it holds nothing yet but a new tab (as at launch), else in its own.
    fn reopen_closed_window(&mut self, cx: &mut Context<Self>) {
        // Past any with nothing left that can be reopened.
        let closed = loop {
            let Some(closed) = self.common.closed_windows.borrow_mut().pop() else {
                return;
            };
            if !restorable(closed.clone()).0.is_empty() {
                break closed;
            }
        };
        if self.untouched() {
            self.take_session(closed, cx);
            return;
        }
        let common = self.common.clone();
        cx.defer(move |cx| {
            open_browser_window(cx, common, false, Some(closed), false, None);
        });
    }

    /// Whether this window holds only a tab that's never been anywhere:
    /// the start page, a blank page, or the home page just opened.
    fn untouched(&self) -> bool {
        let [tab] = self.tabs.as_slice() else {
            return false;
        };
        if self.private || !self.recently_closed.is_empty() {
            return false;
        }
        match tab.page {
            // Not one that went to our page from a web page, whose history
            // it keeps.
            Page::Start => tab.view.is_none(),
            Page::Web => {
                // The home page may have redirected, within its site.
                let home = site_name(&self.settings.home_page);
                (tab.url == "about:blank" || (!home.is_empty() && site_name(&tab.url) == home))
                    // SAFETY: a plain property read on a live web view.
                    && tab.view.as_ref().is_none_or(|view| unsafe { !view.webview().canGoBack() })
            }
            _ => false,
        }
    }

    /// Puts a closed window's tabs in place of this window's.
    fn take_session(&mut self, saved: state::SavedWindow, cx: &mut Context<Self>) {
        let (restored, selected) = restorable(saved);
        if restored.is_empty() {
            return;
        }
        for tab in std::mem::take(&mut self.tabs) {
            if let Some(view) = &tab.view {
                let _ = view.set_visible(false);
                navigation::forget(&view.webview());
            }
            if let Some(extensions) = self.common.extensions.borrow_mut().as_mut() {
                extensions.tab_closed(tab.id);
            }
            self.common.routes.borrow_mut().remove(&tab.id);
        }
        for (url, title) in restored {
            self.push_unloaded_tab(url, title, false);
        }
        self.selected = 0;
        self.select(selected.min(self.tabs.len() - 1), cx);
    }

    /// What a new tab opens to, as the settings say.
    fn new_tab_target(&self) -> TabTarget {
        match self.settings.new_tab_page {
            NewTabPage::StartPage => TabTarget::Page(Page::Start),
            NewTabPage::Home => TabTarget::Url(self.settings.home_page.clone()),
            NewTabPage::Blank => TabTarget::Url("about:blank".into()),
        }
    }

    fn new_tab(&mut self, private: bool, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.new_tab_target();
        self.open_tab(target, private, false, window, cx);
        self.focus_address(window, cx);
    }

    /// Shows an internal page, reusing a tab already showing it.
    fn open_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.iter().position(|tab| tab.page == page) {
            Some(index) => self.select(index, cx),
            None => self.open_tab(TabTarget::Page(page), false, false, window, cx),
        }
    }

    fn open_settings(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_section = section;
        self.notice = None;
        self.open_page(Page::Settings, window, cx);
    }

    fn open_link(&mut self, url: &str, place: Place, window: &mut Window, cx: &mut Context<Self>) {
        let private = self.current().private;
        match place {
            Place::Here => self.navigate(url, window, cx),
            Place::NewTab => {
                self.open_tab(TabTarget::Url(url.to_owned()), private, false, window, cx)
            }
            Place::BackgroundTab => {
                self.open_tab(TabTarget::Url(url.to_owned()), private, true, window, cx)
            }
            Place::PrivateTab => {
                self.open_tab(TabTarget::Url(url.to_owned()), true, false, window, cx)
            }
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        // The tab left behind counts as used until now.
        let now = std::time::Instant::now();
        if let Some(previous) = self.tabs.get_mut(self.selected) {
            previous.last_active = now;
        }
        self.tabs[index].last_active = now;
        self.selected = index;
        if self.tabs[index].media_suspended {
            if let Some(view) = &self.tabs[index].view {
                audio::set_suspended(&view.webview(), false);
            }
            self.tabs[index].media_suspended = false;
        }
        self.selection_generation += 1;
        self.reveal_selected = true;
        let palette_open = self.palette_open.get();
        for (i, tab) in self.tabs.iter().enumerate() {
            if let Some(view) = &tab.view {
                let _ = view.set_visible(i == index && tab.page == Page::Web && !palette_open);
            }
        }
        self.close_suggestions(cx);
        self.close_bookmark_menu(cx);
        self.reset_address(cx);
        // The find bar searches the tab now in front.
        if self.find.open {
            self.find_step(false, cx);
        }
        let id = self.tabs[index].id;
        if let Some(extensions) = self.common.extensions.borrow_mut().as_mut() {
            extensions.tab_activated(id);
        }
        // A page of our own: a hidden web page mustn't keep the keyboard.
        if self.tabs[index].page != Page::Web {
            keyboard_to_gpui(self.ns_window, self.ns_view);
        }
        self.offer_handoff();
        self.apply_theme();
        // Soon rather than now: WebKit waits on the main thread at each step
        // of a load, and this runs at every tab switch and navigation.
        self.persist_soon(cx);
        cx.notify();
    }

    /// What the address field shows for a tab: internal pages show nothing,
    /// so typing starts fresh.
    fn address_text(&self, index: usize) -> String {
        let tab = &self.tabs[index];
        match tab.page {
            Page::Web if tab.url != "about:blank" => tab.url.clone(),
            Page::Web | Page::Start => String::new(),
            Page::Downloads | Page::Bookmarks => tab.page.internal_url().to_owned(),
            Page::Settings => match self.settings_section {
                Section::General => Page::Settings.internal_url().to_owned(),
                section => format!("vamp://settings/{}", section.slug()),
            },
        }
    }

    /// The colour a page on `url`'s site gives the window: the page's own,
    /// else its icon's, else none.
    fn site_tint(&self, url: &str) -> Option<Tint> {
        let key = favicon::site_key(url)?;
        self.site_tints
            .get(&key)
            .copied()
            .flatten()
            .or_else(|| self.favicons().get_by_key(&key)?.tint)
    }

    /// Sets the theme from the settings and the active tab: its colour
    /// glides to the page's, a private tab's deep violet, a fixed hue, or
    /// grey. A tab with no colour drains the window to grey without swinging
    /// its hue, so the next coloured tab doesn't arrive via a detour.
    fn apply_theme(&mut self) {
        let settings = &self.settings;
        // A window closing with its last tab, which other windows can
        // still reach for a moment.
        let Some(tab) = self.tabs.get(self.selected) else {
            return;
        };
        let tint = if tab.private {
            Some(Tint {
                hue: 290.0,
                saturation: 1.0,
            })
        } else {
            match settings.tint {
                TintMode::Page if tab.page == Page::Web => self.site_tint(&tab.url),
                TintMode::Page | TintMode::Neutral => None,
                TintMode::Fixed => Some(Tint {
                    hue: settings.fixed_hue,
                    saturation: 1.0,
                }),
            }
        };
        let intensity = settings.intensity;
        let theme = &mut self.controls.theme;
        theme.scheme = match settings.scheme {
            SchemeChoice::System => vampir::Scheme::System,
            SchemeChoice::Light => vampir::Scheme::Light,
            SchemeChoice::Dark => vampir::Scheme::Dark,
        };
        match tint {
            Some(tint) => {
                theme.hue = tint.hue;
                theme.saturation = tint.saturation * intensity;
            }
            None => theme.saturation = NEUTRAL_SATURATION * intensity.min(1.0),
        }
    }

    /// Asks the page which icons it links to; the answer arrives as an
    /// `Icons` event.
    fn discover_icons(&self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let Some(view) = &tab.view else {
            return;
        };
        let id = tab.id;
        let route = self.route(id);
        let _ = view.evaluate_script_with_callback(favicon::DISCOVER_SCRIPT, move |result| {
            route.send(BrowserEvent::Icons(id, result));
        });
    }

    /// Gets `url`'s site icon from memory, disk or the network; a fetch
    /// reports back as a `Favicon` event.
    fn want_favicon(&mut self, url: &str, candidates: Option<Vec<favicon::Candidate>>) {
        let sender = self.common.anywhere.clone();
        self.favicons().want(url, candidates, move |key, png| {
            let _ = sender.try_send(BrowserEvent::Favicon(key, png));
        });
    }

    /// Asks the page for its colour; the answer arrives as a `Tint` event.
    fn sample_tint(&self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let Some(view) = &tab.view else {
            return;
        };
        // Keyed by the site asked, so an answer arriving after the tab has
        // moved on can't colour the wrong page.
        let Some(key) = favicon::site_key(&tab.url) else {
            return;
        };
        // To wherever the tab is by then.
        let route = self.route(tab.id);
        let _ = view.evaluate_script_with_callback(tint::SAMPLE_SCRIPT, move |result| {
            route.send(BrowserEvent::Tint(key.clone(), tint::tint_from_sample(&result)));
        });
    }

    /// Recompiles the content rules after a privacy setting changes, then
    /// applies them to every tab.
    fn update_rules(&mut self) {
        let sender = self.common.anywhere.clone();
        self.rules().update(&self.settings, move || {
            let _ = sender.try_send(BrowserEvent::Command(Command::ApplyRules));
        });
    }

    /// A page that couldn't load. An old device's server that answered
    /// without a status line is reached through [`legacy_http`] from now
    /// on, and the page tried again; anything else shows what went wrong,
    /// with a way to try again, rather than a blank page.
    fn load_failed(
        &mut self,
        index: usize,
        failure: navigation::LoadFailure,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let url_error = failure.domain == "NSURLErrorDomain";
        // Stopped, or replaced by another load; or a download, or a
        // plug-in, taking over.
        if (url_error && failure.code == -999)
            || (failure.domain == "WebKitErrorDomain" && matches!(failure.code, 102 | 204))
        {
            return;
        }
        let Some(view) = self.tabs[index].view.clone() else {
            return;
        };
        // NSURLErrorCannotParseResponse, from a store not yet sending that
        // host through the proxy: the page again, in one that does.
        if url_error
            && failure.code == -1017
            && let Ok(parsed) = url::Url::parse(&failure.url)
            && parsed.scheme() == "http"
            && let Some(host) = parsed.host_str().map(str::to_owned)
            // SAFETY: plain property reads on a live web view.
            && !legacy_http::reaches(&*unsafe { view.webview().configuration().websiteDataStore() }, &host)
        {
            if legacy_http::add(&host) || self.common.legacy_store.borrow().is_none() {
                if let Some(mtm) = objc2::MainThreadMarker::new() {
                    *self.common.legacy_store.borrow_mut() = Some(legacy_http::fresh_store(mtm));
                }
            }
            // A private tab's in a store of its own, shared with nothing.
            let store = if self.tabs[index].private {
                objc2::MainThreadMarker::new().map(legacy_http::fresh_store)
            } else {
                self.common.legacy_store.borrow().clone()
            };
            if let Some(store) = store {
                self.reload_in_store(index, &failure.url, &store, window);
                return;
            }
        }
        let html = error_page(&failure);
        let webview = view.webview();
        // SAFETY: WebKit's own method, checked for; the page shows under the
        // address that failed, and Reload tries that address again.
        unsafe {
            let html = objc2_foundation::NSString::from_str(&html);
            let unreachable = objc2_foundation::NSURL::URLWithString(&objc2_foundation::NSString::from_str(&failure.url));
            let alternate = objc2::sel!(_loadAlternateHTMLString:baseURL:forUnreachableURL:);
            if let Some(unreachable) = unreachable.as_deref()
                && objc2_foundation::NSObjectProtocol::respondsToSelector(&*webview, alternate)
            {
                let _: () = objc2::msg_send![&*webview, _loadAlternateHTMLString: &*html, baseURL: unreachable, forUnreachableURL: unreachable];
            } else {
                webview.loadHTMLString_baseURL(&html, unreachable.as_deref());
            }
        }
        cx.notify();
    }

    /// Opens a real web document at the challenged origin. Extensions can
    /// inspect and fill its form while the original WebKit request waits.
    fn show_auth_form(
        &mut self,
        original: u64,
        challenge: navigation::AuthChallenge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.web_index_of(original) else { return };
        let Some(origin) = http_auth::origin(&challenge) else {
            if let Some(view) = &self.tabs[index].view { navigation::cancel_auth(&view.webview()); }
            return;
        };
        // A retry replaces the old form without cancelling the new challenge.
        if let Some(form) = self.auth_forms.iter().find_map(|(form, target)| (*target == original).then_some(*form)) {
            self.auth_forms.remove(&form);
            if let Some(index) = self.web_index_of(form) {
                self.close_tab(index, window, cx);
            }
        }
        let Some(index) = self.web_index_of(original) else { return };
        let private = self.tabs[index].private;
        let Some(form) = self.add_tab(TabTarget::Url("about:blank#http-auth".into()), private, false, window, cx) else {
            if let Some(view) = &self.tabs[index].view { navigation::cancel_auth(&view.webview()); }
            return;
        };
        self.auth_forms.insert(form, original);
        self.auth_tabs.insert(form);
        let Some(index) = self.web_index_of(form) else { return };
        self.tabs[index].url = origin.clone();
        self.tabs[index].title = format!("Sign in to {}", challenge.host);
        let Some(view) = &self.tabs[index].view else { return };
        let html = objc2_foundation::NSString::from_str(&http_auth::page(&challenge));
        let Some(url) = objc2_foundation::NSURL::URLWithString(&objc2_foundation::NSString::from_str(&origin)) else { return };
        let request = objc2_foundation::NSURLRequest::requestWithURL(&url);
        // SAFETY: WebKit's public API loads the supplied HTML as the
        // response for this HTTPS or HTTP request, without a network fetch.
        unsafe {
            let _: Option<Retained<objc2::runtime::AnyObject>> = objc2::msg_send![
                &*view.webview(), loadSimulatedRequest: &*request, responseHTMLString: &*html
            ];
        }
        self.report_updated(index);
        self.reset_address(cx);
        cx.notify();
    }

    /// Gives tab `index` a new web view keeping its data in `store`, and
    /// loads `url` there.
    fn reload_in_store(
        &mut self,
        index: usize,
        url: &str,
        store: &objc2_web_kit::WKWebsiteDataStore,
        window: &Window,
    ) {
        let (id, private, zoom) = {
            let tab = &self.tabs[index];
            (tab.id, tab.private, tab.zoom)
        };
        let suspended = self.tabs[index].media_suspended;
        let view = match self.create_webview_in(id, url, private, Some(store), suspended, window) {
            Ok(view) => view,
            Err(err) => {
                eprintln!("Could not open WebKit tab: {err}");
                return;
            }
        };
        if let Some(old) = self.tabs[index].view.replace(view.clone()) {
            let _ = old.set_visible(false);
            navigation::forget(&old.webview());
            if let Some(extensions) = self.common.extensions.borrow_mut().as_mut() {
                extensions.tab_closed(id);
            }
        }
        let _ = view.zoom(zoom);
        let _ = view.set_visible(index == self.selected && !self.palette_open.get());
        self.tabs[index].loading = Some((std::time::Instant::now(), None));
    }

    /// Loads the pages waiting for the content rules, once they're ready.
    fn release_waiting_loads(&self) {
        if !self.common.rules.borrow().ready() {
            return;
        }
        let waiting = std::mem::take(&mut *self.common.waiting_loads.borrow_mut());
        for (view, url) in waiting {
            let Some(view) = view.upgrade() else {
                continue;
            };
            // Unless it's been sent somewhere since.
            // SAFETY: a plain property read on a live web view.
            if unsafe { view.webview().URL() }.is_some() {
                continue;
            }
            self.rules().apply(&view.webview());
            if let Err(err) = view.load_url(&url) {
                eprintln!("Could not load {url}: {err}");
            }
        }
    }

    fn apply_rules(&self) {
        for view in self.tabs.iter().filter_map(|tab| tab.view.as_ref()) {
            self.rules().apply(&view.webview());
        }
    }

    /// The shared rules changed: every window's tabs take them.
    fn apply_rules_everywhere(&self, cx: &mut Context<Self>) {
        self.apply_rules();
        self.for_other_windows(cx, |browser, _| browser.apply_rules());
    }

    /// Saves settings after the settings page changes them, and brings
    /// everything that depends on them up to date.
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        self.settings.normalise();
        if let Err(err) = self.settings.save() {
            eprintln!("Could not save settings: {err}");
        }
        // One set for every window's pages, whichever window made them.
        self.common.live.update(&self.settings);
        self.update_rules();
        self.apply_theme();
        // Every window follows the same settings.
        let settings = self.settings.clone();
        self.for_other_windows(cx, |browser, cx| {
            browser.settings = settings.clone();
            browser.apply_theme();
            cx.notify();
        });
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f64, cx: &mut Context<Self>) {
        let default = self.settings.page_zoom;
        let tab = &mut self.tabs[self.selected];
        tab.zoom = if factor == 0.0 {
            default
        } else {
            (tab.zoom * factor).clamp(0.3, 5.0)
        };
        if let Some(view) = &tab.view {
            let _ = view.zoom(tab.zoom);
        }
        cx.notify();
    }

    fn toggle_vertical_tabs(&mut self, cx: &mut Context<Self>) {
        self.vertical_tabs = !self.vertical_tabs;
        self.reveal_selected = true;
        self.persist();
        cx.notify();
    }

    fn toggle_compact_vertical_tabs(&mut self, cx: &mut Context<Self>) {
        self.compact_vertical_tabs = !self.compact_vertical_tabs;
        self.persist();
        cx.notify();
    }

    fn toggle_bookmarks_bar(&mut self, cx: &mut Context<Self>) {
        self.bookmarks_bar = !self.bookmarks_bar;
        self.persist();
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let id = self.tabs[index].id;
        if let Some(form) = self.auth_forms.iter().find_map(|(form, original)| (*original == id).then_some(*form))
            && let Some(form_index) = self.index_of(form)
        {
            self.close_tab(form_index, window, cx);
            if let Some(index) = self.index_of(id) { self.close_tab(index, window, cx); }
            return;
        }
        // Removing a hovered tab does not reliably deliver its hover-exit
        // event, especially when the pointer stays over the tab strip.
        self.dismiss_hint();
        if self.hovered_label == Some(self.tabs[index].id) {
            self.hovered_label = None;
        }
        self.label_widths.borrow_mut().remove(&self.tabs[index].id);
        let width = self.strip_width_of(self.tabs[index].id);
        let tab = self.tabs.remove(index);
        let auth_form = self.auth_tabs.remove(&tab.id);
        if let Some(original) = self.auth_forms.remove(&tab.id)
            && let Some(original_index) = self.web_index_of(original)
            && let Some(view) = &self.tabs[original_index].view
        {
            navigation::cancel_auth(&view.webview());
        }
        self.auth_forms.retain(|_, original| *original != tab.id);
        if let Some(view) = &tab.view {
            let _ = view.set_visible(false);
            navigation::forget(&view.webview());
        }
        if let Some(extensions) = self.common.extensions.borrow_mut().as_mut() {
            extensions.tab_closed(tab.id);
        }
        self.arriving.remove(&tab.id);
        self.closing.push(ClosingTab {
            id: tab.id,
            prev: index.checked_sub(1).map(|prev| self.tabs[prev].id),
            title: tab.title.clone(),
            url: tab.url.clone(),
            width,
        });
        if !auth_form {
            self.recently_closed.push(ClosedTab {
                url: tab.url.clone(),
                page: tab.page,
                private: tab.private,
            });
        }
        if self.recently_closed.len() > 25 {
            self.recently_closed.remove(0);
        }
        self.common.routes.borrow_mut().remove(&tab.id);
        drop(tab);
        if self.tabs.is_empty() {
            // With other windows open, the window goes with its last tab.
            // Closed now rather than when next drawn: a hidden window isn't
            // drawn, and would linger tabless where other windows reach it.
            if self.common.browsers().len() > 1 {
                self.leave_windows(cx);
                window.remove_window();
            } else {
                self.new_tab(false, window, cx);
            }
        } else {
            let next = if index < self.selected {
                self.selected - 1
            } else {
                self.selected
            };
            self.select(next.min(self.tabs.len() - 1), cx);
        }
    }

    /// Closes a tab from its × or a middle click. In the horizontal strip
    /// the tabs keep their width until the pointer leaves, like Safari's:
    /// the next tab slides under the pointer rather than everything
    /// reflowing beneath it.
    ///
    /// By id, looked up when clicked: tabs may have come or gone since the
    /// frame the click landed on was drawn.
    fn close_tab_by_pointer(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        if !self.vertical_tabs && self.tab_freeze.is_none() {
            self.tab_freeze = Some(self.strip_width_of(id));
        }
        self.close_tab(index, window, cx);
    }

    /// Selects tab `id`, wherever it is now.
    fn select_id(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(index) = self.index_of(id) {
            self.select(index, cx);
        }
    }

    /// A tab's laid-out width in the horizontal strip.
    fn strip_width_of(&self, id: u64) -> f32 {
        self.strip_children
            .iter()
            .position(|child| *child == id)
            .and_then(|child| self.tab_scroll.bounds_for_item(child))
            .map_or(TAB_MAX_WIDTH, |bounds| f32::from(bounds.size.width))
    }

    /// Loads what was typed, or a link, in the current tab. An internal
    /// page becomes a web tab.
    fn navigate(&mut self, text: &str, window: &Window, cx: &mut Context<Self>) {
        let url = settings::destination(text, &self.settings);
        // One of the browser's own pages already open elsewhere: go there,
        // leaving this tab's page be.
        if let Some((page, section)) = parse_internal(&url)
            && self.current().page != page
            && let Some(index) = self.tabs.iter().position(|t| t.page == page)
        {
            if let Some(section) = section {
                self.settings_section = section;
            }
            self.select(index, cx);
            return;
        }
        self.load_in(self.selected, &url, window, cx);
    }

    fn load_in(&mut self, index: usize, url: &str, window: &Window, cx: &mut Context<Self>) {
        if let Some((page, section)) = parse_internal(url) {
            if let Some(section) = section {
                self.settings_section = section;
            }
            let tab = &mut self.tabs[index];
            tab.page = page;
            tab.url = page.internal_url().to_owned();
            tab.title = page.title().to_owned();
            if let Some(view) = &tab.view {
                let _ = view.set_visible(false);
            }
            self.select(index, cx);
            return;
        }
        self.favicons().load(url);
        let id = self.tabs[index].id;
        let private = self.tabs[index].private;
        if self.tabs[index].view.is_none() {
            match self.create_webview(id, url, private, false, window) {
                Ok(view) => {
                    self.report_opened(id, &view, private);
                    self.tabs[index].view = Some(view);
                }
                Err(err) => {
                    eprintln!("Could not open WebKit tab: {err}");
                    return;
                }
            }
        } else if let Some(view) = &self.tabs[index].view
            && let Err(err) = view.load_url(url)
        {
            eprintln!("Could not load {url}: {err}");
        }
        let tab = &mut self.tabs[index];
        tab.page = Page::Web;
        tab.url = url.to_owned();
        tab.title = site_name(url);
        tab.loading = Some((std::time::Instant::now(), None));
        if index == self.selected {
            self.select(index, cx);
        }
        self.persist_soon(cx);
        cx.notify();
    }

    /// Opens URLs other apps handed over: web links and local files, each
    /// in a new tab.
    fn open_external(&mut self, urls: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        for url in urls {
            let Ok(parsed) = url::Url::parse(&url) else {
                continue;
            };
            match parsed.scheme() {
                "http" | "https" => {
                    // Reuse an empty start page rather than piling up tabs.
                    if self.current().page == Page::Start {
                        self.navigate(&url, window, cx);
                    } else {
                        self.open_tab(TabTarget::Url(url), false, false, window, cx);
                    }
                }
                "file" => self.open_tab(TabTarget::Url(url), false, false, window, cx),
                _ => {}
            }
        }
        cx.activate(true);
    }

    fn drain_events(&mut self, first: BrowserEvent, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut pending = Some(first);
        while let Some(event) = pending.take().or_else(|| self.events.try_recv().ok()) {
            // Closing, its last tab gone: nothing below has a tab to go by.
            if self.tabs.is_empty() {
                break;
            }
            // A page's event queued before its tab moved to another window
            // follows it there.
            if let Some(id) = event.page_tab()
                && self.index_of(id).is_none()
            {
                let route = self.common.routes.borrow().get(&id).cloned();
                if let Some(route) = route.filter(|route| !route.leads_to(&self.sender)) {
                    route.send(event);
                }
                continue;
            }
            match event {
                BrowserEvent::AddressEdited(text) => self.address_edited(text, window, cx),
                BrowserEvent::Suggestions(query, found) => {
                    self.remote_suggestions(query, found, window, cx)
                }
                BrowserEvent::SuggestSnapshot(tab, jpeg) => self.suggest_snapshot(tab, jpeg, cx),
                BrowserEvent::Address(text) => {
                    let target = self.submitted_address(text);
                    self.close_suggestions(cx);
                    self.navigate(&target, window, cx);
                    // Like Safari: once it's sent, the field lets go and the
                    // page takes the keyboard.
                    {
                        window.blur();
                        self.reset_address(cx);
                        if let Some(view) = &self.current().view
                            && self.current().page == Page::Web
                        {
                            let _ = view.focus();
                        }
                    }
                }
                BrowserEvent::Title(id, title) => {
                    if let Some(index) = self.web_index_of(id)
                        && !title.trim().is_empty()
                    {
                        let tab = &mut self.tabs[index];
                        tab.title = title.clone();
                        if !tab.private {
                            let url = tab.url.clone();
                            self.history().retitle(&url, &title);
                        }
                        self.report_updated(index);
                        if index == self.selected {
                            self.offer_handoff();
                        }
                        changed = true;
                    }
                }
                BrowserEvent::Loaded(id, url) => {
                    if self.auth_forms.contains_key(&id) && url.starts_with("about:blank") {
                        continue;
                    }
                    if let Some(index) = self.web_index_of(id) {
                        if let Some(&original) = self.auth_forms.get(&id)
                            && self.tabs[index].url != url
                        {
                            self.auth_forms.remove(&id);
                            self.auth_tabs.remove(&id);
                            if let Some(view) = self.web_index_of(original)
                                .and_then(|index| self.tabs[index].view.as_ref())
                            {
                                navigation::cancel_auth(&view.webview());
                            }
                        }
                        let auth_form = self.auth_forms.contains_key(&id);
                        self.tabs[index].url = url.clone();
                        // Don't clobber an address someone is typing.
                        if index == self.selected && !self.address_focused(window, cx) {
                            self.reset_address(cx);
                        }
                        let tab = &self.tabs[index];
                        if !tab.private && !auth_form && self.settings.remember_history {
                            let title = tab.title.clone();
                            // Written out every half minute, not every page.
                            self.history().record(&url, &title);
                        }
                        self.favicons().load(&url);
                        // A link may have led to another site.
                        if index == self.selected {
                            self.apply_theme();
                            self.offer_handoff();
                        }
                        self.sample_tint(index);
                        self.discover_icons(index);
                        self.report_updated(index);
                        changed = true;
                    }
                }
                BrowserEvent::UrlChanged(id) => {
                    if let Some(index) = self.web_index_of(id)
                        && let Some(view) = &self.tabs[index].view
                        && let Ok(url) = view.url()
                        && !url.is_empty()
                        && self.tabs[index].url != url
                    {
                        let _ = view.evaluate_script("window.__vamprowserReader?.close()");
                        self.tabs[index].url = url.clone();
                        if index == self.selected && !self.address_focused(window, cx) {
                            self.reset_address(cx);
                        }
                        let tab = &self.tabs[index];
                        if !tab.private && !self.auth_forms.contains_key(&id)
                            && self.settings.remember_history
                        {
                            let title = tab.title.clone();
                            self.history().record(&url, &title);
                        }
                        self.favicons().load(&url);
                        if index == self.selected {
                            self.apply_theme();
                            self.offer_handoff();
                        }
                        self.report_updated(index);
                        changed = true;
                    }
                }
                BrowserEvent::Icons(id, result) => {
                    // Private tabs' sites stay off the network and the disk.
                    if let Some(url) = self
                        .web_index_of(id)
                        .filter(|&index| !self.tabs[index].private)
                        .map(|index| self.tabs[index].url.clone())
                    {
                        self.want_favicon(&url, Some(favicon::parse_discovered(&result)));
                    }
                }
                BrowserEvent::Favicon(key, png) => {
                    // The page's own links, if they came while its HTML was
                    // being read and that found nothing.
                    let retry = self.favicons().fetched(key, png);
                    if let Some((url, candidates)) = retry {
                        self.want_favicon(&url, Some(candidates));
                    }
                    self.refresh_other_windows(cx);
                    self.apply_theme();
                    cx.notify();
                }
                BrowserEvent::Tint(key, tint) => {
                    self.site_tints.insert(key, tint);
                    self.apply_theme();
                    cx.notify();
                }
                BrowserEvent::Popup(id, url) => {
                    let is_web = matches!(url::Url::parse(&url), Ok(u) if matches!(u.scheme(), "http" | "https"));
                    // Only from a page still showing here: one closed, or
                    // moved to another window, can't say whether it was
                    // private, and mustn't be guessed not to be.
                    let Some(index) = self.web_index_of(id) else {
                        continue;
                    };
                    let private = self.tabs[index].private;
                    if is_web {
                        match self.settings.popups {
                            PopupPolicy::NewTab => {
                                self.open_tab(TabTarget::Url(url), private, false, window, cx)
                            }
                            PopupPolicy::BackgroundTab => {
                                self.open_tab(TabTarget::Url(url), private, true, window, cx)
                            }
                            PopupPolicy::Block => {}
                        }
                    }
                }
                BrowserEvent::LinkInNewTab(id, url, front) => {
                    // Beside the page it's from, private if that is.
                    let Some(index) = self.web_index_of(id) else {
                        continue;
                    };
                    let private = self.tabs[index].private;
                    self.open_tab(TabTarget::Url(url), private, !front, window, cx);
                }
                BrowserEvent::Upgrade(id, url) => {
                    if let Some(index) = self.web_index_of(id) {
                        self.load_in(index, &url, window, cx);
                    }
                }
                BrowserEvent::DownloadStarted(url, path, private) => {
                    self.downloads().started(url, path, private);
                    self.refresh_other_windows(cx);
                    cx.notify();
                }
                BrowserEvent::DownloadFinished(url, path, success) => {
                    self.downloads().finished(&url, path, success);
                    self.refresh_other_windows(cx);
                    cx.notify();
                }
                BrowserEvent::Snapshot(tab, jpeg) => self.palette_snapshot(tab, jpeg, cx),
                BrowserEvent::Extension(event) => self.extension_event(event, window, cx),
                BrowserEvent::SaveField(field) => self.save_field(field, window, cx),
                BrowserEvent::LoadStarted(id) => {
                    if let Some(index) = self.web_index_of(id) {
                        self.tabs[index].loading = Some((std::time::Instant::now(), None));
                        self.tabs[index].committed = Some(std::time::Instant::now());
                        self.tabs[index].editing = false;
                        cx.notify();
                    }
                }
                BrowserEvent::LoadFailed(id, failure) => {
                    if let Some(index) = self.web_index_of(id) {
                        self.load_failed(index, failure, window, cx);
                    }
                }
                BrowserEvent::AuthChallenge(id, challenge) => {
                    self.show_auth_form(id, challenge, window, cx);
                }
                BrowserEvent::AuthSubmitted(id, user, password) => {
                    if let Some(&original) = self.auth_forms.get(&id) {
                        let view = self.web_index_of(original)
                            .and_then(|index| self.tabs[index].view.clone());
                        if let Some(view) = view {
                            navigation::answer_auth(&view.webview(), &user, &password);
                        }
                        if let Some(index) = self.web_index_of(id) {
                            self.close_tab(index, window, cx);
                        }
                    }
                }
                BrowserEvent::PageEditing(id, editing) => {
                    if let Some(index) = self.web_index_of(id) {
                        self.tabs[index].editing = editing;
                    }
                }
                BrowserEvent::PageClicked => {
                    self.close_suggestions(cx);
                    // WebKit takes AppKit's first responder on the click, but
                    // GPUI can still keep a focused tab (and its arrow-key
                    // handler). Clear that focus whenever the page is clicked.
                    let address_focused = self.address_focused(window, cx);
                    window.blur();
                    if address_focused {
                        self.reset_address(cx);
                    }
                    cx.notify();
                }
                BrowserEvent::UblockRules(build, chunks, fingerprints, rules) => {
                    // Only if uBlock Origin is still running by the time the
                    // lists are ready, and nothing newer was asked for.
                    if self.common.ublock_state.borrow().is_some()
                        && build == self.common.ublock_build.get()
                    {
                        eprintln!(
                            "uBlock Origin: enforcing {rules} network rules in {} lists",
                            chunks.len()
                        );
                        self.set_ublock_rules(chunks, fingerprints);
                    }
                }
                BrowserEvent::ExtensionUpdates(report) => self.install_extension_updates(report, cx),
                BrowserEvent::SleepTab(id) => {
                    if let Some(index) = self.index_of(id)
                        && index != self.selected
                        && let Some(view) = self.tabs[index].view.take()
                    {
                        let _ = view.set_visible(false);
                        navigation::forget(&view.webview());
                        if let Some(extensions) = self.common.extensions.borrow_mut().as_mut() {
                            extensions.tab_closed(id);
                        }
                        self.tabs[index].loading = None;
                        self.tabs[index].playing_audio = false;
                        cx.notify();
                    }
                }
                BrowserEvent::FindEdited => self.find_step(false, cx),
                BrowserEvent::Found(tab, serial, found, count) => self.found(tab, serial, found, count, cx),
                BrowserEvent::Notice(text) => {
                    self.notice = Some(text);
                    cx.notify();
                }
                // Shown in Finder, as the notice only shows in settings.
                BrowserEvent::Exported(result) => {
                    self.notice = Some(match result {
                        Ok(path) => {
                            let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
                            format!("Exported to {}.", path.display())
                        }
                        Err(err) => {
                            let _ = app_dialog::open(
                                cx, self.controls.palette(), "Couldn't export your browsing data",
                                err.clone(), "OK", app_dialog::Kind::Alert,
                            );
                            err
                        }
                    });
                    cx.notify();
                }
                BrowserEvent::ImportStaged(result) => match result {
                    Ok(()) => {
                        sitedata::relaunch_after_quit();
                        cx.quit();
                    }
                    Err(err) => {
                        let _ = app_dialog::open(
                            cx, self.controls.palette(), "Couldn't import your browsing data",
                            err.clone(), "OK", app_dialog::Kind::Alert,
                        );
                        self.notice = Some(err);
                        cx.notify();
                    }
                },
                BrowserEvent::ImportChosen(path) => {
                    self.confirm_then(
                        "Import browsing data?".into(),
                        "Your logins, site data, bookmarks, history, settings and extensions are replaced with the archive's, and Vamprowser relaunches. What's here now is kept in “Vamprowser (before import)” beside the profile.".into(),
                        "Import and Relaunch", cx,
                        move |browser, cx| {
                            let sender = browser.common.anywhere.clone();
                            browser.notice = Some("Importing…".into());
                            std::thread::spawn(move || {
                                let result = sitedata::stage_import(&path);
                                let _ = sender.try_send(BrowserEvent::ImportStaged(result));
                            });
                            cx.notify();
                        },
                    );
                }
                BrowserEvent::ExtensionPrepared(result) => {
                    self.install_prepared_extension(result, cx);
                    self.refresh_other_windows(cx);
                }
                BrowserEvent::Command(command) => self.run(command, window, cx),
            }
        }
        // Now, not at the next frame: a key pressed in between would go by
        // what the field and the suggestions were before.
        self.sync_key_flags(window, cx);
        if changed {
            self.persist_soon(cx);
            cx.notify();
        }
    }

    /// Tells the key monitor whether we're typing and whether suggestions
    /// show, as it can't ask GPUI itself.
    fn sync_key_flags(&self, window: &Window, cx: &Context<Self>) {
        self.typing.set(self.text_field_focused(window, cx));
        self.suggesting.set(self.suggestions_open());
        self.removable.set(self.suggestion_removable());
        self.page_editing.set(self.tabs.get(self.selected).is_some_and(|tab| tab.editing));
        self.finding.set(self.find_focused(window, cx));
    }

    /// Saves the session a moment from now, once a burst of changes (titles
    /// ticking over, a window being dragged) has settled; a later call
    /// replaces the pending one.
    fn persist_soon(&mut self, cx: &mut Context<Self>) {
        self._persist_later = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(800)).await;
            let _ = this.update(cx, |browser, _| browser.persist());
        }));
    }

    /// Shows the live page in front again once nothing drawn over it (the
    /// switcher, the address field's suggestions, a bookmarks folder) is
    /// still open: one closing mustn't uncover the page over another.
    fn show_page_if_uncovered(&self) {
        let covered = self.palette.is_some() || self.suggest.is_some() || self.bookmark_menu.is_some();
        let Some(tab) = self.tabs.get(self.selected) else {
            return;
        };
        if let (Some(view), Page::Web, false) = (&tab.view, tab.page, covered) {
            let _ = view.set_visible(true);
        }
    }

    /// Redraws `delay` from now (or sooner, if a redraw is already due
    /// then), for what changes on its own but needn't be drawn every frame.
    fn redraw_soon(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let due = std::time::Instant::now() + delay;
        if self.redraw_later.as_ref().is_some_and(|(pending, _)| *pending <= due) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |browser, cx| {
                browser.redraw_later = None;
                cx.notify();
            });
        });
        self.redraw_later = Some((due, task));
    }

    fn current(&self) -> &BrowserTab {
        &self.tabs[self.selected]
    }

    fn persist(&self) {
        let (tabs, titles, selected) = {
            // Private tabs aren't restored; keep the selection pointing at
            // the same tab among those that are.
            let kept: Vec<(usize, &BrowserTab)> = self
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, tab)| !tab.private && !self.auth_tabs.contains(&tab.id))
                .collect();
            let selected = kept
                .iter()
                .position(|(i, _)| *i >= self.selected)
                .unwrap_or(0);
            (
                kept.iter()
                    .map(|(_, tab)| {
                        if tab.page == Page::Web {
                            tab.view
                                .as_ref()
                                .and_then(|view| view.url().ok())
                                .filter(|url| {
                                    !url.is_empty()
                                        && (url != "about:blank" || tab.url == "about:blank")
                                })
                                .unwrap_or_else(|| tab.url.clone())
                        } else {
                            tab.url.clone()
                        }
                    })
                    .collect(),
                kept.iter().map(|(_, tab)| tab.title.clone()).collect(),
                selected,
            )
        };
        if !self.private {
            let mut sessions = self.common.sessions.borrow_mut();
            let window = state::SavedWindow {
                tabs,
                titles,
                selected,
                bounds: self.window_bounds,
            };
            match sessions.iter_mut().find(|(serial, _)| *serial == self.serial) {
                Some(entry) => entry.1 = window,
                None => sessions.push((self.serial, window)),
            }
        }
        {
            let mut saved = self.common.saved.borrow_mut();
            saved.vertical_tabs = self.vertical_tabs;
            saved.compact_vertical_tabs = self.compact_vertical_tabs;
            saved.bookmarks_bar = self.bookmarks_bar;
        }
        self.common.save_state();
    }

    /// The current tab's load progress and the bar's opacity, while the
    /// bar shows: WebKit's estimate as it loads, then a short fade once
    /// it's done.
    fn load_progress(&mut self) -> Option<(f32, f32)> {
        const FADE: f32 = 0.35;
        let tab = &mut self.tabs[self.selected];
        if tab.page != Page::Web {
            return None;
        }
        let (start, finished) = tab.loading?;
        let view = tab.view.as_ref()?.webview();
        // SAFETY: plain property reads on a live web view, on the main thread.
        let (fraction, busy) = unsafe { (view.estimatedProgress() as f32, view.isLoading()) };
        match finished {
            None if busy => Some((fraction.max(0.1), 1.0)),
            None => {
                tab.loading = Some((start, Some(std::time::Instant::now())));
                Some((1.0, 1.0))
            }
            Some(at) => {
                let t = at.elapsed().as_secs_f32() / FADE;
                if t >= 1.0 {
                    tab.loading = None;
                    None
                } else {
                    Some((1.0, 1.0 - t))
                }
            }
        }
    }

    /// Asks first in a Vampir dialog, then does `action` if answered yes.
    fn confirm_then(
        &self,
        title: String,
        message: String,
        button: &'static str,
        cx: &mut Context<Self>,
        action: impl FnOnce(&mut Browser, &mut Context<Browser>) + 'static,
    ) {
        let receiver = app_dialog::open(
            cx, self.controls.palette(), title, message, button,
            app_dialog::Kind::Confirm,
        );
        cx.spawn(async move |this, cx| {
            if matches!(receiver.recv().await, Ok(Some(_))) {
                let _ = this.update(cx, action);
            }
        })
        .detach();
    }

    /// Removes every cookie, cache and bit of site storage from the shared
    /// store, whichever tabs are open, and the profile's copy of the jar.
    fn clear_website_data(&mut self, cx: &mut Context<Self>) {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        let sender = self.common.anywhere.clone();
        let done = RcBlock::new(move || {
            sitedata::finish_cookie_clear();
            let _ = sender.try_send(BrowserEvent::Notice("Cookies, caches and website data cleared.".into()));
        });
        sitedata::begin_cookie_clear();
        // SAFETY: the default store, on the main thread, with a block of the
        // documented type.
        unsafe {
            let store = objc2_web_kit::WKWebsiteDataStore::defaultDataStore(mtm);
            let types = objc2_web_kit::WKWebsiteDataStore::allWebsiteDataTypes(mtm);
            store.removeDataOfTypes_modifiedSince_completionHandler(
                &types,
                &objc2_foundation::NSDate::distantPast(),
                &done,
            );
        }
        self.notice = Some("Clearing…".into());
        cx.notify();
    }

    /// A message set where the settings page, which shows them, isn't in
    /// front: shown for a moment under the address field instead.
    fn show_notice_away_from_settings(&mut self, cx: &mut Context<Self>) {
        if self.notice.is_none() || self.current().page == Page::Settings {
            return;
        }
        let Some(anchor) = self.omnibox_bounds.get() else {
            return;
        };
        let Some(text) = self.notice.take() else {
            return;
        };
        hint::toast(self.ns_window, &text, anchor, self.controls.palette());
        self._toast_later = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(2800)).await;
            hint::hide_toast();
        }));
    }

    /// Stops the current page loading, and its bar.
    fn stop_loading(&mut self, cx: &mut Context<Self>) {
        let index = self.selected;
        let Some(view) = self.tabs[index].view.clone() else {
            return;
        };
        // SAFETY: a live web view, on the main thread.
        unsafe { view.webview().stopLoading() };
        if let Some((start, None)) = self.tabs[index].loading {
            self.tabs[index].loading = Some((start, Some(std::time::Instant::now())));
        }
        cx.notify();
    }

    /// Opens the Web Inspector on the current page, letting it be
    /// inspected first if the setting hadn't.
    fn show_web_inspector(&mut self) {
        let Some(view) = self.current().view.clone() else {
            return;
        };
        let webview = view.webview();
        // SAFETY: the same switches wry sets for its `devtools` option, on a
        // live web view on the main thread.
        unsafe {
            let preferences = webview.configuration().preferences();
            let on = objc2_foundation::NSNumber::new_bool(true);
            let key = objc2_foundation::NSString::from_str("developerExtrasEnabled");
            let _: () = objc2::msg_send![&*preferences, setValue: &*on, forKey: &*key];
            let _: () = objc2::msg_send![&*webview, setInspectable: true];
        }
        view.open_devtools();
    }

    /// Whether one of our own text fields has the keyboard.
    fn text_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.address.read(cx).focus_handle.is_focused(window)
            || self.find_focused(window, cx)
            || self
                .palette
                .as_ref()
                .is_some_and(|p| p.input.read(cx).focus_handle.is_focused(window))
    }

    /// The caret colour this moment of its blink: shown, then hidden, each
    /// for [`CARET_BLINK`], starting shown whenever it last moved.
    fn caret_color(&self, color: Rgba) -> Rgba {
        let phase = self.caret_epoch.elapsed().as_millis() / CARET_BLINK.as_millis();
        if phase.is_multiple_of(2) {
            color
        } else {
            color::with_alpha(color, 0.0)
        }
    }

    fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ping = (Some(PingTarget::Omnibox), self.ping.1 + 1);
        // The switcher would otherwise keep the arrows, Enter and Escape.
        self.close_palette(cx);
        self.caret_epoch = std::time::Instant::now();
        // Whichever page had the keyboard, even a hidden one from the tab
        // before, gives it up.
        keyboard_to_gpui(self.ns_window, self.ns_view);
        let focus = self.address.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        // While unfocused the field shows a styled label, not the input, so
        // select once the next frame has drawn the input.
        cx.on_next_frame(window, move |_, window, cx| {
            focus.dispatch_action(&vampir::text_input::SelectAll, window, cx);
        });
        cx.notify();
    }

    /// Fetches and compiles uBlock Origin's selected filter lists off the
    /// main thread; the rules come back as `UblockRules`. Each report from
    /// its bridge triggers this; unchanged lists come from cache, so it's
    /// cheap when nothing moved.
    fn refresh_ublock_filters(&mut self, state: filters::UblockState) {
        *self.common.ublock_state.borrow_mut() = Some(state.clone());
        let Some(dir) = state::data_path("Extensions").map(|d| d.join(filters::UBLOCK_ID)) else {
            return;
        };
        let sender = self.common.anywhere.clone();
        // Builds run side by side and finish in any order; numbered, so the
        // rules that take effect are from the latest report.
        let build = self.common.ublock_build.get() + 1;
        self.common.ublock_build.set(build);
        std::thread::spawn(move || {
            if let Some(chunks) = filters::build(&dir, &state) {
                let fingerprints = chunks.iter().map(|c| filters::fingerprint(c)).collect();
                let rules = chunks.iter().map(|c| c.matches("\"trigger\"").count()).sum();
                let _ = sender.try_send(BrowserEvent::UblockRules(build, chunks, fingerprints, rules));
            }
        });
    }

    fn set_ublock_rules(&mut self, chunks: Vec<String>, fingerprints: Vec<String>) {
        let sender = self.common.anywhere.clone();
        self.rules().set_ublock(chunks, fingerprints, move || {
            let _ = sender.try_send(BrowserEvent::Command(Command::ApplyRules));
        });
    }

    fn report_opened(&mut self, id: u64, view: &Rc<WebView>, private: bool) {
        if !private && let Some(extensions) = self.common.extensions.borrow_mut().as_mut() {
            extensions.tab_opened(id, &view.webview());
        }
    }

    /// Tells extensions a tab's title or address changed.
    fn report_updated(&mut self, index: usize) {
        let tab = &self.tabs[index];
        if let Some(extensions) = self.common.extensions.borrow_mut().as_mut()
            && tab.view.is_some()
            && !tab.private
        {
            extensions.tab_updated(tab.id, &tab.title, &tab.url);
        }
    }

    fn extension_event(
        &mut self,
        event: ExtensionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ExtensionEvent::OpenTab {
                url,
                active,
                request,
            } => {
                // Extensions open their own webkit-extension:// pages too,
                // so no scheme filter here.
                let target = if url.is_empty() {
                    TabTarget::Page(Page::Start)
                } else {
                    TabTarget::Url(url)
                };
                let opened = self.add_tab(target, false, !active, window, cx);
                if let Some(request) = request
                    && let Some(extensions) = self.common.extensions.borrow_mut().as_mut()
                {
                    extensions.answer_tab_request(request, opened);
                }
            }
            ExtensionEvent::CloseTab { tab_id } => {
                if let Some(index) = self.index_of(tab_id) {
                    self.close_tab(index, window, cx);
                }
            }
            ExtensionEvent::ActivateTab { tab_id } => {
                if let Some(index) = self.index_of(tab_id) {
                    self.select(index, cx);
                }
            }
            ExtensionEvent::NativeMessage {
                extension_id,
                application,
                message,
            } => {
                if extension_id == filters::UBLOCK_ID
                    && application == filters::BRIDGE_APP
                    && let Ok(state) = serde_json::from_str::<filters::UblockState>(&message)
                {
                    self.refresh_ublock_filters(state);
                }
            }
            ExtensionEvent::Changed => {
                // uBlock Origin switched off or removed: stop enforcing its
                // filters.
                let running = self
                    .common
                    .extensions
                    .borrow()
                    .as_ref()
                    .is_some_and(|e| e.is_enabled(filters::UBLOCK_ID));
                if !running && self.common.ublock_state.take().is_some() {
                    self.set_ublock_rules(Vec::new(), Vec::new());
                }
                // Extension errors go to stderr as well as the settings
                // page, once each, for whoever is running from a terminal.
                if let Some(extensions) = self.common.extensions.borrow().as_ref() {
                    for info in extensions.list() {
                        for error in info.errors {
                            if self
                                .logged_extension_errors
                                .insert(format!("{}: {error}", info.id))
                            {
                                eprintln!("[extension {}] {error}", info.id);
                            }
                        }
                    }
                }
                // Every window restored at launch, not only the one told.
                self.reload_after_extensions_load(cx);
                self.for_other_windows(cx, |browser, cx| browser.reload_after_extensions_load(cx));
                cx.notify();
            }
            ExtensionEvent::ActionChanged => cx.notify(),
            // Handled before it reaches a window.
            ExtensionEvent::Upgraded { .. } => {}
        }
    }

    /// WebKit loads extensions asynchronously, so pages restored at launch
    /// finish before their content scripts exist. Once the loads at launch
    /// have gone quiet, reload those pages, once.
    fn reload_after_extensions_load(&mut self, cx: &mut Context<Self>) {
        if self.reloaded_for_extensions || self.launched.elapsed() > Duration::from_secs(8) {
            return;
        }
        self.extension_loads += 1;
        let generation = self.extension_loads;
        let loaded = std::time::Instant::now();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let _ = this.update(cx, |browser, _| {
                if browser.extension_loads != generation || browser.reloaded_for_extensions {
                    return;
                }
                browser.reloaded_for_extensions = true;
                // Only pages that arrived before the extensions were ready:
                // one arriving after has their scripts, and one still on
                // its way will. Reloading those too loaded the first page
                // twice at every launch.
                for tab in browser
                    .tabs
                    .iter()
                    .filter(|t| !t.private && t.page == Page::Web)
                    .filter(|t| t.committed.is_some_and(|at| at < loaded))
                {
                    if let Some(view) = &tab.view {
                        let _ = view.reload();
                    }
                }
            });
        })
        .detach();
    }

    /// Runs an extension's toolbar action, showing its popup, if it has
    /// one, in a native popover under the button.
    fn extension_action(&mut self, id: &str) {
        let extensions_guard = self.common.extensions.borrow();
        let Some(extensions) = extensions_guard.as_ref() else {
            return;
        };
        let Some(anchor) = self.action_bounds.borrow().get(id).copied() else {
            return;
        };
        // SAFETY: GPUI's view in this window, alive while the browser is.
        let view = unsafe { &*(self.ns_view as *const objc2_app_kit::NSView) };
        let anchor = objc2_foundation::NSRect::new(
            objc2_foundation::NSPoint::new(
                f64::from(f32::from(anchor.origin.x)),
                f64::from(f32::from(anchor.origin.y)),
            ),
            objc2_foundation::NSSize::new(
                f64::from(f32::from(anchor.size.width)),
                f64::from(f32::from(anchor.size.height)),
            ),
        );
        extensions.perform_action(id, anchor, view);
    }

    /// Scrolls the tab strip the least it can to show the selected tab,
    /// clear of the end fade. False until the tab has been laid out.
    fn reveal_selected_tab(&self) -> bool {
        let handle = &self.tab_scroll;
        let id = self.tabs[self.selected].id;
        let Some(item) = self
            .strip_children
            .iter()
            .position(|child| *child == id)
            .and_then(|child| handle.bounds_for_item(child))
        else {
            return false;
        };
        let view = handle.bounds();
        if view.size.width <= px(0.0) {
            return false;
        }
        // Item bounds are where the tab would be unscrolled.
        let margin = px(FADE_REACH_HORIZONTAL);
        let offset = handle.offset();
        let mut x = offset.x;
        if item.left() + x < view.left() {
            x = view.left() - item.left();
        } else if item.right() + x > view.right() - margin {
            x = view.right() - margin - item.right();
        }
        let x = x.min(px(0.0)).max(-handle.max_offset().x);
        if x != offset.x {
            handle.set_offset(point(x, offset.y));
        }
        true
    }

    /// The horizontal tab strip's height this frame, gliding to or from
    /// nothing as vertical tabs are switched off or on.
    fn tab_strip_height(&self) -> f32 {
        let target = if self.vertical_tabs {
            0.0
        } else {
            TAB_STRIP_HEIGHT
        };
        self.controls.tween("tab-strip-height", target, LAYOUT_MOVE)
    }

    /// Wraps a scroll container, sized to fill the wrapper, so its content
    /// fades out at an edge with more beyond it instead of being sliced.
    /// Wider than the toolkit's fade: a tab cut at the edge should read as
    /// "more this way", not as a rendering slip. Horizontal strips are
    /// anchored at their start and fade only at their end, so they read as
    /// a row running off to the right rather than a centred carousel.
    fn faded(
        &self,
        id: &str,
        content: impl IntoElement,
        handle: &ScrollHandle,
        axis: ScrollAxis,
        surface: Rgba,
    ) -> Div {
        self.faded_if(true, id, content, handle, axis, surface)
    }

    /// [`Self::faded`], its fades kept away unless `fading`: for a strip
    /// that only ever overflows by a pixel or two while its tabs change
    /// width, where a fade would flicker on and off.
    fn faded_if(
        &self,
        fading: bool,
        id: &str,
        content: impl IntoElement,
        handle: &ScrollHandle,
        axis: ScrollAxis,
        surface: Rgba,
    ) -> Div {
        let (offset, max) = match axis {
            ScrollAxis::Vertical => (handle.offset().y, handle.max_offset().y),
            ScrollAxis::Horizontal => (handle.offset().x, handle.max_offset().x),
        };
        let (offset, max) = (f32::from(offset), f32::from(max));
        let wanted = |shown: bool| if fading && max > 0.5 && shown { 1.0 } else { 0.0 };
        let start_fades = axis == ScrollAxis::Vertical;
        let at_start = self.controls.tween(
            (id, "fade-start"),
            wanted(start_fades && offset < -0.5),
            SWITCH_SLIDE,
        );
        let at_end =
            self.controls
                .tween((id, "fade-end"), wanted(offset > -max + 0.5), SWITCH_SLIDE);
        let reach = match axis {
            ScrollAxis::Vertical => FADE_REACH_VERTICAL,
            ScrollAxis::Horizontal => FADE_REACH_HORIZONTAL,
        };
        let clear = color::with_alpha(surface, 0.0);
        let fade = |start: bool, opacity: f32| {
            // Solid against the edge, clear where the content is legible.
            let (near, far) = if start {
                (surface, clear)
            } else {
                (clear, surface)
            };
            let angle = match axis {
                ScrollAxis::Vertical => 180.0,
                ScrollAxis::Horizontal => 90.0,
            };
            let veil = div().absolute().opacity(opacity).bg(linear_gradient(
                angle,
                linear_color_stop(near, 0.0),
                linear_color_stop(far, 1.0),
            ));
            match (axis, start) {
                (ScrollAxis::Vertical, true) => veil.top_0().left_0().right_0().h(px(reach)),
                (ScrollAxis::Vertical, false) => veil.bottom_0().left_0().right_0().h(px(reach)),
                (ScrollAxis::Horizontal, true) => veil.left_0().top_0().bottom_0().w(px(reach)),
                (ScrollAxis::Horizontal, false) => veil.right_0().top_0().bottom_0().w(px(reach)),
            }
        };
        div()
            .relative()
            .child(content)
            .when(at_start > 0.01, |el| el.child(fade(true, at_start)))
            .when(at_end > 0.01, |el| el.child(fade(false, at_end)))
    }

    /// The top of the window: the traffic lights, then the toolbar items the
    /// settings list either side of the address field. Its empty space
    /// drags the window; right-clicking it customises it.
    fn toolbar(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let inset = if window.is_fullscreen() {
            14.0
        } else {
            TRAFFIC_LIGHT_INSET
        };
        let (left, right) = self.settings.toolbar_sides();
        let mut bar = div()
            .id("toolbar")
            .h(px(TOOLBAR_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.0))
            .pl(px(inset))
            .pr(px(14.0))
            .on_mouse_down(MouseButton::Left, |event: &MouseDownEvent, window, _| {
                if event.click_count >= 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    let items = this.toolbar_menu();
                    this.context_menu(event.position, items, window, cx);
                }),
            );
        for item in left {
            bar = bar.child(self.toolbar_item(item, palette, cx));
        }
        bar = bar.child(div().w(px(12.0)).flex_none());
        bar = bar.child(self.omnibox(palette, window, cx));
        bar = bar.child(div().w(px(12.0)).flex_none());
        for item in right {
            bar = bar.child(self.toolbar_item(item, palette, cx));
        }
        bar
    }

    fn toolbar_item(
        &mut self,
        item: ToolbarItem,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let web = self.current().page == Page::Web;
        let (glyph, hint, active, command): (Icon, Hint, bool, Command) = match item {
            ToolbarItem::Address => return div().into_any_element(),
            ToolbarItem::Extensions => return self.extension_buttons(palette, cx),
            ToolbarItem::Back => (
                Icon::Back,
                Hint::new("Back").shortcut("⌘←"),
                false,
                Command::Back,
            ),
            ToolbarItem::Forward => (
                Icon::Forward,
                Hint::new("Forward").shortcut("⌘→"),
                false,
                Command::Forward,
            ),
            // While the page loads, it stops it instead.
            ToolbarItem::Reload if matches!(self.current().loading, Some((_, None))) => (
                Icon::Close,
                Hint::new("Stop").shortcut("⌘."),
                false,
                Command::Stop,
            ),
            ToolbarItem::Reload => (
                Icon::Reload,
                Hint::new("Reload").shortcut("⌘R"),
                false,
                Command::Reload,
            ),
            ToolbarItem::Home => (
                Icon::Home,
                Hint::new("Home").shortcut("⌘⇧H"),
                false,
                Command::Home,
            ),
            ToolbarItem::NewTab => (
                Icon::Plus,
                Hint::new("New tab").shortcut("⌘T"),
                false,
                Command::NewTab,
            ),
            ToolbarItem::PrivateTab => (
                Icon::Private,
                Hint::new("New private tab"),
                false,
                Command::NewPrivateTab,
            ),
            ToolbarItem::BookmarksBar => (
                Icon::Bookmark,
                Hint::new("Bookmarks bar").shortcut("⌘⇧B"),
                self.bookmarks_bar,
                Command::ToggleBookmarksBar,
            ),
            ToolbarItem::Sidebar => (
                Icon::Sidebar,
                Hint::new("Vertical tabs").shortcut("⌘⇧L"),
                self.vertical_tabs,
                Command::ToggleVerticalTabs,
            ),
            ToolbarItem::CommandPalette => (
                Icon::Search,
                Hint::new("Switch tabs").shortcut("⌘K"),
                self.palette_open.get(),
                Command::SwitchTabs,
            ),
            ToolbarItem::CopyLink => (
                Icon::Link,
                Hint::new("Copy link").shortcut("⇧⌘C"),
                false,
                Command::CopyLink,
            ),
            ToolbarItem::Downloads => (
                Icon::Download,
                Hint::new("Downloads").shortcut("⌥⌘L"),
                self.current().page == Page::Downloads,
                Command::ShowDownloads,
            ),
            ToolbarItem::Settings => (
                Icon::Gear,
                Hint::new("Settings").shortcut("⌘,"),
                self.current().page == Page::Settings,
                Command::Settings(Section::General),
            ),
        };
        let needs_page = matches!(
            item,
            ToolbarItem::Back | ToolbarItem::Forward | ToolbarItem::Reload | ToolbarItem::CopyLink
        );
        // Back and forward only with somewhere to go.
        let history = self.current().view.as_ref().filter(|_| web).map(|view| {
            let view = view.webview();
            // SAFETY: plain property reads on a live web view.
            unsafe { (view.canGoBack(), view.canGoForward()) }
        });
        let enabled = match item {
            ToolbarItem::Back => history.is_some_and(|h| h.0),
            ToolbarItem::Forward => history.is_some_and(|h| h.1),
            _ => web || !needs_page,
        };
        let ink = if active {
            palette.soft_label
        } else {
            palette.text_secondary
        };
        let button = tool_button_inked(
            ("toolbar-item", item as usize),
            glyph,
            ink,
            hint,
            BUTTON_SIZE,
            active,
            enabled,
            palette,
            cx,
            move |this, window, cx| {
                if enabled {
                    this.run(command.clone(), window, cx)
                }
            },
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                let items = this.toolbar_item_menu(item);
                if !items.is_empty() {
                    this.context_menu(event.position, items, window, cx);
                }
            }),
        );
        // A ping when it acts, from a click or the keyboard alike: a ring of
        // the accent that swells out and fades.
        let (pinged, generation) = self.ping;
        if !pinged.is_some_and(|target| target.toolbar(item)) {
            return button.into_any_element();
        }
        let accent = palette.accent;
        div()
            .relative()
            .size(px(BUTTON_SIZE))
            .flex_none()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded(px(RADIUS))
                    .with_animation(
                        ("toolbar-ping", generation),
                        Animation::new(slowed(Duration::from_millis(520))).with_easing(|t: f32| 1.0 - (1.0 - t).powi(3)),
                        move |ring, t| {
                            let grow = px(5.0 * t);
                            ring.top(-grow)
                                .left(-grow)
                                .size(px(BUTTON_SIZE) + grow * 2.0)
                                .rounded(px(RADIUS) + grow)
                                .border_2()
                                .border_color(color::with_alpha(accent, 0.7 * (1.0 - t)))
                                .bg(color::with_alpha(accent, 0.3 * (1.0 - t)))
                        },
                    ),
            )
            .child(button)
            .into_any_element()
    }

    /// A button per enabled extension with a toolbar action: its icon and
    /// badge. Each records where it was drawn, so the popup can hang from it.
    fn extension_buttons(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let mut row = div().flex().flex_none().items_center().gap(px(2.0));
        let extensions_guard = self.common.extensions.borrow();
        let Some(extensions) = extensions_guard.as_ref() else {
            return row.into_any_element();
        };
        // Extensions don't run in private tabs, so their buttons don't show.
        if self.current().private {
            return row.into_any_element();
        }
        let chrome = Chrome::new(palette);
        for action in extensions.actions() {
            let id = action.extension_id.clone();
            let bounds = self.action_bounds.clone();
            let recorded = id.clone();
            let image = action.icon;
            let command = Command::ExtensionAction(id.clone());
            row = row.child(
                div()
                    .id(ElementId::Name(format!("extension-{id}").into()))
                    .occlude()
                    .relative()
                    .size(px(BUTTON_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(RADIUS))
                    .cursor_pointer()
                    .when(!action.enabled, |el| el.opacity(0.45))
                    .hover(move |style| style.bg(chrome.wash))
                    .with_hint(Hint::new(action.label.clone()), hint::Side::Below, cx)
                    .on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.run(command.clone(), window, cx)
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            let items = vec![(
                                native::MenuEntry::item("Manage Extensions…"),
                                Some(Command::Settings(Section::Extensions)),
                            )];
                            this.context_menu(event.position, items, window, cx);
                        }),
                    )
                    .child(
                        canvas(
                            move |b, _, _| {
                                bounds.borrow_mut().insert(recorded.clone(), b);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(match image {
                        Some(image) => img(image).size(px(16.0)).into_any_element(),
                        None => icon(Icon::Puzzle, 16.0, palette.text_secondary).into_any_element(),
                    })
                    .when(!action.badge.is_empty(), |el| {
                        el.child(
                            div()
                                .absolute()
                                .right(px(1.0))
                                .bottom(px(1.0))
                                .px(px(3.0))
                                .rounded(px(4.0))
                                .bg(palette.accent)
                                .text_size(px(8.5))
                                .text_color(chrome.on_accent)
                                .child(action.badge.clone()),
                        )
                    }),
            );
        }
        row.into_any_element()
    }

    fn omnibox(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chrome = Chrome::new(palette);
        let caret = self.caret_color(palette.accent);
        self.address.update(cx, |input, _| {
            input.restyle(palette);
            input.style.cursor_color = caret;
        });
        let focused = self.address_focused(window, cx);
        let tab = self.current();
        let web = tab.page == Page::Web;
        let private = tab.private;
        let bookmarked = self.bookmarked();
        let webview = tab.view.clone();
        let url = tab.url.clone();
        let zoom = tab.zoom;
        let default_zoom = self.settings.page_zoom;
        let click_input = self.address.clone();
        let (pinged, generation) = self.ping;
        let ping_omnibox = pinged.is_some_and(PingTarget::omnibox);
        let leading: AnyElement = if !web {
            div()
                .size(px(20.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(icon(Icon::Search, 14.0, palette.text_secondary))
                .into_any_element()
        } else {
            site_icon(self.shown_favicon(&url), &url, 20.0, true, palette)
        };
        div()
            .id("omnibox")
            .occlude()
            .flex_1()
            .min_w(px(220.0))
            .h(px(OMNIBOX_HEIGHT))
            .flex()
            .items_center()
            .gap(px(10.0))
            .pl(px(10.0))
            .pr(px(7.0))
            .rounded(px(10.0))
            .bg(chrome.raised)
            .border_1()
            .border_color(if focused {
                palette.accent
            } else {
                color::with_alpha(palette.field_border_strong, 0.45)
            })
            .when(focused, |el| {
                el.shadow(vec![lighting::glow(palette.accent, 0.35, 8.0)])
            })
            .when(!focused, |el| el.shadow(lighting::raised(palette.is_dark)))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if focused {
                        if let Some(view) = &webview {
                            let _ = view.focus_parent();
                        }
                        click_input.update(cx, |input, cx| {
                            input.handle_chrome_click(event.position, window, cx)
                        });
                    } else {
                        this.focus_address(window, cx);
                        window.prevent_default();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let clipboard = cx.read_from_clipboard().and_then(|c| c.text()).is_some();
                    let items = this.omnibox_menu(clipboard);
                    this.context_menu(event.position, items, window, cx);
                }),
            )
            // How far the page has loaded, along the field's bottom edge,
            // as Safari shows it.
            .relative()
            .when(ping_omnibox, |el| {
                let accent = palette.accent;
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .rounded(px(10.0))
                        .with_animation(
                            ("omnibox-ping", generation),
                            Animation::new(slowed(Duration::from_millis(520)))
                                .with_easing(|t: f32| 1.0 - (1.0 - t).powi(3)),
                            move |ring, t| {
                                let grow = px(5.0 * t);
                                ring.top(-grow)
                                    .left(-grow)
                                    .right(-grow)
                                    .bottom(-grow)
                                    .rounded(px(10.0) + grow)
                                    .border_2()
                                    .border_color(color::with_alpha(accent, 0.7 * (1.0 - t)))
                                    .bg(color::with_alpha(accent, 0.3 * (1.0 - t)))
                            },
                        ),
                )
            })
            .when_some(self.progress, |el, (fraction, opacity)| {
                el.child(
                    div()
                        .absolute()
                        .bottom(px(-1.0))
                        .left(px(6.0))
                        .right(px(6.0))
                        .h(px(2.0))
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(fraction))
                                .rounded(px(1.0))
                                .bg(palette.accent)
                                .opacity(opacity),
                        ),
                )
            })
            .child({
                let record = self.omnibox_bounds.clone();
                canvas(move |bounds, _, _| record.set(Some(bounds)), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
            })
            .child(leading)
            .child(
                div()
                    .id("omnibox-text")
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    // The first click selects the whole address, so typing
                    // replaces it. Taken before the field sees the press,
                    // or it would place a caret and a drag would undo it.
                    .when(!focused, |el| {
                        el.capture_any_mouse_down(cx.listener(
                            |this, event: &MouseDownEvent, window, cx| {
                                if event.button == MouseButton::Left {
                                    this.focus_address(window, cx);
                                    window.prevent_default();
                                    cx.stop_propagation();
                                }
                            },
                        ))
                    })
                    .relative()
                    // The input stays rendered, so focus and select-all
                    // always have somewhere to land; while the field is idle
                    // the styled address sits over it.
                    .child(self.address.clone())
                    .when(!focused && self.current().page != Page::Start, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .items_center()
                                .bg(chrome.raised)
                                .child(address_label(&self.address_text(self.selected), palette)),
                        )
                    }),
            )
            .when((zoom - default_zoom).abs() > 0.001 && web, |el| {
                el.child(
                    div()
                        .id("zoom-level")
                        .occlude()
                        .px(px(7.0))
                        .h(px(26.0))
                        .flex()
                        .items_center()
                        .rounded(px(7.0))
                        .bg(palette.soft_fill)
                        .text_size(px(11.5))
                        .text_color(palette.soft_label)
                        .cursor_pointer()
                        .with_hint(Hint::new("Reset zoom").shortcut("⌘0"), hint::Side::Below, cx)
                        .on_click(cx.listener(|this, _, _, cx| this.zoom_by(0.0, cx)))
                        .child(format!("{:.0}%", zoom * 100.0)),
                )
            })
            .when(private, |el| {
                el.child(
                    div()
                        .px(px(8.0))
                        .h(px(26.0))
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .rounded(px(7.0))
                        .bg(palette.control_fill)
                        .text_size(px(11.5))
                        .text_color(palette.control_label)
                        .child(icon(Icon::Private, 12.0, palette.control_label))
                        .child("Private"),
                )
            })
            .when(web, |el| {
                let button = tool_button_inked(
                    "bookmark-page",
                    if bookmarked {
                        Icon::StarFilled
                    } else {
                        Icon::Star
                    },
                    if bookmarked {
                        palette.accent
                    } else {
                        palette.text_secondary
                    },
                    Hint::new(if bookmarked {
                        "Remove bookmark"
                    } else {
                        "Bookmark this page"
                    })
                    .shortcut("⌘D"),
                    28.0,
                    false,
                    true,
                    palette,
                    cx,
                    |this, window, cx| this.run(Command::BookmarkPage, window, cx),
                );
                let mut star = div().relative().size(px(28.0)).flex_none();
                if pinged == Some(PingTarget::BookmarkPage) {
                    let accent = palette.accent;
                    star = star.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .rounded(px(7.0))
                            .with_animation(
                                ("bookmark-ping", generation),
                                Animation::new(slowed(Duration::from_millis(520)))
                                    .with_easing(|t: f32| 1.0 - (1.0 - t).powi(3)),
                                move |ring, t| {
                                    let grow = px(5.0 * t);
                                    ring.top(-grow)
                                        .left(-grow)
                                        .size(px(28.0) + grow * 2.0)
                                        .rounded(px(7.0) + grow)
                                        .border_2()
                                        .border_color(color::with_alpha(accent, 0.7 * (1.0 - t)))
                                        .bg(color::with_alpha(accent, 0.3 * (1.0 - t)))
                                },
                            ),
                    );
                }
                el.child(star.child(button))
            })
    }

    /// A site's icon, if icons are shown at all.
    fn shown_favicon(&self, url: &str) -> Option<Favicon> {
        if !self.settings.show_favicons {
            return None;
        }
        let icon = self.favicons().get(url).cloned();
        // Only sites have icons to fetch; blank and extension pages don't.
        if icon.is_none() && (url.starts_with("https://") || url.starts_with("http://")) {
            self.favicon_wants.borrow_mut().insert(url.to_owned());
        }
        icon
    }

    /// A tab's icon: its site's, or the internal page's own, with a private
    /// tab's mask over the corner.
    /// The icon of whatever is at `url`: an internal page's own, or the
    /// site's.
    fn page_icon(&self, page: Page, url: &str, size: f32, active: bool, palette: Palette) -> AnyElement {
        let glyph = match page {
            Page::Web => None,
            Page::Start => Some(Icon::Home),
            Page::Settings => Some(Icon::Gear),
            Page::Downloads => Some(Icon::Download),
            Page::Bookmarks => Some(Icon::Bookmark),
        };
        match glyph {
            Some(glyph) => div()
                .size(px(size))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(icon(glyph, size * 0.8, palette.text_secondary))
                .into_any_element(),
            None => site_icon(self.shown_favicon(url), url, size, active, palette),
        }
    }

    /// A closed tab's icon as it leaves: the same as it had.
    fn ghost_icon(&self, url: &str, size: f32, palette: Palette) -> AnyElement {
        let page = Page::from_internal_url(url).unwrap_or(Page::Web);
        self.page_icon(page, url, size, false, palette)
    }

    fn tab_icon(&self, tab: &BrowserTab, size: f32, active: bool, palette: Palette) -> AnyElement {
        let base = self.page_icon(tab.page, &tab.url, size, active, palette);
        if !tab.private {
            return base;
        }
        div()
            .relative()
            .size(px(size))
            .flex_none()
            .child(base)
            .child(
                div()
                    .absolute()
                    .right(px(-4.0))
                    .bottom(px(-3.0))
                    .size(px(size * 0.62))
                    .rounded_full()
                    .bg(palette.control_fill)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(Icon::Private, size * 0.5, palette.control_label)),
            )
            .into_any_element()
    }

    /// The width every horizontal tab shares this frame: frozen after a
    /// close until the pointer leaves the strip, so the tab that slides
    /// into place brings its × to where the last one was; otherwise as wide
    /// as fits, within limits. It glides between the two.
    /// The room the horizontal tabs have, the + after them taken out:
    /// the same whether the + is in the strip, after the last tab, or
    /// pinned beside it once they overflow.
    fn horizontal_tab_room(&self) -> f32 {
        let view = f32::from(self.tab_scroll.bounds().size.width);
        let pinned = if self.new_tab_pinned { NEW_TAB_PINNED_ROOM } else { 0.0 };
        // A little to spare, so rounding never leaves the + a pixel over.
        view + pinned - NEW_TAB_IN_STRIP_ROOM - 2.0
    }

    /// Whether the horizontal tabs, at their narrowest, don't fit: the +
    /// then stays in view at the strip's end instead of scrolling away.
    fn horizontal_tabs_overflow(&self) -> bool {
        let count = self.tabs.len().max(1) as f32;
        f32::from(self.tab_scroll.bounds().size.width) > 0.0
            && count * TAB_MIN_WIDTH + TAB_GAP * (count - 1.0) > self.horizontal_tab_room()
    }

    fn horizontal_tab_width(&self) -> f32 {
        let natural = {
            let room = self.horizontal_tab_room();
            let count = self.tabs.len().max(1) as f32;
            if f32::from(self.tab_scroll.bounds().size.width) <= 0.0 {
                TAB_MAX_WIDTH
            } else {
                ((room - TAB_GAP * (count - 1.0)) / count).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH)
            }
        };
        let target = self.tab_freeze.unwrap_or(natural);
        self.controls.tween("tab-width", target, MOVE)
    }

    /// How far a tab has grown in (0 to 1), for tabs opened this session.
    fn tab_arrival(&mut self, id: u64) -> f32 {
        if !self.arriving.contains(&id) {
            return 1.0;
        }
        let t = self
            .controls
            .tween_from(("tab-arrival", id), 0.0, 1.0, MOVE);
        if t >= 0.999 {
            self.arriving.remove(&id);
        }
        t
    }

    /// The pointer on a tab, however tabs are listed: right-click for its
    /// menu, middle-click to close it, press and drag to move it.
    fn tab_row_events(
        &self,
        row: Stateful<Div>,
        index: usize,
        id: u64,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let active = index == self.selected;
        let palette = self.controls.palette();
        let accent = palette.accent;
        let wash = color::with_alpha(palette.accent, 0.18);
        row.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                let items = this.tab_menu(id);
                this.context_menu(event.position, items, window, cx);
            }),
        )
        .on_mouse_up(
            MouseButton::Middle,
            cx.listener(move |this, _, window, cx| this.close_tab_by_pointer(id, window, cx)),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, _| {
                this.begin_tab_drag(id, event.position)
            }),
        )
        .child(tabdrag::record_bounds(self.tab_bounds.clone(), id))
        // A bookmark dropped on the tab in front opens there; on another
        // tab, in a new one beside it.
        .drag_over::<bookmarks_view::DraggedBookmark>(move |style, _, _, _| {
            if active {
                style.border_2().border_color(accent)
            } else {
                style.bg(wash)
            }
        })
        .on_drop(cx.listener(move |this, dragged: &bookmarks_view::DraggedBookmark, window, cx| {
            {
                for (index, &bookmark) in dragged.ids().iter().enumerate() {
                    // Onto the tab, the first; the rest in tabs of their own.
                    let on = (index == 0).then_some(id);
                    this.drop_bookmark_on_tabs(bookmark, on, window, cx);
                }
            }
        }))
    }

    /// What a tab list lays out: departing tabs still shrinking away, the
    /// order of tabs and those ghosts, and how far each tab has grown in.
    fn tab_slots(&mut self) -> (Vec<(ClosingTab, f32)>, Vec<Slot>, Vec<f32>) {
        let ghosts = self.departures();
        let slots = self.tab_sequence(&ghosts);
        let ids: Vec<u64> = self.tabs.iter().map(|tab| tab.id).collect();
        let arrivals = ids.into_iter().map(|id| self.tab_arrival(id)).collect();
        (ghosts, slots, arrivals)
    }

    /// Closed tabs still shrinking away, with how far along each is (1 down
    /// to 0). Those that have finished are dropped.
    fn departures(&mut self) -> Vec<(ClosingTab, f32)> {
        let controls = &self.controls;
        let mut out = Vec::new();
        self.closing.retain(|ghost| {
            let t = controls.tween_from(("tab-departure", ghost.id), 1.0, 0.0, MOVE);
            if t <= 0.001 {
                return false;
            }
            out.push((ghost.clone(), t));
            true
        });
        out
    }

    /// Tabs and departing ghosts in on-screen order: each ghost follows the
    /// tab (or ghost) that was to its left when it closed.
    fn tab_sequence(&self, ghosts: &[(ClosingTab, f32)]) -> Vec<Slot> {
        let mut slots = Vec::new();
        let known = |id: u64| {
            self.tabs.iter().any(|tab| tab.id == id) || ghosts.iter().any(|(g, _)| g.id == id)
        };
        fn after(
            id: Option<u64>,
            ghosts: &[(ClosingTab, f32)],
            slots: &mut Vec<Slot>,
            known: &dyn Fn(u64) -> bool,
        ) {
            for (index, (ghost, _)) in ghosts.iter().enumerate() {
                let anchored = match ghost.prev {
                    Some(prev) if known(prev) => Some(prev),
                    _ => None,
                };
                if anchored == id {
                    slots.push(Slot::Ghost(index));
                    after(Some(ghost.id), ghosts, slots, known);
                }
            }
        }
        after(None, ghosts, &mut slots, &known);
        for (index, tab) in self.tabs.iter().enumerate() {
            slots.push(Slot::Tab(index));
            after(Some(tab.id), ghosts, &mut slots, &known);
        }
        slots
    }

    fn horizontal_tabs(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chrome = Chrome::new(palette);
        let focus = self.controls.focus("horizontal-tabs", cx);
        let focused = focus.is_focused(window);
        let count = self.tabs.len();
        self.new_tab_pinned = self.horizontal_tabs_overflow();
        let pinned = self.new_tab_pinned;
        let width = self.horizontal_tab_width();
        let (ghosts, slots, arrivals) = self.tab_slots();
        let new_tab = |cx: &mut Context<Self>| {
            tool_button(
                "new-tab-strip",
                Icon::Plus,
                Hint::new("New tab").shortcut("⌘T"),
                NEW_TAB_BUTTON,
                false,
                palette,
                cx,
                |this, window, cx| this.new_tab(false, window, cx),
            )
        };
        let mut strip = div()
            .id("horizontal-tab-scroll")
            // A bookmark dropped among the tabs opens in a new one.
            .drag_over::<bookmarks_view::DraggedBookmark>(move |style, _, _, _| {
                style.bg(color::with_alpha(palette.accent, 0.1))
            })
            .on_drop(cx.listener(|this, dragged: &bookmarks_view::DraggedBookmark, window, cx| {
                {
                    for &bookmark in dragged.ids() {
                        this.drop_bookmark_on_tabs(bookmark, None, window, cx);
                    }
                }
            }))
            .size_full()
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll)
            .flex()
            .items_center()
            .gap(px(TAB_GAP));
        let mut children = Vec::new();
        for slot in slots {
            let (id, element) = match slot {
                Slot::Ghost(index) => {
                    let (ghost, t) = &ghosts[index];
                    let row = div()
                        .h(px(28.0))
                        .w(px(ghost.width * t))
                        .flex_none()
                        // Take the gap with it, so nothing jumps at the end.
                        .mr(px(-TAB_GAP * (1.0 - t)))
                        .overflow_hidden()
                        .opacity(*t)
                        .rounded(px(7.0))
                        .bg(chrome.rest)
                        .child(
                            div()
                                .w(px(ghost.width))
                                .h_full()
                                .flex()
                                .items_center()
                                .gap(px(7.0))
                                .pl(px(7.0))
                                .text_size(px(12.5))
                                .text_color(palette.text_secondary)
                                .child(self.ghost_icon(&ghost.url, 16.0, palette))
                                .child(
                                    div()
                                        .min_w(px(0.0))
                                        .flex_1()
                                        .truncate()
                                        .child(ghost.title.clone()),
                                ),
                        );
                    (ghost.id, row.into_any_element())
                }
                Slot::Tab(index) => {
                    let tab = &self.tabs[index];
                    let active = index == self.selected;
                    let arrival = arrivals[index];
                    let click_focus = focus.clone();
                    let id = tab.id;
                    let row = div()
                        .id(("horizontal-tab", tab.id))
                        .group("tab")
                        .w(px(width * arrival))
                        .flex_none()
                        .mr(px(-TAB_GAP * (1.0 - arrival)))
                        .opacity(arrival)
                        .overflow_hidden()
                        .h(px(28.0))
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .pl(px(7.0))
                        .pr(px(5.0))
                        .rounded(px(7.0))
                        .text_size(px(12.5))
                        .border_1()
                        .border_color(if active && focused {
                            palette.accent
                        } else {
                            Palette::transparent()
                        })
                        .text_color(if active {
                            palette.text_primary
                        } else {
                            palette.text_secondary
                        })
                        .cursor_pointer()
                        .when(active, |el| {
                            el.bg(chrome.raised)
                                .shadow(lighting::raised(palette.is_dark))
                                .track_focus(&focus)
                                .on_key_down(cx.listener(
                                    move |this, event: &KeyDownEvent, _, cx| {
                                        if let Some(key) = vampir::key(event)
                                            && let Some(next) = vampir::keyboard::step(
                                                key,
                                                vampir::Orientation::Horizontal,
                                                index,
                                                count,
                                            )
                                        {
                                            cx.stop_propagation();
                                            this.select(next, cx);
                                        }
                                    },
                                ))
                        })
                        .when(!active, |el| {
                            el.bg(chrome.rest).hover(move |style| style.bg(chrome.wash))
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(view) = &this.current().view {
                                let _ = view.focus_parent();
                            }
                            window.focus(&click_focus, cx);
                            this.select_id(id, cx);
                        }))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            this.hover_label(id, *hovered, cx)
                        }))
                        .child(self.tab_icon(tab, 16.0, active, palette))
                        .child(marquee::label(
                            tab.title.clone(),
                            id,
                            self.hovered_label == Some(id),
                            &self.label_widths,
                        ))
                        .when(tab.playing_audio || tab.muted, |row| {
                            row.child(tab_sound_button(tab, palette, hint::Side::Below, cx))
                        })
                        .child(close_button(
                            ("horizontal-close", tab.id),
                            id,
                            active,
                            palette,
                            cx,
                        ));
                    let row = self.tab_row_events(row, index, id, cx);
                    let element = row.into_any_element();
                    (tab.id, element)
                }
            };
            children.push(id);
            strip = strip.child(element);
        }
        self.strip_children = children;
        // Right after the last tab, as long as they all fit.
        if !pinned {
            strip = strip.child(new_tab(cx));
        }
        div()
            .id("tab-strip")
            .h(px(TAB_STRIP_HEIGHT))
            .w_full()
            .flex_none()
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            // Leaving the strip lets tabs take the room a close left.
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && this.tab_freeze.take().is_some() {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    let items = this.tab_strip_menu();
                    this.context_menu(event.position, items, window, cx);
                }),
            )
            .child(
                // All in view, the + with them: nothing to fade.
                self.faded_if(
                    pinned,
                    "horizontal-tabs",
                    strip,
                    &self.tab_scroll,
                    ScrollAxis::Horizontal,
                    chrome.ground,
                )
                .flex_1()
                .min_w(px(0.0))
                .h_full(),
            )
            .when(pinned, |el| el.child(new_tab(cx)))
    }

    fn vertical_tabs(&mut self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        if self.compact_vertical_tabs {
            return self.compact_vertical_rail(palette, cx).into_any_element();
        }
        let chrome = Chrome::new(palette);
        let new_tab = div()
            .id("sidebar-new-tab")
            .h(px(30.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(9.0))
            .px(px(8.0))
            .rounded(px(7.0))
            .text_size(px(12.5))
            .text_color(palette.text_secondary)
            .cursor_pointer()
            .hover(move |style| style.bg(chrome.wash).text_color(palette.text_primary))
            .on_click(cx.listener(|this, _, window, cx| this.new_tab(false, window, cx)))
            .child(
                div()
                    .size(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(Icon::Plus, 14.0, palette.text_secondary)),
            )
            .child("New tab");
        let scroll = self.controls.scroll("vertical-tab-list");
        let (ghosts, slots, arrivals) = self.tab_slots();
        let mut list = div()
            .id("vertical-tab-list")
            // A bookmark dropped among the tabs opens in a new one.
            .drag_over::<bookmarks_view::DraggedBookmark>(move |style, _, _, _| {
                style.bg(color::with_alpha(palette.accent, 0.1))
            })
            .on_drop(cx.listener(|this, dragged: &bookmarks_view::DraggedBookmark, window, cx| {
                {
                    for &bookmark in dragged.ids() {
                        this.drop_bookmark_on_tabs(bookmark, None, window, cx);
                    }
                }
            }))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(px(ROW_GAP))
            .px(px(10.0))
            .pb(px(10.0));
        for slot in slots {
            let element = match slot {
                Slot::Ghost(index) => {
                    let (ghost, t) = &ghosts[index];
                    revealed(
                            div()
                                .h(px(30.0))
                                .w_full()
                                .rounded(px(7.0))
                                .bg(chrome.rest)
                                .flex()
                                .items_center()
                                .gap(px(9.0))
                                .pl(px(8.0))
                                .text_size(px(12.5))
                                .text_color(palette.text_secondary)
                                .child(self.ghost_icon(&ghost.url, 18.0, palette))
                                .child(
                                    div()
                                        .min_w(px(0.0))
                                        .flex_1()
                                        .truncate()
                                        .child(ghost.title.clone()),
                                ),
                        30.0,
                        ROW_GAP,
                        *t,
                    )
                }
                Slot::Tab(index) => {
                    let tab = &self.tabs[index];
                    let active = index == self.selected;
                    let arrival = arrivals[index];
                    let id = tab.id;
                    let row = div()
                        .id(("tab", tab.id))
                        .group("tab")
                        .h(px(30.0))
                        .w_full()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .pl(px(8.0))
                        .pr(px(5.0))
                        .rounded(px(7.0))
                        .text_size(px(12.5))
                        .text_color(if active {
                            palette.text_primary
                        } else {
                            palette.text_secondary
                        })
                        .cursor_pointer()
                        .when(active, |el| {
                            el.bg(chrome.raised)
                                .shadow(lighting::raised(palette.is_dark))
                        })
                        .when(!active, |el| {
                            el.bg(chrome.rest).hover(move |style| style.bg(chrome.wash))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.select_id(id, cx)))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            this.hover_label(id, *hovered, cx)
                        }))
                        .child(self.tab_icon(tab, 18.0, active, palette))
                        .child(marquee::label(
                            tab.title.clone(),
                            id,
                            self.hovered_label == Some(id),
                            &self.label_widths,
                        ))
                        .when(tab.playing_audio || tab.muted, |row| {
                            row.child(tab_sound_button(tab, palette, hint::Side::Right, cx))
                        })
                        .child(close_button(("close", tab.id), id, active, palette, cx));
                    let row = self.tab_row_events(row, index, id, cx);
                    revealed(row, 30.0, ROW_GAP, arrival)
                }
            };
            list = list.child(element);
        }
        // Right below the last tab.
        list = list.child(new_tab);
        let footer = div()
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .px(px(10.0))
            .child(tool_button(
                "collapse-vertical-tabs",
                Icon::ChevronLeft,
                "Collapse sidebar",
                30.0,
                false,
                palette,
                cx,
                |this, _, cx| this.toggle_compact_vertical_tabs(cx),
            ));
        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                self.faded(
                    "vertical-tab-list",
                    list,
                    &scroll,
                    ScrollAxis::Vertical,
                    chrome.ground,
                )
                .flex_1()
                .min_h(px(0.0))
                .w_full(),
            )
            .child(footer)
            .into_any_element()
    }

    fn compact_vertical_rail(
        &mut self,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chrome = Chrome::new(palette);
        let scroll = self.controls.scroll("compact-tab-list");
        let (ghosts, slots, arrivals) = self.tab_slots();
        let mut list = div()
            .id("compact-tab-list")
            // A bookmark dropped among the tabs opens in a new one.
            .drag_over::<bookmarks_view::DraggedBookmark>(move |style, _, _, _| {
                style.bg(color::with_alpha(palette.accent, 0.1))
            })
            .on_drop(cx.listener(|this, dragged: &bookmarks_view::DraggedBookmark, window, cx| {
                {
                    for &bookmark in dragged.ids() {
                        this.drop_bookmark_on_tabs(bookmark, None, window, cx);
                    }
                }
            }))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .items_center()
            .gap(px(RAIL_GAP))
            .pb(px(10.0));
        for slot in slots {
            let element = match slot {
                Slot::Ghost(index) => {
                    let (ghost, t) = &ghosts[index];
                    revealed(
                        div()
                            .w(px(34.0))
                            .h(px(34.0))
                            .rounded(px(RADIUS))
                            .bg(chrome.rest)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(self.ghost_icon(&ghost.url, 20.0, palette)),
                        34.0,
                        RAIL_GAP,
                        *t,
                    )
                }
                Slot::Tab(index) => {
                    let tab = &self.tabs[index];
                    let active = index == self.selected;
                    let arrival = arrivals[index];
                    let id = tab.id;
                    let row = div()
                        .id(("compact-tab", tab.id))
                        .relative()
                        .w(px(34.0))
                        .h(px(34.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(RADIUS))
                        .cursor_pointer()
                        .when(active, |el| {
                            el.bg(chrome.raised)
                                .shadow(lighting::raised(palette.is_dark))
                        })
                        .when(!active, |el| {
                            el.bg(chrome.rest).hover(move |style| style.bg(chrome.wash))
                        })
                        .with_hint(Hint::new(tab.title.clone()), hint::Side::Right, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.select_id(id, cx)))
                        .child(self.tab_icon(tab, 20.0, active, palette))
                        .when(tab.playing_audio || tab.muted, |row| {
                            row.child(
                                tab_sound_button(tab, palette, hint::Side::Right, cx)
                                    .absolute()
                                    .right(px(-2.0))
                                    .bottom(px(-2.0))
                                    .bg(chrome.raised),
                            )
                        });
                    let row = self.tab_row_events(row, index, id, cx);
                    revealed(row, 34.0, RAIL_GAP, arrival)
                }
            };
            list = list.child(element);
        }
        // Right below the last tab.
        list = list.child(tool_button(
            "compact-new-tab",
            Icon::Plus,
            Hint::new("New tab").shortcut("⌘T"),
            34.0,
            false,
            palette,
            cx,
            |this, window, cx| this.new_tab(false, window, cx),
        ));
        div()
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .overflow_hidden()
            .child(
                self.faded(
                    "compact-tab-list",
                    list,
                    &scroll,
                    ScrollAxis::Vertical,
                    chrome.ground,
                )
                .flex_1()
                .min_h(px(0.0))
                .w_full(),
            )
            .child(
                div()
                    .h(px(48.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(tool_button(
                        "expand-vertical-tabs",
                        Icon::ChevronRight,
                        "Expand sidebar",
                        30.0,
                        false,
                        palette,
                        cx,
                        |this, _, cx| this.toggle_compact_vertical_tabs(cx),
                    )),
            )
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.close_when_drawn || self.tabs.is_empty() {
            window.remove_window();
            return div().into_any_element();
        }
        self.wake_current(window);
        if self.focused_selection_generation != self.selection_generation && self.palette.is_none() {
            self.focused_selection_generation = self.selection_generation;
            // Restored pages are created by wake_current above. Give the
            // newly selected page the keyboard once it exists, and clear
            // GPUI's tab focus so its arrow handler cannot steal page keys.
            // An explicitly opened browser field keeps its own focus.
            if !self.text_field_focused(window, cx)
                && let Some(view) = self.current().view.as_ref()
                && self.current().page == Page::Web
            {
                window.blur();
                let _ = view.focus();
            }
        }
        self.sync_key_flags(window, cx);
        self.show_notice_away_from_settings(cx);
        // Moved or resized: the session keeps where it is.
        let bounds = window.bounds();
        let bounds = Some([
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
        ]);
        if self.window_bounds != bounds {
            let first = self.window_bounds.is_none();
            self.window_bounds = bounds;
            if !first && !self.private {
                self.persist_soon(cx);
            }
        }
        self.progress = self.load_progress();
        let palette = self.controls.palette();
        let chrome = Chrome::new(palette);
        // Switching layouts is one motion: the sidebar's width and the tab
        // strip's height glide together, each clipping content laid out at
        // its full size, so nothing reflows on the way.
        let sidebar_target = match (self.vertical_tabs, self.compact_vertical_tabs) {
            (false, _) => 0.0,
            (true, true) => RAIL_WIDTH,
            (true, false) => SIDEBAR_WIDTH,
        };
        let reveal = self.chrome_reveal();
        self.fade_traffic_lights(reveal);
        let sidebar_width = self
            .controls
            .tween("sidebar-width", sidebar_target, LAYOUT_MOVE)
            * reveal;
        let strip_height = self.tab_strip_height() * reveal;
        // The strip's tabs are where they were when last drawn, so it can
        // only scroll to the selected one once it has been drawn again.
        self.strip_frames = if strip_height > 0.5 { self.strip_frames.saturating_add(1) } else { 0 };
        if self.reveal_selected && !self.vertical_tabs && self.strip_frames > 1 && self.reveal_selected_tab() {
            self.reveal_selected = false;
        }
        let bookmarks_target = if self.bookmarks_bar {
            BOOKMARKS_HEIGHT
        } else {
            0.0
        };
        let bookmarks_height = self
            .controls
            .tween("bookmarks-height", bookmarks_target, LAYOUT_MOVE)
            * reveal;
        let shelf_target = if self.downloads().shelf().next().is_some() {
            SHELF_HEIGHT
        } else {
            0.0
        };
        let shelf_height = self
            .controls
            .tween("download-shelf-height", shelf_target, LAYOUT_MOVE);
        let web_page = self.current().page == Page::Web && self.palette.is_none();
        let find_target = if self.find.open && web_page { find::FIND_HEIGHT } else { 0.0 };
        let find_height = self.controls.tween("find-height", find_target, find::FIND_MOVE);
        // While the browser's parts slide, the page keeps one size and only
        // moves: the larger of its size when the slide began and where it
        // will end up, clipped by the window. Resizing it every frame would
        // make it reflow every frame. How much bigger it ends up, per axis:
        let strip_target = if self.vertical_tabs { 0.0 } else { TAB_STRIP_HEIGHT };
        let grow = (
            sidebar_width - sidebar_target,
            (strip_height - strip_target)
                + (bookmarks_height - bookmarks_target)
                + (shelf_height - shelf_target)
                + (find_height - find_target),
        );
        let sliding = !self.minimal && (grow.0.abs() > 0.5 || grow.1.abs() > 0.5);
        if !sliding {
            self.page_hold.set(None);
        }
        let page_hold = self.page_hold.clone();
        let content: AnyElement = if self.palette.is_some() {
            self.palette_overlay(palette, window, cx)
        } else {
            match self.current().page {
                Page::Web => {
                    let view = self.current().view.clone();
                    let minimal = self.minimal;
                    canvas(
                        move |bounds, window, _| {
                            // In minimal mode the page keeps the size it has
                            // with the browser folded away; the browser
                            // slides it down rather than squeezing it, so it
                            // doesn't reflow each time.
                            let mut size = bounds.size;
                            if sliding {
                                let held = page_hold.get().unwrap_or((
                                    f32::from(size.width),
                                    f32::from(size.height),
                                ));
                                page_hold.set(Some(held));
                                let end = (f32::from(size.width) + grow.0, f32::from(size.height) + grow.1);
                                size.width = px(held.0.max(end.0));
                                size.height = px(held.1.max(end.1));
                            }
                            if minimal {
                                let viewport = window.viewport_size();
                                size.width = size.width.max(viewport.width);
                                let resting = viewport.height
                                    - px(minimal::MINIMAL_BAR + 1.0 + shelf_height + find_height);
                                size.height = size.height.max(resting);
                            }
                            if let Some(view) = &view {
                                // WebKit moves the view into its own window for element
                                // fullscreen. Resizing it to the browser's page bounds
                                // there would shrink the fullscreen content back down.
                                if unsafe { view.webview().fullscreenState() }
                                    == WKFullscreenState::NotInFullscreen
                                {
                                    let _ = view.set_bounds(Rect {
                                        position: dpi::Position::Logical(dpi::LogicalPosition::new(
                                            f64::from(bounds.origin.x),
                                            f64::from(bounds.origin.y),
                                        )),
                                        size: dpi::Size::Logical(dpi::LogicalSize::new(
                                            f64::from(size.width),
                                            f64::from(size.height),
                                        )),
                                    });
                                }
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full()
                    .into_any_element()
                }
                Page::Start => self.start_page(palette, window, cx),
                Page::Settings => self.settings_page(palette, window, cx),
                Page::Downloads => self.downloads_page(palette, cx),
                Page::Bookmarks => self.bookmarks_page(palette, cx),
            }
        };
        let progress = self.progress;
        // The find bar above the page, which gives it the room.
        let find_bar = (find_height > 0.5).then(|| self.find_bar(find_height, palette, window, cx));
        let web_area = div()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .border_t_1()
            .when(sidebar_width > 0.5, |el| el.border_l_1())
            .border_color(chrome.line)
            .children(find_bar)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .relative()
                    .child(content)
                    .children(self.suggestion_backdrop()),
            );
        let mut body = div().flex_1().min_h(px(0.0)).flex();
        if sidebar_width > 0.5 {
            body = body.child(
                div()
                    .w(px(sidebar_width))
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    // Fades only on the way to and from nothing.
                    .opacity((sidebar_width / RAIL_WIDTH).min(1.0))
                    .child(self.vertical_tabs(palette, cx)),
            );
        }
        body = body.child(web_area);
        let mut root = vampir::root(div().id("root"), self, cx)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family(ui_font())
            .text_size(px(13.0))
            .bg(chrome.ground)
            .text_color(palette.text_primary)
            .children(self.minimal_bar(palette));
        root = if self.minimal {
            // Slides down from above rather than unrolling.
            root.child(
                div()
                    .h(px(TOOLBAR_HEIGHT * reveal))
                    .w_full()
                    .flex_none()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .child(self.toolbar(palette, window, cx)),
            )
        } else {
            root.child(self.toolbar(palette, window, cx))
        };
        if bookmarks_height > 0.5 {
            root = root.child(
                // It slides up under the toolbar as it goes, rather than
                // being cut off where it stands.
                div()
                    .h(px(bookmarks_height))
                    .w_full()
                    .flex_none()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .opacity(bookmarks_height / BOOKMARKS_HEIGHT)
                    .child(self.bookmarks_bar(palette, cx)),
            );
        }
        if strip_height > 0.5 {
            root = root.child(
                div()
                    .h(px(strip_height))
                    .w_full()
                    .flex_none()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .opacity(strip_height / TAB_STRIP_HEIGHT)
                    .child(
                        div()
                            .h(px(TAB_STRIP_HEIGHT))
                            .w_full()
                            .flex_none()
                            .child(self.horizontal_tabs(palette, window, cx)),
                    ),
            );
        } else {
            // Out of sight, it forgets where it was scrolled, so it comes
            // back at its start instead of jumping there.
            self.tab_scroll.set_offset(point(px(0.0), px(0.0)));
        }
        root = root.child(body);
        if let Some(anchor) = self.omnibox_bounds.get()
            && let Some(list) = self.suggestion_list(anchor, palette, cx)
        {
            root = root.child(list);
        }
        if let Some(menu) = self.bookmark_menu_overlay(palette, window, cx) {
            root = root.child(menu);
        }
        if let Some(tracker) = self.drag_tracker(cx) {
            root = root.child(tracker);
        }
        if let Some(highlight) = self.drop_highlight(palette) {
            root = root.child(highlight);
        }
        if shelf_height > 0.5 {
            root = root.child(
                div()
                    .h(px(shelf_height))
                    .w_full()
                    .flex_none()
                    .overflow_hidden()
                    .child(self.download_shelf(palette, cx)),
            );
        }
        // The hue gliding to a new tab's colour, the scheme following the
        // desktop, the scroll fades and the tab animations all need frames
        // until they settle. Asked last, so anything that started animating
        // in this frame counts.
        let downloading = self
            .downloads()
            .shelf()
            .any(|d| d.state == downloads::DownloadState::InProgress);
        // Private tabs' sites stay off the network and out of the cache.
        let wants: Vec<String> = self.favicon_wants.borrow_mut().drain().collect();
        for url in wants {
            let private = self.tabs.iter().any(|t| t.private && t.url == url);
            if !private {
                self.want_favicon(&url, None);
            }
        }
        // The progress bar's fade needs every frame; a load's progress and
        // downloads' don't. Drawing the whole window every frame while a
        // page loads kept the main thread, which WebKit asks at each step
        // of the load, busy.
        let fading = progress.is_some_and(|(fraction, opacity)| fraction >= 1.0 || opacity < 1.0);
        if self.controls.animating()
            || (self.reveal_selected && !self.vertical_tabs)
            || fading
            // The switcher's fade in and out; open, it sits still.
            || self.palette_animating()
        {
            window.request_animation_frame();
        } else if progress.is_some() || downloads::sizes_pending() {
            self.redraw_soon(Duration::from_millis(66), cx);
        } else if downloading {
            self.redraw_soon(Duration::from_millis(250), cx);
        }
        root.into_any_element()
    }
}

actions!(
    vamprowser,
    [
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        NewTab,
        NewPrivateTab,
        NewWindow,
        NewPrivateWindow,
        CloseWindow,
        ReopenClosedTab,
        CloseTab,
        OpenLocation,
        Back,
        Forward,
        Reload,
        EraseCacheAndReload,
        GoHome,
        BookmarkPage,
        ToggleBookmarks,
        ToggleVerticalTabs,
        ToggleMinimalMode,
        ToggleReaderMode,
        ShowBookmarks,
        ImportBookmarks,
        NextTab,
        PreviousTab,
        SwitchTabs,
        OpenSettings,
        ShowHistory,
        ShowDownloads,
        ClearHistory,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        Print,
        MakeDefaultBrowser,
        CustomizeToolbar,
        ManageExtensions,
        ReopenClosedWindow,
        Minimize,
        Zoom,
        Stop,
        ShowWebInspector,
        FindInPage,
        FindNext,
        FindPrevious,
        About,
    ]
);

fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Vamprowser").items([
            MenuItem::action("About Vamprowser", About),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Make Vamprowser the Default Browser", MakeDefaultBrowser),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Vamprowser", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Vamprowser", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Window", NewWindow),
            MenuItem::action("New Private Window", NewPrivateWindow),
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("New Private Tab", NewPrivateTab),
            MenuItem::action("Reopen Closed Tab", ReopenClosedTab),
            MenuItem::action("Reopen Closed Window", ReopenClosedWindow),
            MenuItem::separator(),
            MenuItem::action("Open Location…", OpenLocation),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Window", CloseWindow),
            MenuItem::separator(),
            MenuItem::action("Print…", Print),
        ]),
        {
            // Find in Edit, as in every Mac app.
            let mut edit = vampir::edit_menu();
            edit.items.push(MenuItem::separator());
            edit.items.push(MenuItem::submenu(Menu::new("Find").items([
                MenuItem::action("Find in Page…", FindInPage),
                MenuItem::action("Find Next", FindNext),
                MenuItem::action("Find Previous", FindPrevious),
            ])));
            edit
        },
        Menu::new("View").items([
            MenuItem::action("Switch Tabs…", SwitchTabs),
            MenuItem::separator(),
            MenuItem::action("Toggle Bookmarks Bar", ToggleBookmarks),
            MenuItem::action("Toggle Vertical Tabs", ToggleVerticalTabs),
            MenuItem::action("Minimal Mode", ToggleMinimalMode),
            MenuItem::action("Reader Mode", ToggleReaderMode),
            MenuItem::action("Customize Toolbar…", CustomizeToolbar),
            MenuItem::separator(),
            MenuItem::action("Reload Page", Reload),
            MenuItem::action("Erase Cache and Reload", EraseCacheAndReload),
            MenuItem::action("Stop Loading", Stop),
            MenuItem::separator(),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Actual Size", ZoomReset),
            MenuItem::separator(),
            MenuItem::action("Show Web Inspector", ShowWebInspector),
        ]),
        Menu::new("History").items([
            MenuItem::action("Back", Back),
            MenuItem::action("Forward", Forward),
            MenuItem::action("Home", GoHome),
            MenuItem::separator(),
            MenuItem::action("Show All History", ShowHistory),
            MenuItem::action("Clear History…", ClearHistory),
        ]),
        Menu::new("Bookmarks").items([
            MenuItem::action("Show All Bookmarks", ShowBookmarks),
            MenuItem::action("Bookmark This Page", BookmarkPage),
            MenuItem::action("Toggle Bookmarks Bar", ToggleBookmarks),
            MenuItem::separator(),
            MenuItem::action("Import Bookmarks…", ImportBookmarks),
        ]),
        Menu::new("Tools").items([
            MenuItem::action("Downloads", ShowDownloads),
            MenuItem::action("Extensions…", ManageExtensions),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Show Next Tab", NextTab),
            MenuItem::action("Show Previous Tab", PreviousTab),
        ]),
    ]
}

/// The dock icon when running straight out of `target/`; a bundle has
/// `AppIcon.icns` instead.
const ICON: vampir::AppIcon =
    vampir::AppIcon::png(include_bytes!("../packaging/macos/icon-512.png"));

thread_local! {
    /// What every window shares, for the Dock icon's reopening, which is
    /// set up before it exists.
    static COMMON: RefCell<Option<Rc<Common>>> = const { RefCell::new(None) };
}

/// The frontmost browser window.
fn active_browser(cx: &mut App) -> Option<gpui::WindowHandle<Browser>> {
    cx.active_window()
        .and_then(|w| w.downcast::<Browser>())
        .or_else(|| cx.windows().into_iter().find_map(|w| w.downcast::<Browser>()))
}

/// Opens a browser window, a little down and right of the frontmost one.
fn open_browser_window(
    cx: &mut App,
    common: Rc<Common>,
    private: bool,
    restore: Option<state::SavedWindow>,
    launch: bool,
    carry: Option<BrowserTab>,
) -> Option<gpui::WindowHandle<Browser>> {
    let window_size = size(px(1100.0), px(760.0));
    let remembered = restore
        .as_ref()
        .and_then(|w| w.bounds)
        .filter(|b| b[2] >= 400.0 && b[3] >= 300.0)
        .map(|b| Bounds::new(point(px(b[0]), px(b[1])), size(px(b[2]), px(b[3]))));
    let bounds = if let Some(bounds) = remembered {
        bounds
    } else {
        match cx.active_window().and_then(|w| w.update(cx, |_, window, _| window.bounds()).ok()) {
        // A step down and right of the front window, if that stays on
        // screen; otherwise centred.
        Some(front)
            if cx.displays().iter().any(|display| {
                let screen = display.bounds();
                let next = front.origin + point(px(26.0), px(26.0));
                next.x >= screen.origin.x
                    && next.y >= screen.origin.y
                    && next.x + front.size.width <= screen.origin.x + screen.size.width
                    && next.y + front.size.height <= screen.origin.y + screen.size.height
            }) =>
        {
            Bounds::new(front.origin + point(px(26.0), px(26.0)), front.size)
        }
        _ => Bounds::centered(None, window_size, cx),
    }
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(700.0), px(460.0))),
        // The toolbar is the titlebar, so the whole window takes the
        // active tab's hue. It drags the window itself.
        titlebar: Some(TitlebarOptions {
            title: Some(if private { "Private Browsing" } else { "Vamprowser" }.into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(TRAFFIC_LIGHTS.0), px(TRAFFIC_LIGHTS.1))),
        }),
        app_owns_titlebar_drag: true,
        ..Default::default()
    };
    let handle = cx
        .open_window(options, move |window, cx| {
            cx.new(|cx| Browser::new(window, cx, common, private, restore, launch, carry))
        })
        .ok()?;
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    Some(handle)
}

/// Sends an extension's event to the window it concerns: a tab's own
/// window, the frontmost ordinary window for a new tab, else the oldest.
fn dispatch_extension_event(common: &Rc<Common>, event: ExtensionEvent, cx: &mut App) {
    if let ExtensionEvent::Upgraded { id, error } = event {
        if let Some(extensions) = common.extensions.borrow().as_ref() {
            extensions.finish_upgrade(&id, error);
        }
        return;
    }
    let browsers = common.browsers();
    let target = match &event {
        ExtensionEvent::CloseTab { tab_id } | ExtensionEvent::ActivateTab { tab_id } => browsers
            .iter()
            .find(|b| b.read(cx).index_of(*tab_id).is_some())
            .cloned(),
        ExtensionEvent::OpenTab { .. } => {
            let front = active_browser(cx).and_then(|h| h.entity(cx).ok());
            front
                .filter(|b| !b.read(cx).private)
                .or_else(|| browsers.iter().find(|b| !b.read(cx).private).cloned())
        }
        _ => browsers.first().cloned(),
    };
    if matches!(event, ExtensionEvent::Changed | ExtensionEvent::ActionChanged) {
        for browser in browsers.iter().skip(1) {
            browser.update(cx, |_, cx| cx.notify());
        }
    }
    // A tab to open with no ordinary window to open it in: one opens.
    let target = target.or_else(|| {
        matches!(event, ExtensionEvent::OpenTab { .. })
            .then(|| open_browser_window(cx, common.clone(), false, None, false, None))
            .flatten()
            .and_then(|handle| handle.entity(cx).ok())
    });
    if let Some(browser) = target {
        let _ = browser.read(cx).sender.try_send(BrowserEvent::Extension(event));
    }
}

/// A menu command with every window closed (the app keeps running): a
/// window opens for it (the last one closed, to reopen it), and it runs
/// there if there's more to it than the window.
fn without_window(command: Command, cx: &mut App) {
    let Some(common) = COMMON.with(|slot| slot.borrow().clone()) else {
        return;
    };
    let (private, restore) = match command {
        Command::ReopenClosedTab | Command::ReopenClosedWindow => {
            let closed = common.closed_windows.borrow_mut().pop();
            (false, closed)
        }
        Command::NewPrivateTab => (true, None),
        _ => (false, None),
    };
    let Some(handle) = open_browser_window(cx, common, private, restore, false, None) else {
        return;
    };
    let done = matches!(
        command,
        Command::NewTab | Command::NewPrivateTab | Command::ReopenClosedTab | Command::ReopenClosedWindow
    );
    if !done {
        let _ = handle.update(cx, |browser, window, cx| browser.run(command, window, cx));
    }
}

/// Hands each menu-bar action to the frontmost browser as a [`Command`].
/// Keyboard shortcuts arrive through the key monitor instead, so they also
/// work while a page has the keyboard.
macro_rules! route {
    ($cx:expr, { $($action:ident => $command:expr),* $(,)? }) => {
        $(
            {
                $cx.on_action(move |_: &$action, cx: &mut App| {
                    match active_browser(cx) {
                        Some(browser) => {
                            let _ = browser.update(cx, |this, window, cx| this.run($command, window, cx));
                        }
                        None => without_window($command, cx),
                    }
                });
            }
        )*
    };
}

fn main() {
    // Before anything reads the profile or starts WebKit.
    sitedata::finish_import();
    sitedata::adopt();
    let app = application();
    // URLs and files macOS asks us to open, possibly before the window
    // exists: they wait in the channel until the browser reads it.
    let (opened_tx, opened_rx) = async_channel::unbounded::<Vec<String>>();
    app.on_open_urls(move |urls| {
        let _ = opened_tx.try_send(urls);
    });
    // The Dock icon clicked with no window open: a new one. The ones
    // closed come back with ⌘⇧T.
    app.on_reopen(|cx| {
        if cx.windows().iter().any(|w| w.downcast::<Browser>().is_some()) {
            return;
        }
        let Some(common) = COMMON.with(|slot| slot.borrow().clone()) else {
            return;
        };
        open_browser_window(cx, common, false, None, false, None);
    });
    app.run(move |cx: &mut App| {
        let bundled = std::env::current_exe()
            .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
        if !bundled {
            ICON.install(cx);
        }
        bind_keys(cx);
        // Shown next to the menu items. The key monitor does the work.
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("cmd-alt-h", HideOthers, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-t", NewTab, None),
            KeyBinding::new("cmd-n", NewWindow, None),
            KeyBinding::new("cmd-shift-n", NewPrivateWindow, None),
            KeyBinding::new("cmd-shift-w", CloseWindow, None),
            KeyBinding::new("cmd-shift-t", ReopenClosedTab, None),
            KeyBinding::new("cmd-w", CloseTab, None),
            KeyBinding::new("cmd-l", OpenLocation, None),
            KeyBinding::new("cmd-r", Reload, None),
            KeyBinding::new("cmd-shift-r", EraseCacheAndReload, None),
            KeyBinding::new("cmd-[", Back, None),
            KeyBinding::new("cmd-]", Forward, None),
            KeyBinding::new("cmd-shift-h", GoHome, None),
            KeyBinding::new("cmd-d", BookmarkPage, None),
            KeyBinding::new("cmd-shift-b", ToggleBookmarks, None),
            KeyBinding::new("cmd-shift-l", ToggleVerticalTabs, None),
            KeyBinding::new("cmd-shift-m", ToggleMinimalMode, None),
            KeyBinding::new("cmd-alt-r", ToggleReaderMode, None),
            KeyBinding::new("cmd-alt-b", ShowBookmarks, None),
            KeyBinding::new("cmd-k", SwitchTabs, None),
            KeyBinding::new("cmd-y", ShowHistory, None),
            KeyBinding::new("cmd-alt-l", ShowDownloads, None),
            KeyBinding::new("cmd-=", ZoomIn, None),
            KeyBinding::new("cmd--", ZoomOut, None),
            KeyBinding::new("cmd-0", ZoomReset, None),
            KeyBinding::new("cmd-p", Print, None),
            KeyBinding::new("cmd-f", FindInPage, None),
            KeyBinding::new("cmd-g", FindNext, None),
            KeyBinding::new("cmd-shift-g", FindPrevious, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("cmd-.", Stop, None),
            KeyBinding::new("cmd-alt-i", ShowWebInspector, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        ]);
        cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
        cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx: &mut App| cx.unhide_other_apps());
        cx.set_menus(menus());
        cx.set_dock_menu(vec![
            MenuItem::action("New Window", NewWindow),
            MenuItem::action("New Private Window", NewPrivateWindow),
            MenuItem::action("New Tab", NewTab),
        ]);
        // Closing the last window leaves the app running, as Safari does;
        // the Dock icon brings it back.
        let (extension_tx, extension_rx) = async_channel::unbounded::<ExtensionEvent>();
        let (anywhere_tx, anywhere_rx) = async_channel::unbounded::<BrowserEvent>();
        let common = Common::new(extension_tx, anywhere_tx);
        COMMON.with(|slot| *slot.borrow_mut() = Some(common.clone()));
        // Last session's windows, if the startup setting restores them;
        // otherwise one window as it says, where the first was, and ⌘⇧T
        // brings the others back.
        let saved = common.saved.borrow().clone();
        let windows = if saved.windows.is_empty() && !saved.tabs.is_empty() {
            // From a version that kept only one window.
            vec![state::SavedWindow {
                tabs: saved.tabs.clone(),
                selected: saved.selected,
                ..Default::default()
            }]
        } else {
            saved.windows.clone()
        };
        let settings = Settings::load();
        if settings.startup == Startup::Restore && !windows.is_empty() {
            for window in windows {
                open_browser_window(cx, common.clone(), false, Some(window), true, None);
            }
        } else {
            let fresh = state::SavedWindow {
                bounds: windows.first().and_then(|w| w.bounds),
                ..Default::default()
            };
            // The first window comes back first.
            for window in windows.into_iter().rev() {
                common.remember_closed(window, &settings.home_page);
            }
            open_browser_window(cx, common.clone(), false, Some(fresh), true, None);
        }
        // Extensions speak to the browser as a whole; each event goes to the
        // window it concerns.
        let dispatch = common.clone();
        cx.spawn(async move |cx| {
            while let Ok(event) = extension_rx.recv().await {
                let common = dispatch.clone();
                cx.update(|cx| dispatch_extension_event(&common, event, cx));
            }
        })
        .detach();
        // Answers for no window in particular go to the front one, else the
        // oldest: whichever is open when they arrive.
        let anywhere = common.clone();
        cx.spawn(async move |cx| {
            while let Ok(event) = anywhere_rx.recv().await {
                let common = anywhere.clone();
                cx.update(|cx| {
                    let front = active_browser(cx)
                        .and_then(|h| h.entity(cx).ok())
                        .filter(|b| common.browsers().iter().any(|o| o.entity_id() == b.entity_id()));
                    match front.or_else(|| common.browsers().first().cloned()) {
                        Some(browser) => {
                            let _ = browser.read(cx).sender.try_send(event);
                        }
                        None => common.stranded.borrow_mut().push(event),
                    }
                });
            }
        })
        .detach();
        // Extensions update themselves, checked a while after launch and
        // hourly after that, at most once a day, whichever windows are open.
        let updating = common.clone();
        cx.spawn(async move |cx| {
            let mut wait = Duration::from_secs(90);
            loop {
                cx.background_executor().timer(wait).await;
                wait = Duration::from_secs(60 * 60);
                let common = updating.clone();
                cx.update(|cx| {
                    if let Some(browser) = common.browsers().first() {
                        browser.update(cx, |browser, cx| {
                            if browser.settings.auto_update_extensions && updates::due() {
                                browser.check_extension_updates(false, cx);
                            }
                        });
                    }
                });
            }
        })
        .detach();
        // URLs and files macOS hands over, including the ones that launched
        // the app, go to the frontmost window.
        let forward = common.clone();
        cx.spawn(async move |cx| {
            while let Ok(urls) = opened_rx.recv().await {
                let common = forward.clone();
                cx.update(|cx| {
                    // Never into a private window: links from other apps
                    // are ordinary browsing.
                    let ordinary = |handle: &gpui::WindowHandle<Browser>, cx: &App| {
                        handle.entity(cx).is_ok_and(|b| !b.read(cx).private)
                    };
                    let target = active_browser(cx).filter(|h| ordinary(h, cx)).or_else(|| {
                        common.browsers().into_iter().find(|b| !b.read(cx).private).and_then(|b| {
                            cx.windows().into_iter().find_map(|w| {
                                let handle = w.downcast::<Browser>()?;
                                (handle.entity(cx).ok()?.entity_id() == b.entity_id()).then_some(handle)
                            })
                        })
                    });
                    match target {
                        Some(browser) => {
                            let _ = browser.update(cx, |this, window, cx| {
                                this.open_external(urls, window, cx)
                            });
                        }
                        None => {
                            open_browser_window(
                                cx,
                                common.clone(),
                                false,
                                Some(state::SavedWindow { tabs: urls, ..Default::default() }),
                                false,
                                None,
                            );
                        }
                    }
                });
            }
        })
        .detach();
        // History goes to disk every half minute when it has changed, and
        // the cookie jar's copy in the profile is kept up to date.
        let history = common.clone();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                history.history.borrow_mut().save();
                sitedata::keep_cookies();
            }
        })
        .detach();
        // However the app quits — ⌘Q, the Dock, logging out — every window
        // stays in the session, and whatever was waiting to be written goes
        // out now, before GPUI closes the windows.
        let quitting = common.clone();
        cx.on_app_quit(move |cx| {
            quitting.quitting.set(true);
            for browser in quitting.browsers() {
                browser.update(cx, |browser, _| browser.persist());
            }
            quitting.history.borrow_mut().save();
            sitedata::keep_cookies();
            async {}
        })
        .detach();
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        let new_window = common.clone();
        cx.on_action(move |_: &NewWindow, cx: &mut App| {
            open_browser_window(cx, new_window.clone(), false, None, false, None);
        });
        let new_private = common.clone();
        cx.on_action(move |_: &NewPrivateWindow, cx: &mut App| {
            open_browser_window(cx, new_private.clone(), true, None, false, None);
        });
        route!(cx, {
            NewTab => Command::NewTab,
            NewPrivateTab => Command::NewPrivateTab,
            CloseWindow => Command::CloseWindow,
            ReopenClosedTab => Command::ReopenClosedTab,
            CloseTab => Command::CloseTab,
            OpenLocation => Command::FocusAddress,
            Back => Command::Back,
            Forward => Command::Forward,
            Reload => Command::Reload,
            EraseCacheAndReload => Command::EraseCacheAndReload,
            GoHome => Command::Home,
            BookmarkPage => Command::BookmarkPage,
            ToggleBookmarks => Command::ToggleBookmarksBar,
            ToggleVerticalTabs => Command::ToggleVerticalTabs,
            ToggleMinimalMode => Command::ToggleMinimalMode,
            ToggleReaderMode => Command::ToggleReaderMode,
            ShowBookmarks => Command::ShowBookmarks,
            ImportBookmarks => Command::ShowBookmarks,
            NextTab => Command::NextTab,
            PreviousTab => Command::PreviousTab,
            SwitchTabs => Command::SwitchTabs,
            OpenSettings => Command::Settings(Section::General),
            ShowHistory => Command::Settings(Section::History),
            ShowDownloads => Command::ShowDownloads,
            ClearHistory => Command::ClearHistory,
            ZoomIn => Command::ZoomIn,
            ZoomOut => Command::ZoomOut,
            ZoomReset => Command::ZoomReset,
            Print => Command::Print,
            MakeDefaultBrowser => Command::MakeDefaultBrowser,
            CustomizeToolbar => Command::Settings(Section::Toolbar),
            ManageExtensions => Command::Settings(Section::Extensions),
            ReopenClosedWindow => Command::ReopenClosedWindow,
            Minimize => Command::Minimize,
            Zoom => Command::Zoom,
            Stop => Command::Stop,
            ShowWebInspector => Command::ShowWebInspector,
            FindInPage => Command::Find,
            FindNext => Command::FindNext,
            FindPrevious => Command::FindPrevious,
            About => Command::Settings(Section::About),
        });
        cx.activate(true);
    });
}

#[cfg(test)]
mod browser_input_tests {
    use super::*;

    #[test]
    fn command_arrows_navigate_without_option_or_shift() {
        let cmd = NSEventModifierFlags::Command;
        let idle = KeyState::default();
        assert!(matches!(shortcut("", 123, cmd, idle), Some(Command::Back)));
        assert!(matches!(shortcut("", 124, cmd, idle), Some(Command::Forward)));
        assert!(shortcut("", 123, cmd | NSEventModifierFlags::Option, idle).is_none());
        assert!(shortcut("", 124, NSEventModifierFlags::empty(), idle).is_none());
        // Editing text, they move the caret.
        let editing = KeyState { editing: true, ..idle };
        assert!(shortcut("", 123, cmd, editing).is_none());
        assert!(shortcut("", 124, cmd, editing).is_none());
    }

    #[test]
    fn command_shift_c_copies_current_link() {
        let cmd_shift = NSEventModifierFlags::Command | NSEventModifierFlags::Shift;
        let editing = KeyState { editing: true, ..KeyState::default() };
        assert!(matches!(shortcut("c", 8, cmd_shift, editing), Some(Command::CopyLink)));
        assert!(shortcut("c", 8, NSEventModifierFlags::Command, editing).is_none());
        assert!(shortcut("c", 8, cmd_shift | NSEventModifierFlags::Option, editing).is_none());
    }

    #[test]
    fn side_mouse_buttons_navigate() {
        assert!(matches!(mouse_navigation(3), Some(Command::Back)));
        assert!(matches!(mouse_navigation(4), Some(Command::Forward)));
        assert!(mouse_navigation(2).is_none());
        assert!(mouse_navigation(5).is_none());
    }

    #[test]
    fn option_command_r_opens_reader_mode() {
        let flags = NSEventModifierFlags::Command | NSEventModifierFlags::Option;
        assert!(matches!(shortcut("r", 15, flags, KeyState::default()), Some(Command::ToggleReaderMode)));
        assert!(matches!(shortcut("r", 15, NSEventModifierFlags::Command, KeyState::default()), Some(Command::Reload)));
    }

    #[test]
    fn shift_delete_removes_only_a_picked_row() {
        let shift = NSEventModifierFlags::Shift;
        let suggesting = KeyState { suggesting: true, ..KeyState::default() };
        assert!(shortcut("", 51, shift, suggesting).is_none());
        let picked = KeyState { removable: true, ..suggesting };
        assert!(matches!(shortcut("", 51, shift, picked), Some(Command::RemoveSuggestion)));
    }
}
