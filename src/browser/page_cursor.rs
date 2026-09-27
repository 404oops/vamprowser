//! Lets WebKit set the pointer over a page: the hand over links, the
//! I-beam over text, a page's own cursors.
//!
//! GPUI keeps one cursor rect over the whole of its view, pages included,
//! and WebKit leaves the cursor alone while a cursor rect is in force, so
//! every page showed GPUI's arrow. GPUI's view now puts up no cursor rect
//! while the pointer is over a page, and has its rects remade each time the
//! pointer crosses between a page and the browser around it.

use std::{cell::Cell, ffi::c_char, ptr::NonNull, sync::OnceLock};

use block2::RcBlock;
use objc2::{
    ffi,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};
use objc2_app_kit::{NSEvent, NSEventMask, NSView, NSWindow};

/// GPUI's own `resetCursorRects`, for everywhere but over a page.
static GPUI_RESET: OnceLock<Imp> = OnceLock::new();

type Reset = unsafe extern "C-unwind" fn(&AnyObject, Sel);

/// Whether the pointer is over a page in `window`.
fn over_page(window: &NSWindow) -> bool {
    let Some(content) = window.contentView() else {
        return false;
    };
    let location = window.mouseLocationOutsideOfEventStream();
    content
        .hitTest(content.convertPoint_fromView(location, None))
        .is_some_and(crate::in_web_view)
}

unsafe extern "C-unwind" fn reset_cursor_rects(this: &AnyObject, cmd: Sel) {
    // SAFETY: only ever installed on GPUI's NSView subclass.
    let view = unsafe { &*(this as *const AnyObject as *const NSView) };
    if view.window().is_some_and(|window| over_page(&window)) {
        // No rect: the page's cursor stands.
        return;
    }
    if let Some(imp) = GPUI_RESET.get() {
        // SAFETY: GPUI's method, of this very signature.
        unsafe { std::mem::transmute::<Imp, Reset>(*imp)(this, cmd) };
    }
}

/// Puts [`reset_cursor_rects`] in place of GPUI's, once, given GPUI's view.
fn install(ns_view: usize) {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ns_view == 0 || DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    // SAFETY: GPUI's live view.
    let view = unsafe { &*(ns_view as *const AnyObject) };
    let class: &AnyClass = view.class();
    let selector = sel!(resetCursorRects);
    let Some(method) = class.instance_method(selector) else {
        return;
    };
    // SAFETY: the replacement has the method's signature (an object and the
    // selector, returning nothing), with the original's type encoding.
    unsafe {
        let types: *const c_char = ffi::method_getTypeEncoding(method);
        let imp: Imp = std::mem::transmute::<Reset, Imp>(reset_cursor_rects);
        let previous =
            ffi::class_replaceMethod(class as *const AnyClass as *mut AnyClass, selector, imp, types);
        if let Some(previous) = previous {
            let _ = GPUI_RESET.set(previous);
        }
    }
}

/// Has GPUI's view in `ns_window` remake its cursor rects whenever the
/// pointer moves onto a page or off one.
pub fn watch(ns_window: usize, ns_view: usize) -> Option<Retained<AnyObject>> {
    if ns_window == 0 || ns_view == 0 {
        return None;
    }
    install(ns_view);
    let was_over = Cell::new(false);
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit owns the event for the callback's duration.
        let moved = unsafe { event.as_ref() };
        if !crate::event_in(moved, ns_window) {
            return event.as_ptr();
        }
        // SAFETY: the browser's own window and GPUI's view in it, both alive
        // while the browser, which removes this monitor, is.
        let (window, view) = unsafe {
            (
                &*(ns_window as *const NSWindow),
                &*(ns_view as *const NSView),
            )
        };
        let over = over_page(window);
        if over != was_over.replace(over) {
            window.invalidateCursorRectsForView(view);
        }
        event.as_ptr()
    });
    // SAFETY: the block returns the live event, as AppKit requires.
    unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::MouseMoved | NSEventMask::MouseEntered | NSEventMask::MouseExited,
            &handler,
        )
    }
}
