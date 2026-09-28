//! Browser-owned context menus in a Vampir popup above WebKit.
//!
//! It behaves like a native menu: it floats above the window without
//! taking the keyboard from it (the field or page being typed in keeps its
//! caret), one is open at a time, and a click anywhere else in the app, a
//! key, or switching to another app closes it; that click does nothing
//! else.

use std::{cell::Cell, cell::RefCell, ptr::NonNull, rc::Rc};

use async_channel::{Receiver, Sender};
use block2::RcBlock;
use gpui::{
    App, Bounds, Context, DisplayId, Point, Render, Task, Window, WindowBounds, WindowKind,
    WindowOptions, div, prelude::*, px, size,
};
use objc2::{
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{NSColor, NSEvent, NSEventMask, NSEventType};
use objc2_foundation::{NSNotificationCenter, NSObjectProtocol};
use vampir::{ControlHost, ControlState, Palette, ui_font};

use crate::native::MenuEntry;

const WIDTH: f32 = 230.0;
const ROW_HEIGHT: f32 = 28.0;
/// A separator's hairline and the room either side of it.
const SEPARATOR_HEIGHT: f32 = 9.0;
const PADDING: f32 = 5.0;
/// Taller menus scroll.
const MAX_HEIGHT: f32 = 500.0;

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
    // Each level stores the first index after its parent row. Native menu
    // numbering included submenu rows and separators, so keep that scheme.
    levels: Vec<(Vec<MenuEntry>, usize)>,
    answer: Sender<Option<usize>>,
    ns_window: usize,
    watch: Option<Watch>,
    done: bool,
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

/// The height a level of `entries` needs, with a Back row if it's nested.
fn height_of(entries: &[MenuEntry], nested: bool) -> f32 {
    let rows: f32 = entries
        .iter()
        .map(|entry| match entry {
            MenuEntry::Separator => SEPARATOR_HEIGHT,
            _ => ROW_HEIGHT,
        })
        .sum();
    let back = if nested { ROW_HEIGHT } else { 0.0 };
    (rows + back + 2.0 * PADDING).min(MAX_HEIGHT)
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.levels.push((entries, base));
        self.reopen(window, cx);
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.levels.len() > 1 {
            self.levels.pop();
            self.reopen(window, cx);
        }
    }

    fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        cx.defer(move |cx| open_level(cx, origin, display, levels, answer, palette));
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
        let (entries, base) = self.levels.last().cloned().unwrap();
        let mut next = base;
        let mut rows = Vec::new();
        let mut rule: gpui::Hsla = vampir::color::to_hsla(palette.field_border);
        rule.alpha = 0.8;
        for entry in entries {
            let index = next;
            next += width_of(&entry);
            match entry {
                MenuEntry::Separator => rows.push(
                    div()
                        .flex_none()
                        .w_full()
                        .h(px(1.0))
                        .my(px((SEPARATOR_HEIGHT - 1.0) / 2.0))
                        .bg(rule)
                        .into_any_element(),
                ),
                MenuEntry::Item {
                    label,
                    enabled,
                    checked,
                } => {
                    rows.push(
                        div()
                            .id(("menu-item", index))
                            .flex_none()
                            .h(px(ROW_HEIGHT))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .rounded(px(5.0))
                            .when(enabled, |el| {
                                el.cursor_pointer()
                                    .hover(move |style| style.bg(palette.soft_fill))
                            })
                            .when(!enabled, |el| el.opacity(0.45))
                            .when(enabled, |el| {
                                el.on_click(cx.listener(move |this, _, window, _| {
                                    this.finish(Some(index), window)
                                }))
                            })
                            .child(div().w(px(14.0)).child(if checked { "✓" } else { "" }))
                            .child(div().flex_1().min_w(px(0.0)).truncate().child(label))
                            .into_any_element(),
                    );
                }
                MenuEntry::Submenu { label, entries } => {
                    rows.push(
                        div()
                            .id(("menu-submenu", index))
                            .flex_none()
                            .h(px(ROW_HEIGHT))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .hover(move |style| style.bg(palette.soft_fill))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_level(entries.clone(), index + 1, window, cx);
                            }))
                            .child(div().w(px(14.0)))
                            .child(div().flex_1().min_w(px(0.0)).truncate().child(label))
                            .child("›")
                            .into_any_element(),
                    );
                }
            }
        }
        vampir::root(div().id("app-menu"), self, cx)
            .size_full()
            .p(px(PADDING))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .font_family(ui_font())
            .text_size(px(12.0))
            .text_color(palette.text_primary)
            .bg(palette.field_surface)
            .on_action(
                cx.listener(|this, _: &vampir::keyboard::Dismiss, window, cx| {
                    if this.levels.len() > 1 {
                        this.back(window, cx);
                    } else {
                        this.finish(None, window);
                    }
                }),
            )
            .when(self.levels.len() > 1, |el| {
                el.child(
                    div()
                        .id("menu-back")
                        .flex_none()
                        .h(px(ROW_HEIGHT))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .rounded(px(5.0))
                        .cursor_pointer()
                        .hover(move |style| style.bg(palette.soft_fill))
                        .child("‹  Back")
                        .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
                )
            })
            .children(rows)
    }
}

pub(crate) fn open(
    cx: &mut App,
    window: &Window,
    position: Point<gpui::Pixels>,
    entries: Vec<MenuEntry>,
    palette: Palette,
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
        vec![(entries, 0)],
        sender,
        palette,
    );
    receiver
}

fn open_level(
    cx: &mut App,
    mut origin: Point<gpui::Pixels>,
    display: Option<(DisplayId, Bounds<gpui::Pixels>)>,
    levels: Vec<(Vec<MenuEntry>, usize)>,
    answer: Sender<Option<usize>>,
    palette: Palette,
) {
    let height = levels.last().map_or(2.0 * PADDING, |(entries, _)| {
        height_of(entries, levels.len() > 1)
    });
    if let Some((_, screen)) = display {
        origin.x = origin
            .x
            .min(screen.origin.x + screen.size.width - px(WIDTH))
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
            size(px(WIDTH), px(height)),
        ))),
        // Above everything, and it doesn't take the keyboard or make the
        // window it's for look inactive.
        kind: WindowKind::PopUp,
        focus: false,
        // GPUI's macOS popup backing starts out black. Keep it hidden until
        // its first draw has filled the window with the menu surface.
        show: false,
        titlebar: None,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    let surface = vampir::color::channels(palette.field_surface);
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
            _dismiss: None,
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
            let background = NSColor::colorWithSRGBRed_green_blue_alpha(
                f64::from(surface[0]),
                f64::from(surface[1]),
                f64::from(surface[2]),
                f64::from(surface[3]),
            );
            ns_window.setBackgroundColor(Some(&background));
            ns_window.orderFront(None);
        });
    }
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
            },
            MenuEntry::item("D"),
        ];
        assert_eq!(entries.iter().map(width_of).sum::<usize>(), 5);
    }

    #[test]
    fn menus_are_as_tall_as_their_rows() {
        let entries = [
            MenuEntry::item("A"),
            MenuEntry::Separator,
            MenuEntry::item("B"),
        ];
        assert_eq!(
            height_of(&entries, false),
            2.0 * ROW_HEIGHT + SEPARATOR_HEIGHT + 2.0 * PADDING
        );
        assert_eq!(
            height_of(&entries, true),
            height_of(&entries, false) + ROW_HEIGHT
        );
    }
}
