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
    Animation, AnimationExt, AnyElement, App, Bounds, Context, DisplayId, FontWeight, Point,
    Render, Task, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div,
    prelude::*, px, size,
};
use objc2::{
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType};
use objc2_foundation::{NSNotificationCenter, NSObjectProtocol};
use vampir::{ControlHost, ControlState, Palette, color, ui_font};

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
const RADIUS: f32 = 10.0;
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

/// How long the panel takes to sweep open, and each row to settle in after
/// the one above it.
const SWEEP: Duration = Duration::from_millis(190);
const ROW_SWEEP: Duration = Duration::from_millis(170);
const STAGGER: Duration = Duration::from_millis(14);
/// Rows past this many arrive with the last staggered one, so a long menu
/// isn't still filling in when the pointer gets to it.
const STAGGERED_ROWS: usize = 8;

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
    answer: Sender<Option<usize>>,
    ns_window: usize,
    watch: Option<Watch>,
    done: bool,
    sweep: Sweep,
    /// The panel's full height, which it unrolls to.
    height: f32,
    /// The row under the pointer, whose icon takes the accent.
    hovered: Option<usize>,
    _dismiss: Option<Task<()>>,
    _settle: Option<Task<()>>,
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

/// The height `level` needs, with its way back if it's nested.
fn height_of(level: &Level) -> f32 {
    let rows: f32 = level
        .entries
        .iter()
        .map(|entry| match entry {
            MenuEntry::Separator => SEPARATOR_HEIGHT,
            _ => ROW_HEIGHT,
        })
        .sum();
    let back = if level.title.is_some() {
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
/// finds a label.
fn width_of_level(level: &Level, measure: impl Fn(&str) -> f32) -> f32 {
    let (icons, trailing) = columns(&level.entries);
    let label = level
        .entries
        .iter()
        .filter_map(|entry| match entry {
            MenuEntry::Item { label, .. } | MenuEntry::Submenu { label, .. } => {
                Some(measure(label))
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
    let back = level
        .title
        .as_deref()
        .map_or(0.0, |title| ICON_SLOT + GAP + measure(title));
    // A little over, so the longest label isn't flush against the edge.
    let slack = 12.0;
    (row.max(back) + 2.0 * (PADDING + BORDER + ROW_INSET) + slack).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// How wide `text` sets in the menu's font, from its glyphs' advances.
fn measure(cx: &App, text: &str) -> f32 {
    let system = cx.text_system();
    let font = system.resolve_font(&gpui::font(ui_font()));
    let size = px(TEXT_SIZE);
    text.chars()
        .map(|ch| {
            system
                .advance(font, size, ch)
                .map_or(TEXT_SIZE * 0.6, |advance| f32::from(advance.width))
        })
        .sum()
}

/// `row` settling into place, `order` rows after the first.
fn sweep_in<E: IntoElement + Styled + 'static>(row: E, order: usize, sweep: Sweep) -> AnyElement {
    let delay = STAGGER.as_secs_f32() * order.min(STAGGERED_ROWS) as f32;
    let travel = crate::slowed(ROW_SWEEP).as_secs_f32();
    let delay = crate::slowed(Duration::from_secs_f32(delay)).as_secs_f32();
    let total = delay + travel;
    let (dx, dy) = sweep.offset();
    row.with_animation(
        ("menu-row-in", order),
        Animation::new(Duration::from_secs_f32(total)),
        move |row, t| {
            let t = ease_out(((t * total - delay) / travel).clamp(0.0, 1.0));
            row.relative()
                .left(px(dx * (1.0 - t)))
                .top(px(dy * (1.0 - t)))
                .opacity(t)
        },
    )
    .into_any_element()
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

    /// Opens a correctly sized popup for the next menu level.
    fn show_level(
        &mut self,
        entries: Vec<MenuEntry>,
        base: usize,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.levels.push(Level {
            entries,
            base,
            title: Some(title),
        });
        self.reopen(Sweep::Forward, window, cx);
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.levels.len() > 1 {
            self.levels.pop();
            self.reopen(Sweep::Back, window, cx);
        }
    }

    fn reopen(&mut self, sweep: Sweep, window: &mut Window, cx: &mut Context<Self>) {
        let origin = window.bounds().origin;
        let display = window
            .display(cx)
            .map(|display| (display.id(), display.bounds()));
        let levels = self.levels.clone();
        let answer = self.answer.clone();
        let palette = self.palette;
        self.done = true;
        self.stop_watching();
        OPEN.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|open| open.ns_window == self.ns_window)
            {
                if let Some(open) = slot.take() {
                    open.dismissed.set(true);
                }
            }
        });
        hide(self.ns_window);
        window.remove_window();
        cx.defer(move |cx| open_level(cx, origin, display, levels, answer, palette, sweep));
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
        let tint = if hovered {
            palette.accent
        } else {
            palette.text_secondary
        };
        div()
            .id(("menu-row", key))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_INSET))
            .flex()
            .items_center()
            .gap(px(GAP))
            .rounded(px(6.0))
            .when(hovered, |el| el.bg(palette.row_hover))
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
    }
}

impl Render for Menu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let palette = self.palette;
        let sweep = self.sweep;
        let level = self.levels.last().cloned().unwrap();
        let columns = columns(&level.entries);
        let mut rule: gpui::Hsla = color::to_hsla(palette.field_border);
        rule.alpha = 0.8;
        let separator = move || {
            div()
                .flex_none()
                .h(px(1.0))
                .mx(px(ROW_INSET))
                .my(px((SEPARATOR_HEIGHT - 1.0) / 2.0))
                .bg(rule)
        };
        let mut rows = Vec::new();
        if let Some(title) = level.title {
            // The way back, headed with the row that led here.
            let back = self
                .row(
                    usize::MAX,
                    title,
                    Some(Icon::ChevronLeft),
                    (true, columns.1),
                    Trail::Nothing,
                    true,
                    cx,
                )
                .text_color(palette.text_secondary)
                .font_weight(FontWeight::MEDIUM)
                .on_click(cx.listener(|this, _, window, cx| this.back(window, cx)));
            rows.push(sweep_in(back, 0, sweep));
            rows.push(sweep_in(separator(), 1, sweep));
        }
        let mut next = level.base;
        for entry in level.entries {
            let index = next;
            next += width_of(&entry);
            let order = rows.len();
            let row = match entry {
                MenuEntry::Separator => sweep_in(separator(), order, sweep),
                MenuEntry::Item {
                    label,
                    enabled,
                    checked,
                    icon: glyph,
                } => {
                    let trail = if checked {
                        Trail::Check
                    } else {
                        Trail::Nothing
                    };
                    let row = self
                        .row(index, label, glyph, columns, trail, enabled, cx)
                        .when(enabled, |el| {
                            el.on_click(cx.listener(move |this, _, window, _| {
                                this.finish(Some(index), window)
                            }))
                        });
                    sweep_in(row, order, sweep)
                }
                MenuEntry::Submenu {
                    label,
                    entries,
                    icon: glyph,
                } => {
                    let title = label.clone();
                    let row = self
                        .row(index, label, glyph, columns, Trail::Chevron, true, cx)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show_level(entries.clone(), index + 1, title.clone(), window, cx);
                        }));
                    sweep_in(row, order, sweep)
                }
            };
            rows.push(row);
        }
        let height = self.height;
        let panel = div()
            .id("menu-panel")
            .flex_none()
            .w_full()
            .h(px(height))
            .overflow_hidden()
            .rounded(px(RADIUS))
            .border_1()
            .border_color(color::with_alpha(palette.field_border_strong, 0.55))
            .bg(palette.field_surface)
            .child(
                div()
                    .id("menu-rows")
                    .flex_none()
                    .w_full()
                    .h(px(height - 2.0 * BORDER))
                    .p(px(PADDING))
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .children(rows),
            );
        // Opening, the panel unrolls from the pointer's edge while its rows
        // settle in one after another; into or out of a submenu, it's
        // already there, and only the rows sweep across.
        let panel = if sweep.unrolls() {
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
        vampir::root(div().id("app-menu"), self, cx)
            .size_full()
            .flex()
            .flex_col()
            .when(sweep == Sweep::Up, |el| el.justify_end())
            .font_family(ui_font())
            .text_size(px(TEXT_SIZE))
            .text_color(palette.text_primary)
            .on_action(
                cx.listener(|this, _: &vampir::keyboard::Dismiss, window, cx| {
                    if this.levels.len() > 1 {
                        this.back(window, cx);
                    } else {
                        this.finish(None, window);
                    }
                }),
            )
            .child(panel)
    }
}

pub(crate) fn open(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    entries: Vec<MenuEntry>,
    palette: Palette,
) -> Receiver<Option<usize>> {
    open_at(cx, window, position, entries, palette, Sweep::Down)
}

/// Place a shelf menu with its bottom edge at the trigger, above the shelf.
pub(crate) fn open_above(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    entries: Vec<MenuEntry>,
    palette: Palette,
) -> Receiver<Option<usize>> {
    open_at(cx, window, position, entries, palette, Sweep::Up)
}

fn open_at(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    entries: Vec<MenuEntry>,
    palette: Palette,
    sweep: Sweep,
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
        sweep,
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
    sweep: Sweep,
) {
    let Some(level) = levels.last() else {
        return;
    };
    let height = height_of(level);
    let width = width_of_level(level, |text| measure(cx, text));
    if sweep == Sweep::Up {
        origin.y -= px(height);
    }
    if let Some((_, screen)) = display {
        origin.x = origin
            .x
            .min(screen.origin.x + screen.size.width - px(width))
            .max(screen.origin.x);
        origin.y = origin
            .y
            .min(screen.origin.y + screen.size.height - px(height))
            .max(screen.origin.y);
    }
    let options = WindowOptions {
        display_id: display.map(|(id, _)| id),
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            origin,
            size(px(width), px(height)),
        ))),
        // Above everything, and it doesn't take the keyboard or make the
        // window it's for look inactive.
        kind: WindowKind::PopUp,
        focus: false,
        // Clear around the panel's rounded corners, and while it unrolls.
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
        let menu = cx.new(|_| Menu {
            controls: ControlState::default(),
            palette,
            levels,
            answer,
            ns_window,
            watch: Some(watch),
            done: false,
            sweep,
            height,
            hovered: None,
            _dismiss: None,
            _settle: None,
        });
        menu.update(cx, |menu, cx| {
            menu._dismiss = Some(cx.spawn_in(window, async move |this, cx| {
                while let Ok(signal) = signals.recv().await {
                    let closing = matches!(signal, Signal::Close);
                    let _ = this.update_in(cx, |menu, window, cx| match signal {
                        Signal::Close => menu.finish(None, window),
                        Signal::Back => menu.back(window, cx),
                    });
                    if closing {
                        break;
                    }
                }
            }));
            // The window's shadow is cast from what's drawn in it, so it's
            // only right once the panel has finished coming in.
            let settled = crate::slowed(
                SWEEP.max(ROW_SWEEP + STAGGER * STAGGERED_ROWS as u32) + Duration::from_millis(20),
            );
            menu._settle = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(settled).await;
                let _ = this.update(cx, |menu, _| {
                    if !menu.done {
                        recast_shadow(menu.ns_window);
                    }
                });
            }));
        });
        menu
    });
    if let Ok(handle) = result {
        let _ = handle.update(cx, |_, window, _| {
            let (ns_window, _) = crate::ns_window_of(window);
            if ns_window == 0 {
                return;
            }
            // SAFETY: the native window belongs to this live GPUI window.
            let ns_window = unsafe { &*(ns_window as *const objc2_app_kit::NSWindow) };
            ns_window.setHasShadow(true);
            ns_window.orderFront(None);
        });
    }
}

/// Has the menu window's shadow follow its panel as drawn now.
fn recast_shadow(ns_window: usize) {
    if ns_window == 0 {
        return;
    }
    // SAFETY: the menu's own window, still open: `Menu::done` is set before
    // it is closed.
    let window = unsafe { &*(ns_window as *const objc2_app_kit::NSWindow) };
    window.invalidateShadow();
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
        let by_char = |text: &str| text.chars().count() as f32 * 7.0;
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
}
