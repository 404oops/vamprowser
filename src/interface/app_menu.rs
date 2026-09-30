//! Browser-owned context menus in a Vampir popup above WebKit.
//!
//! It behaves like a native menu: it floats above the window without
//! taking the keyboard from it (the field or page being typed in keeps its
//! caret), one is open at a time, and a click anywhere else in the app, a
//! key, or switching to another app closes it; that click does nothing
//! else.

use std::{cell::Cell, cell::RefCell, ptr::NonNull, rc::Rc, time::Duration};

use async_channel::{Receiver, Sender};
use block2::RcBlock;
use gpui::{
    Animation, AnimationExt, App, Bounds, Context, DisplayId, FontWeight, Point, Render, Task,
    Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, prelude::*,
    px, size,
};
use objc2::{
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType};
use objc2_foundation::{NSNotificationCenter, NSObjectProtocol};
use vampir::{ControlHost, ControlState, Palette, color, lighting, ui_font};

use crate::{
    icons::{Icon, icon},
    native::MenuEntry,
};

/// A menu is as wide as its longest row, within these.
const MIN_WIDTH: f32 = 184.0;
const MAX_WIDTH: f32 = 340.0;
const ROW_HEIGHT: f32 = 28.0;
/// A separator's hairline and the room either side of it.
const SEPARATOR_HEIGHT: f32 = 11.0;
/// Between the panel's edge and its rows.
const PADDING: f32 = 6.0;
const BORDER: f32 = 1.0;
const RADIUS: f32 = 12.0;
const ROW_RADIUS: f32 = 7.0;
/// The shared panel shadow reaches three blur radii (30 points), plus
/// its two-point downward offset. Keep its tail inside the native popup.
const EDGE_GUTTER: f32 = 32.0;
/// Between a row's highlight and what's in it.
const ROW_INSET: f32 = 8.0;
const TEXT_SIZE: f32 = 13.0;
const ICON_SIZE: f32 = 15.0;
/// The column icons sit in, and the one checks and chevrons sit in.
const ICON_SLOT: f32 = 18.0;
const TRAILING_SLOT: f32 = 14.0;
const GAP: f32 = 8.0;
/// Taller menus scroll.
const MAX_HEIGHT: f32 = 520.0;
/// Clear space the popup keeps above its panel. AppKit treats the top of
/// any titled window, even one with no visible title bar, as its title
/// bar: a click there in a window that isn't key goes to moving the
/// window instead of to the view. A menu never becomes key, so its rows
/// stay below that band.
const TITLE_BAND: f32 = 28.0;

/// How long the panel opens, its rows sweep, and its size changes.
const SWEEP: Duration = Duration::from_millis(190);
const ROW_SWEEP: Duration = Duration::from_millis(170);
const RESIZE: Duration = Duration::from_millis(170);
const HOVER: Duration = Duration::from_millis(120);

/// Which way a menu comes in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sweep {
    /// Unrolling down from the pointer.
    Down,
    /// Unrolling up from a shelf.
    Up,
    /// Into a submenu: rows arrive from the right.
    Forward,
    /// Back out of one: rows arrive from the left.
    Back,
}

impl Sweep {
    /// Where a row starts, relative to where it settles.
    fn offset(self) -> (f32, f32) {
        match self {
            Sweep::Down => (0.0, -6.0),
            Sweep::Up => (0.0, 6.0),
            Sweep::Forward => (16.0, 0.0),
            Sweep::Back => (-16.0, 0.0),
        }
    }

    /// Whether the panel itself unrolls, rather than just its rows.
    fn unrolls(self) -> bool {
        matches!(self, Sweep::Down | Sweep::Up)
    }
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// One level of a menu: the top, or a submenu opened from it.
#[derive(Clone)]
struct Level {
    entries: Vec<MenuEntry>,
    // The first index after its parent row. Native menu numbering
    // included submenu rows and separators, so keep that scheme.
    base: usize,
    /// The row that opened it, shown at the top as the way back.
    title: Option<String>,
}

/// What the watch tells a menu from outside it.
enum Signal {
    Close,
    /// Escape in a submenu: back to the level above.
    Back,
}

/// The menu showing, if one is.
struct OpenMenu {
    ns_window: usize,
    dismiss: Sender<Signal>,
    dismissed: Rc<Cell<bool>>,
}

/// What closes a menu from outside it: presses and keys anywhere else in
/// the app, and the app going to the background.
struct Watch {
    monitor: Option<Retained<AnyObject>>,
    resign: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

thread_local! {
    static OPEN: RefCell<Option<OpenMenu>> = const { RefCell::new(None) };
}

struct Menu {
    controls: ControlState,
    palette: Palette,
    levels: Vec<Level>,
    /// Flattened command index for each row in the current level.
    row_indices: Vec<usize>,
    answer: Sender<Option<usize>>,
    ns_window: usize,
    watch: Option<Watch>,
    done: bool,
    /// GPUI draws once while the popup is hidden. Warm its text and icons
    /// before starting the short opening animation.
    animations_ready: bool,
    sweep: Sweep,
    anchored_above: bool,
    animation_epoch: usize,
    depth: Rc<Cell<usize>>,
    /// The visible panel's target size. The popup itself fits every level.
    width: f32,
    height: f32,
    /// The row under the pointer, whose icon takes the accent.
    hovered: Option<usize>,
    _dismiss: Option<Task<()>>,
}

impl ControlHost for Menu {
    fn control_state(&self) -> &ControlState {
        &self.controls
    }
    fn control_state_mut(&mut self) -> &mut ControlState {
        &mut self.controls
    }
}

fn width_of(entry: &MenuEntry) -> usize {
    match entry {
        MenuEntry::Submenu { entries, .. } => 1 + entries.iter().map(width_of).sum::<usize>(),
        _ => 1,
    }
}

fn row_indices(level: &Level) -> Vec<usize> {
    let mut next = level.base;
    level
        .entries
        .iter()
        .map(|entry| {
            let index = next;
            next += width_of(entry);
            index
        })
        .collect()
}

/// The height `level` needs, with its way back if it's nested.
fn height_of(level: &Level) -> f32 {
    height_of_entries(&level.entries, level.title.is_some())
}

fn height_of_entries(entries: &[MenuEntry], has_title: bool) -> f32 {
    let rows: f32 = entries
        .iter()
        .map(|entry| match entry {
            MenuEntry::Separator => SEPARATOR_HEIGHT,
            _ => ROW_HEIGHT,
        })
        .sum();
    let back = if has_title {
        ROW_HEIGHT + SEPARATOR_HEIGHT
    } else {
        0.0
    };
    (rows + back + 2.0 * (PADDING + BORDER)).min(MAX_HEIGHT)
}

/// Whether any row of `entries` has an icon, so every label lines up after
/// the icon column; and whether any has a check or a chevron after it.
fn columns(entries: &[MenuEntry]) -> (bool, bool) {
    let icons = entries.iter().any(|entry| entry.icon().is_some());
    let trailing = entries.iter().any(|entry| {
        matches!(
            entry,
            MenuEntry::Submenu { .. } | MenuEntry::Item { checked: true, .. }
        )
    });
    (icons, trailing)
}

/// The width that fits `level`'s longest row, given how wide `measure`
/// finds a label at a weight.
fn width_of_level(level: &Level, measure: impl Fn(&str, FontWeight) -> f32) -> f32 {
    width_of_entries(&level.entries, level.title.as_deref(), measure)
}

fn width_of_entries(
    entries: &[MenuEntry],
    title: Option<&str>,
    measure: impl Fn(&str, FontWeight) -> f32,
) -> f32 {
    let (icons, trailing) = columns(entries);
    let label = entries
        .iter()
        .filter_map(|entry| match entry {
            MenuEntry::Item { label, .. } | MenuEntry::Submenu { label, .. } => {
                Some(measure(label, FontWeight::NORMAL))
            }
            MenuEntry::Separator => None,
        })
        .fold(0.0, f32::max);
    let mut row = label;
    if icons {
        row += ICON_SLOT + GAP;
    }
    if trailing {
        row += TRAILING_SLOT + GAP;
    }
    // The way back keeps the level's columns, with its chevron in the
    // icon's and its title set medium.
    let back = title.map_or(0.0, |title| {
        let mut back = ICON_SLOT + GAP + measure(title, FontWeight::MEDIUM);
        if trailing {
            back += TRAILING_SLOT + GAP;
        }
        back
    });
    // A little over, so the longest label isn't flush against the edge.
    let slack = 12.0;
    (row.max(back) + 2.0 * (PADDING + BORDER + ROW_INSET) + slack).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// How wide `text` sets in the menu's font at `weight`, from its glyphs'
/// advances.
fn measure(cx: &App, text: &str, weight: FontWeight) -> f32 {
    let system = cx.text_system();
    let mut font = gpui::font(ui_font());
    font.weight = weight;
    let font = system.resolve_font(&font);
    let size = px(TEXT_SIZE);
    text.chars()
        .map(|ch| {
            system
                .advance(font, size, ch)
                .map_or(TEXT_SIZE * 0.6, |advance| f32::from(advance.width))
        })
        .sum()
}

/// A transparent popup large enough for any level, so changing levels does
/// not resize its native window.
fn level_sizes(
    level: &Level,
    measure: impl Fn(&str, FontWeight) -> f32 + Copy,
) -> ((f32, f32), (f32, f32)) {
    let root = (width_of_level(level, measure), height_of(level));
    (root, maximum_size_from(&level.entries, root, measure))
}

fn maximum_size_from(
    entries: &[MenuEntry],
    (mut width, mut height): (f32, f32),
    measure: impl Fn(&str, FontWeight) -> f32 + Copy,
) -> (f32, f32) {
    for entry in entries {
        if let MenuEntry::Submenu { label, entries, .. } = entry {
            let child = (
                width_of_entries(entries, Some(label), measure),
                height_of_entries(entries, true),
            );
            let (child_width, child_height) = maximum_size_from(entries, child, measure);
            width = width.max(child_width);
            height = height.max(child_height);
        }
    }
    (width, height)
}

/// Takes the menu window off the screen at once, before GPUI gets round to
/// closing it.
fn hide(ns_window: usize) {
    if ns_window == 0 {
        return;
    }
    // SAFETY: the menu's own window, still open: it is only closed by
    // `Menu::finish`, which forgets it first.
    let window = unsafe { &*(ns_window as *const objc2_app_kit::NSWindow) };
    window.orderOut(None);
}

/// Closes the menu showing, if there is one.
fn dismiss_open() {
    if let Some(open) = OPEN.with(|slot| slot.borrow_mut().take()) {
        open.dismissed.set(true);
        hide(open.ns_window);
        let _ = open.dismiss.try_send(Signal::Close);
    }
}

/// Closes the menu at `ns_window` from outside it, at once.
fn close_from_outside(ns_window: usize, dismiss: &Sender<Signal>, dismissed: &Cell<bool>) {
    dismissed.set(true);
    OPEN.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|open| open.ns_window == ns_window)
        {
            *slot = None;
        }
    });
    hide(ns_window);
    let _ = dismiss.try_send(Signal::Close);
}

/// Watches for what closes the menu at `ns_window`: a press outside it,
/// which goes no further, as with a native menu; any key, Escape going no
/// further; and the app going to the background.
fn watch(
    ns_window: usize,
    dismiss: Sender<Signal>,
    dismissed: Rc<Cell<bool>>,
    depth: Rc<Cell<usize>>,
) -> Watch {
    let (on_resign, resigned) = (dismiss.clone(), dismissed.clone());
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit owns the event for the callback's duration.
        let event_ref = unsafe { event.as_ref() };
        if dismissed.get() {
            return event.as_ptr();
        }
        if event_ref.r#type() == NSEventType::KeyDown {
            if event_ref.keyCode() == 53 && depth.get() > 1 {
                let _ = dismiss.try_send(Signal::Back);
                return std::ptr::null_mut();
            }
            close_from_outside(ns_window, &dismiss, &dismissed);
            // Escape.
            return if event_ref.keyCode() == 53 {
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            };
        }
        if crate::event_in(event_ref, ns_window) {
            return event.as_ptr();
        }
        close_from_outside(ns_window, &dismiss, &dismissed);
        std::ptr::null_mut()
    });
    // SAFETY: the block returns the live event or null, as AppKit requires.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown
                | NSEventMask::RightMouseDown
                | NSEventMask::OtherMouseDown
                | NSEventMask::KeyDown,
            &handler,
        )
    };
    let block = RcBlock::new(move |_| {
        if !resigned.get() {
            close_from_outside(ns_window, &on_resign, &resigned);
        }
    });
    // SAFETY: AppKit's own notification name and a block of the documented
    // type; the observer is removed with the watch.
    let resign = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(objc2_app_kit::NSApplicationDidResignActiveNotification),
            None,
            None,
            &block,
        )
    };
    Watch { monitor, resign }
}

/// What follows a row's label.
enum Trail {
    Nothing,
    Check,
    Chevron,
}

impl Menu {
    fn finish(&mut self, choice: Option<usize>, window: &mut Window) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        let _ = self.answer.try_send(choice);
        self.stop_watching();
        let ns_window = self.ns_window;
        allow_key(ns_window);
        OPEN.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|open| open.ns_window == ns_window)
            {
                *slot = None;
            }
        });
        hide(ns_window);
        window.remove_window();
    }

    fn stop_watching(&mut self) {
        let Some(watch) = self.watch.take() else {
            return;
        };
        if let Some(monitor) = watch.monitor {
            // SAFETY: the token AppKit returned for this monitor.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        // SAFETY: the observer the notification center returned.
        unsafe {
            NSNotificationCenter::defaultCenter()
                .removeObserver(ProtocolObject::as_ref(&*watch.resign))
        };
    }

    /// Shows the next level in the same popup, resizing it to fit.
    fn show_level(&mut self, row: usize, base: usize, cx: &mut Context<Self>) {
        let Some(MenuEntry::Submenu { label, entries, .. }) =
            self.levels.last().and_then(|level| level.entries.get(row))
        else {
            return;
        };
        self.levels.push(Level {
            entries: entries.clone(),
            base,
            title: Some(label.clone()),
        });
        self.change_level(Sweep::Forward, cx);
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.levels.len() > 1 {
            self.levels.pop();
            self.change_level(Sweep::Back, cx);
        }
    }

    fn change_level(&mut self, sweep: Sweep, cx: &mut Context<Self>) {
        let level = self.levels.last().expect("menu has a level");
        self.width = width_of_level(level, |text, weight| measure(cx, text, weight));
        self.height = height_of(level);
        self.row_indices = row_indices(level);
        self.sweep = sweep;
        self.hovered = None;
        self.animation_epoch += 1;
        self.depth.set(self.levels.len());
        cx.notify();
    }

    /// A row: `key` tells it apart for hovering, `icons` and `trailing` say
    /// whether the level keeps a column for an icon before the label and
    /// for a check or chevron after it.
    fn row(
        &self,
        key: usize,
        label: String,
        glyph: Option<Icon>,
        (icons, trailing): (bool, bool),
        trail: Trail,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette;
        let hovered = enabled && self.hovered == Some(key);
        let hover = if cx.reduce_motion() {
            self.controls
                .snap(("menu-row-hover", key), if hovered { 1.0 } else { 0.0 })
        } else {
            self.controls
                .blend(("menu-row-hover", key), hovered, crate::slowed(HOVER))
        };
        let tint = color::lerp(palette.text_secondary, palette.accent, hover);
        div()
            .id(("menu-row", key))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_INSET))
            .flex()
            .items_center()
            .gap(px(GAP))
            .rounded(px(ROW_RADIUS))
            .when(hover > 0.0, |el| {
                el.bg(lighting::lit_at(palette.soft_fill, 0.035, hover))
            })
            .when(enabled, |el| el.cursor_pointer())
            .when(!enabled, |el| el.opacity(0.4))
            .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                if *over {
                    this.hovered = Some(key);
                } else if this.hovered == Some(key) {
                    this.hovered = None;
                } else {
                    return;
                }
                cx.notify();
            }))
            .when(icons, |el| {
                el.child(
                    div()
                        .flex_none()
                        .w(px(ICON_SLOT))
                        .flex()
                        .justify_center()
                        .children(glyph.map(|glyph| icon(glyph, ICON_SIZE, tint))),
                )
            })
            .child(div().flex_1().min_w(px(0.0)).truncate().child(label))
            .when(trailing, |el| {
                el.child(
                    div()
                        .flex_none()
                        .w(px(TRAILING_SLOT))
                        .flex()
                        .justify_center()
                        .map(|slot| match trail {
                            Trail::Nothing => slot,
                            Trail::Check => slot.child(icon(Icon::Check, 13.0, palette.accent)),
                            Trail::Chevron => slot.child(icon(Icon::ChevronRight, 11.0, tint)),
                        }),
                )
            })
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        self.stop_watching();
        allow_key(self.ns_window);
    }
}

impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let palette = self.palette;
        let chrome = crate::Chrome::new(palette);
        let sweep = self.sweep;
        let level = self.levels.last().unwrap();
        let columns = columns(&level.entries);
        let separator = move || {
            div()
                .flex_none()
                .h(px(1.0))
                .mx(px(ROW_INSET))
                .my(px((SEPARATOR_HEIGHT - 1.0) / 2.0))
                .bg(color::with_alpha(chrome.line, 0.8))
        };
        let mut rows = Vec::with_capacity(level.entries.len() + 2);
        if let Some(title) = &level.title {
            // The way back, headed with the row that led here.
            let back = self
                .row(
                    usize::MAX,
                    title.clone(),
                    Some(Icon::ChevronLeft),
                    (true, columns.1),
                    Trail::Nothing,
                    true,
                    cx,
                )
                .text_color(palette.text_secondary)
                .font_weight(FontWeight::MEDIUM)
                .on_click(cx.listener(|this, _, _, cx| this.back(cx)));
            rows.push(back.into_any_element());
            rows.push(separator().into_any_element());
        }
        for (row_index, entry) in level.entries.iter().enumerate() {
            let index = self.row_indices[row_index];
            let row = match entry {
                MenuEntry::Separator => separator().into_any_element(),
                MenuEntry::Item {
                    label,
                    enabled,
                    checked,
                    icon: glyph,
                } => {
                    let trail = if *checked {
                        Trail::Check
                    } else {
                        Trail::Nothing
                    };
                    let row = self
                        .row(index, label.clone(), *glyph, columns, trail, *enabled, cx)
                        .when(*enabled, |el| {
                            el.on_click(cx.listener(move |this, _, window, _| {
                                this.finish(Some(index), window)
                            }))
                        });
                    row.into_any_element()
                }
                MenuEntry::Submenu {
                    label, icon: glyph, ..
                } => {
                    let row = self
                        .row(
                            index,
                            label.clone(),
                            *glyph,
                            columns,
                            Trail::Chevron,
                            true,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.show_level(row_index, index + 1, cx);
                        }));
                    row.into_any_element()
                }
            };
            rows.push(row);
        }
        if cx.reduce_motion() {
            self.controls.snap("menu-width", self.width);
            self.controls.snap("menu-height", self.height);
        }
        let width = self
            .controls
            .tween("menu-width", self.width, crate::slowed(RESIZE));
        let height = self
            .controls
            .tween("menu-height", self.height, crate::slowed(RESIZE));
        let (dx, dy) = sweep.offset();
        let rows = div()
            .id("menu-rows")
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .p(px(PADDING))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .children(rows);
        let rows = if self.animations_ready {
            rows.with_animation(
                ("menu-level-sweep", self.animation_epoch),
                Animation::new(crate::slowed(ROW_SWEEP)).with_easing(ease_out),
                move |rows, t| {
                    rows.relative()
                        .left(px(dx * (1.0 - t)))
                        .top(px(dy * (1.0 - t)))
                        .opacity(t)
                },
            )
            .into_any_element()
        } else {
            rows.into_any_element()
        };
        let panel = div()
            .id("menu-panel")
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .flex_none()
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(RADIUS))
            .border_1()
            .border_color(color::with_alpha(palette.field_border_strong, 0.5))
            .bg(chrome.raised)
            .shadow(lighting::panel(palette.is_dark))
            .child(rows);
        // Keep the native popup fixed; only its visible panel unrolls or
        // resizes. Animation IDs survive hover redraws and finish once.
        let panel = if self.animations_ready && sweep.unrolls() {
            panel
                .with_animation(
                    "menu-unroll",
                    Animation::new(crate::slowed(SWEEP)).with_easing(ease_out),
                    move |panel, t| {
                        panel
                            .h(px(height * (0.35 + 0.65 * t)))
                            .opacity((t * 1.8).min(1.0))
                    },
                )
                .into_any_element()
        } else {
            panel.into_any_element()
        };
        let root = vampir::root(div().id("app-menu"), self, cx)
            .size_full()
            .pt(px(TITLE_BAND))
            .pb(px(EDGE_GUTTER))
            .px(px(EDGE_GUTTER))
            .relative()
            .flex()
            .flex_col()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| this.finish(None, window)),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, _, window, _| this.finish(None, window)),
            )
            .when(self.anchored_above, |el| el.justify_end())
            .font_family(ui_font())
            .text_size(px(TEXT_SIZE))
            .text_color(palette.text_primary)
            .on_action(
                cx.listener(|this, _: &vampir::keyboard::Dismiss, window, cx| {
                    if this.levels.len() > 1 {
                        this.back(cx);
                    } else {
                        this.finish(None, window);
                    }
                }),
            )
            .child(panel);
        if self.controls.animating() {
            window.request_animation_frame();
        }
        root
    }
}

pub(crate) fn open(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    entries: Vec<MenuEntry>,
    palette: Palette,
) -> Receiver<Option<usize>> {
    open_at(cx, window, position, entries, palette, false)
}

/// Place a shelf menu with its bottom edge at the trigger, above the shelf.
pub(crate) fn open_above(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    entries: Vec<MenuEntry>,
    palette: Palette,
) -> Receiver<Option<usize>> {
    open_at(cx, window, position, entries, palette, true)
}

fn open_at(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    entries: Vec<MenuEntry>,
    palette: Palette,
    anchored_above: bool,
) -> Receiver<Option<usize>> {
    // One menu at a time.
    dismiss_open();
    let (sender, receiver) = async_channel::bounded(1);
    open_level(
        cx,
        window.bounds().origin + position,
        window
            .display(cx)
            .map(|display| (display.id(), display.bounds())),
        vec![Level {
            entries,
            base: 0,
            title: None,
        }],
        sender,
        palette,
        anchored_above,
    );
    receiver
}

fn open_level(
    cx: &mut App,
    mut origin: Point<gpui::Pixels>,
    display: Option<(DisplayId, Bounds<gpui::Pixels>)>,
    levels: Vec<Level>,
    answer: Sender<Option<usize>>,
    palette: Palette,
    anchored_above: bool,
) {
    let Some(level) = levels.last() else {
        return;
    };
    let ((width, height), (popup_width, popup_height)) =
        level_sizes(level, |text, weight| measure(cx, text, weight));
    let popup_height = popup_height + TITLE_BAND + EDGE_GUTTER;
    let popup_width = popup_width + 2.0 * EDGE_GUTTER;
    let initial_row_indices = row_indices(level);
    // Keep the panel's left and opening edge at the pointer while leaving
    // transparent space around its rounded border.
    origin.x -= px(EDGE_GUTTER);
    origin.y -= px(if anchored_above {
        popup_height - EDGE_GUTTER
    } else {
        TITLE_BAND
    });
    if let Some((_, screen)) = display {
        origin.x = origin
            .x
            .min(screen.origin.x + screen.size.width - px(popup_width))
            .max(screen.origin.x);
        origin.y = origin
            .y
            .min(screen.origin.y + screen.size.height - px(popup_height))
            .max(screen.origin.y);
    }
    let options = WindowOptions {
        display_id: display.map(|(id, _)| id),
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            origin,
            size(px(popup_width), px(popup_height)),
        ))),
        // Above everything, and it doesn't take the keyboard or make the
        // window it's for look inactive.
        kind: WindowKind::PopUp,
        focus: false,
        // Clear around the panel's rounded corners and while it unrolls.
        window_background: WindowBackgroundAppearance::Transparent,
        show: false,
        titlebar: None,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    let result = cx.open_window(options, move |window, cx| {
        let (ns_window, _) = crate::ns_window_of(window);
        let (dismiss, signals) = async_channel::unbounded();
        let dismissed = Rc::new(Cell::new(false));
        let depth = Rc::new(Cell::new(levels.len()));
        let watch = watch(ns_window, dismiss.clone(), dismissed.clone(), depth.clone());
        OPEN.with(|slot| {
            *slot.borrow_mut() = Some(OpenMenu {
                ns_window,
                dismiss,
                dismissed,
            })
        });
        let menu = cx.new(|_| {
            let controls = ControlState::default();
            controls.snap("menu-width", width);
            controls.snap("menu-height", height);
            Menu {
                controls,
                palette,
                levels,
                row_indices: initial_row_indices,
                answer,
                ns_window,
                watch: Some(watch),
                done: false,
                animations_ready: false,
                sweep: if anchored_above {
                    Sweep::Up
                } else {
                    Sweep::Down
                },
                anchored_above,
                animation_epoch: 0,
                depth,
                width,
                height,
                hovered: None,
                _dismiss: None,
            }
        });
        menu.update(cx, |menu, cx| {
            menu._dismiss = Some(cx.spawn_in(window, async move |this, cx| {
                while let Ok(signal) = signals.recv().await {
                    let closing = matches!(signal, Signal::Close);
                    let _ = this.update_in(cx, |menu, window, cx| match signal {
                        Signal::Close => menu.finish(None, window),
                        Signal::Back => menu.back(cx),
                    });
                    if closing {
                        break;
                    }
                }
            }));
        });
        menu
    });
    if let Ok(handle) = result {
        let _ = handle.update(cx, |menu, _, cx| {
            menu.animations_ready = true;
            cx.notify();
        });
        // End the Menu borrow before drawing it. Its required hidden draw
        // has warmed text and icons; replace that scene with the animation's
        // first frame before AppKit makes the popup visible.
        let _ = cx.update_window(handle.into(), |_, window, cx| {
            let (ns_window, _) = crate::ns_window_of(window);
            if ns_window == 0 {
                return;
            }
            // SAFETY: the native window belongs to this live GPUI window.
            let ns_window = unsafe { &*(ns_window as *const objc2_app_kit::NSWindow) };
            // The popup reserves transparent space for wider/taller
            // submenus and the edge gutter. A native shadow would outline it.
            ns_window.setHasShadow(false);
            never_key(ns_window);
            window.draw(cx).clear(cx);
            ns_window.orderFront(None);
        });
    }
}

/// Keeps `window` from becoming the key window while it's a menu.
///
/// GPUI's panels answer `canBecomeKeyWindow` with YES, so any click that
/// leaves the menu open — into a submenu, or back out of one — would take
/// the keyboard from the browser window. `becomesKeyOnlyIfNeeded` isn't
/// enough: AppKit still makes the panel key on a later click. So the
/// panel class's answer is replaced with one that says NO for the windows
/// in [`KEYLESS`], and what it said before for any other.
fn never_key(window: &objc2_app_kit::NSWindow) {
    use objc2::runtime::{AnyObject, Bool, Sel};

    type CanBecomeKey = unsafe extern "C-unwind" fn(&AnyObject, Sel) -> Bool;

    unsafe extern "C-unwind" fn can_become_key(this: &AnyObject, sel: Sel) -> Bool {
        let this_ptr = this as *const AnyObject as usize;
        if KEYLESS.with(|set| set.borrow().contains(&this_ptr)) {
            return Bool::NO;
        }
        let Some(original) = ORIGINAL_CAN_BECOME_KEY.with(Cell::get) else {
            return Bool::YES;
        };
        // SAFETY: the implementation this one replaced, of the same shape.
        let original: CanBecomeKey = unsafe { std::mem::transmute(original) };
        unsafe { original(this, sel) }
    }

    KEYLESS.with(|set| {
        set.borrow_mut()
            .insert(window as *const objc2_app_kit::NSWindow as usize)
    });
    if ORIGINAL_CAN_BECOME_KEY.with(Cell::get).is_some() {
        return;
    }
    let Some(method) = window
        .class()
        .instance_method(objc2::sel!(canBecomeKeyWindow))
    else {
        return;
    };
    let replacement: CanBecomeKey = can_become_key;
    // SAFETY: `canBecomeKeyWindow` takes nothing and returns a BOOL, as
    // the replacement does; the original is kept to answer for other
    // windows of the class.
    let original = unsafe { method.set_implementation(std::mem::transmute(replacement)) };
    ORIGINAL_CAN_BECOME_KEY.with(|slot| slot.set(Some(original)));
}

/// Lets `ns_window` become key again, once it's no longer a menu.
fn allow_key(ns_window: usize) {
    KEYLESS.with(|set| set.borrow_mut().remove(&ns_window));
}

thread_local! {
    /// The menu windows open: those answer `canBecomeKeyWindow` with NO.
    static KEYLESS: RefCell<std::collections::HashSet<usize>> =
        RefCell::new(std::collections::HashSet::new());
    /// What GPUI's panel class answered `canBecomeKeyWindow` with, before
    /// [`never_key`] replaced it.
    static ORIGINAL_CAN_BECOME_KEY: Cell<Option<objc2::runtime::Imp>> = const { Cell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_include_nested_rows() {
        let entries = [
            MenuEntry::item("A"),
            MenuEntry::Submenu {
                label: "B".into(),
                entries: vec![MenuEntry::item("C"), MenuEntry::Separator],
                icon: None,
            },
            MenuEntry::item("D"),
        ];
        assert_eq!(entries.iter().map(width_of).sum::<usize>(), 5);
    }

    #[test]
    fn menus_are_as_tall_as_their_rows() {
        let mut level = Level {
            entries: vec![
                MenuEntry::item("A"),
                MenuEntry::Separator,
                MenuEntry::item("B"),
            ],
            base: 0,
            title: None,
        };
        let top = height_of(&level);
        assert_eq!(
            top,
            2.0 * ROW_HEIGHT + SEPARATOR_HEIGHT + 2.0 * (PADDING + BORDER)
        );
        level.title = Some("Back to".into());
        assert_eq!(height_of(&level), top + ROW_HEIGHT + SEPARATOR_HEIGHT);
    }

    #[test]
    fn menus_are_as_wide_as_their_longest_row() {
        let by_char = |text: &str, _: FontWeight| text.chars().count() as f32 * 7.0;
        let short = Level {
            entries: vec![MenuEntry::item("Open")],
            base: 0,
            title: None,
        };
        assert_eq!(width_of_level(&short, by_char), MIN_WIDTH);
        let label = "Clear local storage and databases for every site you have visited";
        let long = Level {
            entries: vec![MenuEntry::item(label)],
            base: 0,
            title: None,
        };
        assert_eq!(width_of_level(&long, by_char), MAX_WIDTH);
        // An icon column and a check column make room for themselves.
        let plain = Level {
            entries: vec![MenuEntry::item("Close Tabs to the Right")],
            base: 0,
            title: None,
        };
        let dressed = Level {
            entries: vec![
                MenuEntry::checked("Close Tabs to the Right", true).with_icon(Icon::Tabs),
            ],
            base: 0,
            title: None,
        };
        assert_eq!(
            width_of_level(&dressed, by_char) - width_of_level(&plain, by_char),
            ICON_SLOT + TRAILING_SLOT + 2.0 * GAP
        );
    }

    #[test]
    fn popup_fits_submenus_before_they_open() {
        let level = Level {
            entries: vec![MenuEntry::Submenu {
                label: "Move to".into(),
                entries: vec![
                    MenuEntry::item("A longer destination than the parent row"),
                    MenuEntry::item("Another destination"),
                ],
                icon: None,
            }],
            base: 0,
            title: None,
        };
        let (_, (width, height)) = level_sizes(&level, |text, _| text.len() as f32 * 7.0);
        assert!(width > width_of_level(&level, |text, _| text.len() as f32 * 7.0));
        assert!(height > height_of(&level));
    }

    #[test]
    fn preparation_borrows_nested_labels_and_measures_each_level_once() {
        let level = Level {
            entries: vec![
                MenuEntry::item("Open"),
                MenuEntry::Separator,
                MenuEntry::Submenu {
                    label: "Move to".into(),
                    entries: vec![
                        MenuEntry::checked(
                            "A destination with a label wide enough to fill the panel",
                            true,
                        )
                        .with_icon(Icon::Folder),
                        MenuEntry::Submenu {
                            label: "Nested".into(),
                            entries: vec![MenuEntry::Separator, MenuEntry::item("Here")],
                            icon: None,
                        },
                    ],
                    icon: Some(Icon::Folder),
                },
            ],
            base: 0,
            title: Some("Parent".into()),
        };
        let MenuEntry::Item { label: open, .. } = &level.entries[0] else {
            unreachable!()
        };
        let MenuEntry::Submenu {
            label: move_to,
            entries,
            ..
        } = &level.entries[2]
        else {
            unreachable!()
        };
        let MenuEntry::Item {
            label: destination, ..
        } = &entries[0]
        else {
            unreachable!()
        };
        let MenuEntry::Submenu {
            label: nested,
            entries,
            ..
        } = &entries[1]
        else {
            unreachable!()
        };
        let MenuEntry::Item { label: here, .. } = &entries[1] else {
            unreachable!()
        };
        let parent = level.title.as_deref().unwrap();
        let calls = RefCell::new(Vec::new());
        let (root, maximum) = level_sizes(&level, |text, weight| {
            calls.borrow_mut().push((text.as_ptr(), weight));
            text.len() as f32 * 7.0
        });
        let expected_height = 3.0 * ROW_HEIGHT + 2.0 * SEPARATOR_HEIGHT + 2.0 * (PADDING + BORDER);
        assert_eq!(root, (MIN_WIDTH, expected_height));
        assert_eq!(maximum, (MAX_WIDTH, expected_height));
        assert_eq!(
            *calls.borrow(),
            vec![
                (open.as_ptr(), FontWeight::NORMAL),
                (move_to.as_ptr(), FontWeight::NORMAL),
                (parent.as_ptr(), FontWeight::MEDIUM),
                (destination.as_ptr(), FontWeight::NORMAL),
                (nested.as_ptr(), FontWeight::NORMAL),
                (move_to.as_ptr(), FontWeight::MEDIUM),
                (here.as_ptr(), FontWeight::NORMAL),
                (nested.as_ptr(), FontWeight::MEDIUM),
            ]
        );
    }
}
