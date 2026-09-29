//! Dragging tabs: along the strip to reorder them, out of it to tear the
//! tab off into a window of its own that follows the pointer, and onto
//! another window's tabs to move it there. A window's only tab carries the
//! whole window. Pages move live, with their history and state: the same
//! WebKit view changes windows.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    WeakEntity, canvas, prelude::*, px,
};
use objc2_app_kit::{NSEvent, NSWindow};
use objc2_foundation::NSPoint;
use wry::WebViewExtMacOS;

use crate::{
    Browser, BrowserTab, TAB_STRIP_HEIGHT, TOOLBAR_HEIGHT, TRAFFIC_LIGHT_INSET, open_browser_window,
};

/// How far the pointer moves before a press on a tab becomes a drag.
const DRAG_THRESHOLD: f32 = 5.0;
/// How far past the tabs the pointer can stray before the tab tears off.
const STRIP_SLACK: f32 = 24.0;

pub(crate) type TabBounds = Rc<RefCell<HashMap<u64, Bounds<Pixels>>>>;

pub(crate) struct TabDrag {
    id: u64,
    /// The tab's title, for its ghost in a window it would join.
    title: String,
    start: Point<Pixels>,
    active: bool,
    /// The window following the pointer, once the tab has left the strip.
    carrier: Option<Carrier>,
}

#[derive(Clone)]
struct Carrier {
    browser: WeakEntity<Browser>,
    ns_window: usize,
    /// Where the pointer holds the window, from its top-left, in points.
    grab: (f64, f64),
}

/// Records where a tab was drawn, for hit-testing drags against.
pub(crate) fn record_bounds(bounds: TabBounds, id: u64) -> impl IntoElement {
    canvas(
        move |area, _, _| {
            bounds.borrow_mut().insert(id, area);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Puts the top-left of `ns_window` where the pointer is, less `grab`.
fn follow_pointer(ns_window: usize, grab: (f64, f64)) {
    if ns_window == 0 {
        return;
    }
    let mouse = NSEvent::mouseLocation();
    // SAFETY: a live browser window's address: callers check that the
    // carrier's browser, and so its window, is still there.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    window.setFrameTopLeftPoint(NSPoint::new(mouse.x - grab.0, mouse.y + grab.1));
    // Above the window the drag started in, which keeps the pointer.
    window.orderFrontRegardless();
}

/// Fades `ns_window` while it hovers where it would join another.
fn set_alpha(ns_window: usize, alpha: f64) {
    if ns_window == 0 {
        return;
    }
    // SAFETY: as above.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    if (window.alphaValue() - alpha).abs() > 0.01 {
        window.setAlphaValue(alpha);
    }
}

/// Brings `ns_window` forward and gives it the keyboard.
fn bring_forward(ns_window: usize) {
    if ns_window == 0 {
        return;
    }
    // SAFETY: as above.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    window.makeKeyAndOrderFront(None);
}

/// The pointer in `ns_window`'s own coordinates, top-left origin, in points.
pub(crate) fn pointer_in(ns_window: usize) -> Option<Point<Pixels>> {
    if ns_window == 0 {
        return None;
    }
    let mouse = NSEvent::mouseLocation();
    // SAFETY: as above.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    let frame = window.frame();
    let inside = mouse.x >= frame.origin.x
        && mouse.x <= frame.origin.x + frame.size.width
        && mouse.y >= frame.origin.y
        && mouse.y <= frame.origin.y + frame.size.height;
    inside.then(|| {
        Point::new(
            px((mouse.x - frame.origin.x) as f32),
            px((frame.origin.y + frame.size.height - mouse.y) as f32),
        )
    })
}

impl Browser {
    /// A press on tab `id`; a drag, if the pointer moves far enough.
    pub(crate) fn begin_tab_drag(&mut self, id: u64, at: Point<Pixels>) {
        let title = self
            .index_of(id)
            .map(|i| self.tabs[i].title.clone())
            .unwrap_or_default();
        self.tab_drag = Some(TabDrag {
            id,
            title,
            start: at,
            active: false,
            carrier: None,
        });
    }

    /// Where a tab dropped at `at` (window coordinates) would go among this
    /// window's tabs, or `None` if `at` is away from them. `skip` is a tab
    /// that doesn't count, being the one dragged.
    fn drop_index(&self, at: Point<Pixels>, skip: Option<u64>) -> Option<usize> {
        let bounds = self.tab_bounds.borrow();
        let tabs: Vec<(u64, Bounds<Pixels>)> = self
            .tabs
            .iter()
            .filter(|t| Some(t.id) != skip)
            .filter_map(|t| bounds.get(&t.id).map(|b| (t.id, *b)))
            .collect();
        let slack = px(STRIP_SLACK);
        let vertical = self.vertical_tabs;
        if tabs.is_empty() {
            // Only the dragged tab here: anywhere in the tab area will do.
            let own = skip.and_then(|id| bounds.get(&id).copied())?;
            let near = if vertical {
                at.x <= own.origin.x + own.size.width + slack
            } else {
                at.y >= own.origin.y - slack && at.y <= own.origin.y + own.size.height + slack
            };
            return near.then_some(0);
        }
        let near = if vertical {
            let right = tabs
                .iter()
                .map(|(_, b)| b.origin.x + b.size.width)
                .fold(px(0.0), |a, b| a.max(b));
            at.x <= right + slack
        } else {
            let top = tabs
                .iter()
                .map(|(_, b)| b.origin.y)
                .fold(px(f32::MAX), |a, b| a.min(b));
            let bottom = tabs
                .iter()
                .map(|(_, b)| b.origin.y + b.size.height)
                .fold(px(0.0), |a, b| a.max(b));
            at.y >= top - slack && at.y <= bottom + slack
        };
        if !near {
            return None;
        }
        Some(
            tabs.iter()
                .filter(|(_, b)| {
                    if vertical {
                        b.origin.y + b.size.height / 2.0 < at.y
                    } else {
                        b.origin.x + b.size.width / 2.0 < at.x
                    }
                })
                .count(),
        )
    }

    pub(crate) fn drag_moved(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.tab_drag else {
            return;
        };
        if !drag.active {
            let moved = (at.x - drag.start.x).abs().max((at.y - drag.start.y).abs());
            if moved < px(DRAG_THRESHOLD) {
                return;
            }
            drag.active = true;
        }
        if let Some(carrier) = drag.carrier.clone() {
            // Its window closed under the drag (an extension closed the
            // tab): nothing left to carry, nor to touch.
            if carrier.browser.upgrade().is_none() {
                self.tab_drag = None;
                self.hint_drop(None, cx);
                return;
            }
            follow_pointer(carrier.ns_window, carrier.grab);
            let title = std::mem::take(&mut drag.title);
            let joining = self.hint_drop(Some((&carrier, &title)), cx);
            if let Some(drag) = &mut self.tab_drag {
                drag.title = title;
            }
            // Over a window it would join, the carried window steps aside
            // for its tab's ghost there.
            set_alpha(carrier.ns_window, if joining { 0.0 } else { 1.0 });
            return;
        }
        let id = drag.id;
        let start = drag.start;
        let Some(from) = self.index_of(id) else {
            self.tab_drag = None;
            return;
        };
        // The compact window's titlebar carries its whole window. Keep
        // the original grab point so crossing the drag threshold does not
        // make the window jump under the pointer.
        if self.compact {
            let grab = (f64::from(f32::from(start.x)), f64::from(f32::from(start.y)));
            if let Some(drag) = &mut self.tab_drag {
                drag.carrier = Some(Carrier {
                    browser: cx.weak_entity(),
                    ns_window: self.ns_window,
                    grab,
                });
            }
            follow_pointer(self.ns_window, grab);
            return;
        }
        // Along the strip: the tab moves among the others as it goes.
        if let Some(to) = self.drop_index(at, Some(id)) {
            if to != from {
                let selected = self.tabs.get(self.selected).map(|t| t.id);
                let tab = self.tabs.remove(from);
                self.tabs.insert(to, tab);
                if let Some(index) = selected.and_then(|s| self.index_of(s)) {
                    self.selected = index;
                }
                cx.notify();
            }
            return;
        }
        // Out of it. A window's only tab takes the window along.
        if self.tabs.len() <= 1 {
            let grab = (f64::from(f32::from(at.x)), f64::from(f32::from(at.y)));
            if let Some(drag) = &mut self.tab_drag {
                drag.carrier = Some(Carrier {
                    browser: cx.weak_entity(),
                    ns_window: self.ns_window,
                    grab,
                });
            }
            return;
        }
        // Otherwise it tears off into a window of its own, held where a
        // tab sits in a fresh window.
        let tab = self.take_tab(from, cx);
        let grab = if self.vertical_tabs {
            (70.0, f64::from(TOOLBAR_HEIGHT) + 24.0)
        } else {
            (
                f64::from(TRAFFIC_LIGHT_INSET) + 60.0,
                f64::from(TOOLBAR_HEIGHT + TAB_STRIP_HEIGHT / 2.0),
            )
        };
        let common = self.common.clone();
        let private = self.private;
        let me = cx.weak_entity();
        cx.defer(move |cx| {
            let Some(handle) = open_browser_window(cx, common, private, None, false, Some(tab))
            else {
                return;
            };
            let Ok(entity) = handle.entity(cx) else {
                return;
            };
            let ns_window = entity.read(cx).ns_window;
            follow_pointer(ns_window, grab);
            let carrier = Carrier {
                browser: entity.downgrade(),
                ns_window,
                grab,
            };
            let _ = me.update(cx, |browser, _| {
                if let Some(drag) = &mut browser.tab_drag {
                    drag.carrier = Some(carrier);
                }
            });
        });
    }

    pub(crate) fn drag_ended(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.tab_drag.take() else {
            return;
        };
        if !drag.active {
            return;
        }
        self.persist();
        cx.notify();
        self.hint_drop(None, cx);
        let Some(carrier) = drag.carrier else {
            return;
        };
        if carrier.browser.upgrade().is_none() {
            return;
        }
        set_alpha(carrier.ns_window, 1.0);
        let id = drag.id;
        let common = self.common.clone();
        // Onto another window's tabs: the tab moves in there.
        cx.defer(move |cx| {
            let Some(source) = carrier.browser.upgrade() else {
                return;
            };
            let private = source.read(cx).private;
            let target = common.browsers().into_iter().find_map(|browser| {
                if browser.entity_id() == source.entity_id() {
                    return None;
                }
                let index = browser.read(cx).landing(private)?;
                Some((browser, index))
            });
            let Some((target, index)) = target else {
                // Left where it was dropped, as a window of its own.
                bring_forward(carrier.ns_window);
                return;
            };
            bring_forward(target.read(cx).ns_window);
            let tab = source.update(cx, |browser, cx| {
                let from = browser.index_of(id)?;
                Some(browser.take_tab(from, cx))
            });
            if let Some(tab) = tab {
                target.update(cx, |browser, cx| browser.adopt_tab(tab, index, cx));
            }
        });
    }

    /// Marks the window a carried tab would join if dropped now, with where
    /// in its tabs; `None` clears every mark.
    fn hint_drop(&mut self, carried: Option<(&Carrier, &str)>, cx: &mut Context<Self>) -> bool {
        let me = cx.entity_id();
        let Some((carrier, title)) = carried else {
            self.set_drop_hint(None, cx);
            self.for_other_windows(cx, |browser, cx| browser.set_drop_hint(None, cx));
            return false;
        };
        let private = match carrier.browser.upgrade() {
            Some(b) if b.entity_id() == me => self.private,
            Some(b) => b.read(cx).private,
            None => return false,
        };
        let mut found = false;
        for browser in self.common.browsers() {
            let id = browser.entity_id();
            if id == carrier.browser.entity_id() {
                continue;
            }
            let hint = if found {
                None
            } else if id == me {
                self.landing(private)
            } else {
                browser.read(cx).landing(private)
            }
            .map(|index| (index, title.to_owned()));
            found |= hint.is_some();
            if id == me {
                self.set_drop_hint(hint, cx);
            } else {
                browser.update(cx, |b, cx| b.set_drop_hint(hint, cx));
            }
        }
        found
    }

    /// Where among this window's tabs a carried tab from a window that is
    /// (or isn't) private would land, with the pointer where it is now.
    fn landing(&self, private: bool) -> Option<usize> {
        // Private tabs stay out of ordinary windows, and the other way round.
        if self.private != private || self.compact {
            return None;
        }
        self.drop_index(pointer_in(self.ns_window)?, None)
    }

    fn set_drop_hint(&mut self, hint: Option<(usize, String)>, cx: &mut Context<Self>) {
        if self.drop_hint != hint {
            self.drop_hint = hint;
            cx.notify();
        }
    }

    /// Where a carried tab would land: the tab area lit, and a bar where it
    /// would go.
    pub(crate) fn drop_highlight(&self, palette: vampir::Palette) -> Option<AnyElement> {
        let accent = palette.accent;
        let (index, title) = self.drop_hint.clone()?;
        let bounds = self.tab_bounds.borrow();
        let tabs: Vec<Bounds<Pixels>> = self
            .tabs
            .iter()
            .filter_map(|t| bounds.get(&t.id).copied())
            .collect();
        let first = *tabs.first()?;
        let last = *tabs.last()?;
        let pad = px(4.0);
        let wash = vampir::color::with_alpha(accent, 0.18);
        let edge = accent;
        let (area, bar) = if self.vertical_tabs {
            let bottom = last.origin.y + last.size.height;
            let y = match tabs.get(index) {
                Some(tab) => tab.origin.y - px(3.0),
                None => bottom + px(1.0),
            };
            (
                Bounds::new(
                    Point::new(first.origin.x - pad, first.origin.y - pad),
                    gpui::size(
                        first.size.width + pad * 2.0,
                        bottom - first.origin.y + pad * 2.0,
                    ),
                ),
                Bounds::new(
                    Point::new(first.origin.x, y),
                    gpui::size(first.size.width, px(3.0)),
                ),
            )
        } else {
            let right = last.origin.x + last.size.width;
            let x = match tabs.get(index) {
                Some(tab) => tab.origin.x - px(4.0),
                None => right + px(1.0),
            };
            (
                Bounds::new(
                    Point::new(first.origin.x - pad, first.origin.y - pad),
                    gpui::size(
                        right - first.origin.x + pad * 2.0,
                        first.size.height + pad * 2.0,
                    ),
                ),
                Bounds::new(
                    Point::new(x, first.origin.y),
                    gpui::size(px(3.0), first.size.height),
                ),
            )
        };
        Some(
            gpui::div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(
                    gpui::div()
                        .absolute()
                        .left(area.origin.x)
                        .top(area.origin.y)
                        .w(area.size.width)
                        .h(area.size.height)
                        .rounded(px(10.0))
                        .bg(wash)
                        .border_2()
                        .border_color(edge),
                )
                .child(
                    gpui::div()
                        .absolute()
                        .left(bar.origin.x)
                        .top(bar.origin.y)
                        .w(bar.size.width)
                        .h(bar.size.height)
                        .rounded(px(2.0))
                        .bg(accent),
                )
                // The tab itself, where it will land.
                .child({
                    let (left, top, width, height) = if self.vertical_tabs {
                        (
                            first.origin.x + px(10.0),
                            bar.origin.y - first.size.height / 2.0,
                            first.size.width - px(20.0),
                            first.size.height,
                        )
                    } else {
                        (
                            bar.origin.x + px(6.0),
                            first.origin.y,
                            px(170.0),
                            first.size.height,
                        )
                    };
                    gpui::div()
                        .absolute()
                        .left(left)
                        .top(top)
                        .w(width)
                        .h(height)
                        .flex()
                        .items_center()
                        .px(px(10.0))
                        .rounded(px(7.0))
                        .bg(crate::Chrome::new(palette).raised)
                        .border_1()
                        .border_color(accent)
                        .shadow(vampir::lighting::raised(palette.is_dark))
                        .opacity(0.92)
                        .text_size(px(12.5))
                        .text_color(palette.text_primary)
                        .child(gpui::div().min_w(px(0.0)).truncate().child(title))
                })
                .into_any_element(),
        )
    }

    /// Takes tab `index` out of this window, page and all, for another
    /// window to adopt. A window left without tabs closes when next drawn.
    fn take_tab(&mut self, index: usize, cx: &mut Context<Self>) -> BrowserTab {
        let selected = self.tabs.get(self.selected).map(|t| t.id);
        let tab = self.tabs.remove(index);
        self.tab_bounds.borrow_mut().remove(&tab.id);
        if let Some(view) = &tab.view {
            let _ = view.set_visible(false);
        }
        if self.tabs.is_empty() {
            self.leave_windows(cx);
        } else {
            let next = selected
                .filter(|&s| s != tab.id)
                .and_then(|s| self.index_of(s))
                .unwrap_or(index.min(self.tabs.len() - 1));
            // Selecting saves the session and redraws.
            self.select(next, cx);
            return tab;
        }
        self.persist();
        cx.notify();
        tab
    }

    /// Takes in a tab from another window, at `index`, and shows it.
    pub(crate) fn adopt_tab(&mut self, tab: BrowserTab, index: usize, cx: &mut Context<Self>) {
        if let Some(view) = &tab.view {
            let _ = view.reparent(self.ns_window as *mut NSWindow);
            let _ = view.zoom(tab.zoom);
        }
        self.route(tab.id).set(self.sender.clone());
        let index = index.min(self.tabs.len());
        self.tabs.insert(index, tab);
        // Selecting saves the session and redraws.
        self.select(index, cx);
    }

    /// While a tab is pressed, follows the pointer wherever it goes, in or
    /// out of the window, until the button comes up.
    pub(crate) fn drag_tracker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.tab_drag.as_ref()?;
        let me = cx.weak_entity();
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let moved = me.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            let _ = moved
                                .update(cx, |browser, cx| browser.drag_moved(event.position, cx));
                        }
                    });
                    let ended = me.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            let _ = ended.update(cx, |browser, cx| browser.drag_ended(cx));
                        }
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }
}
