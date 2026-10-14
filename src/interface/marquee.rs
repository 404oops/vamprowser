//! Labels too long for their place: cut off at rest, and on hover they
//! glide along to show the rest, pause, and come back, as the iOS lock
//! screen does with a song title. In place of tooltips.

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{AnyElement, Font, Pixels, SharedString, TextRun, Window, canvas, div, prelude::*, px};

/// Speed of the glide, in points a second.
const SPEED: f32 = 42.0;
/// Pauses at the start and at the end.
const HOLD_START: f32 = 0.9;
const HOLD_END: f32 = 1.1;
/// How long the glide back to the start takes.
const RETURN: f32 = 0.45;

/// Eased at both ends.
fn smooth(p: f32) -> f32 {
    p * p * (3.0 - 2.0 * p)
}

/// A hovered label's shape survives animation frames and width changes.
/// Text or font changes invalidate it; resizing only changes the overflow.
pub(crate) struct Measurement {
    text: SharedString,
    font: Font,
    font_size: Pixels,
    letter_spacing: Option<Pixels>,
    text_width: f32,
    available_width: f32,
    overflow: f32,
    /// When the glide began: the label's motion is read off the clock, so
    /// it needs frames only while moving, not through its pauses.
    started: Instant,
}

/// Where a gliding label is, `elapsed` into its cycle over `overflow`:
/// how far left it's drawn, and whether it's moving (needs a frame now)
/// or how long until it next does.
fn position(elapsed: f32, overflow: f32) -> (f32, Option<Duration>) {
    let travel = overflow + 6.0;
    let glide = travel / SPEED;
    let total = HOLD_START + glide + HOLD_END + RETURN;
    let at = elapsed % total;
    let until = |next: f32| Some(Duration::from_secs_f32((next - at).max(0.0)));
    if at < HOLD_START {
        (0.0, until(HOLD_START))
    } else if at < HOLD_START + glide {
        let p = (at - HOLD_START) / glide;
        // Eased at both ends, like a hand scrolling.
        (travel * smooth(p), None)
    } else if at < HOLD_START + glide + HOLD_END {
        (travel, until(HOLD_START + glide + HOLD_END))
    } else {
        // Back to the start, briskly but not in a jump.
        let p = (at - HOLD_START - glide - HOLD_END) / RETURN;
        (travel * (1.0 - smooth(p.min(1.0))), None)
    }
}

/// When the labels gliding in `widths` next need drawing: `Some(zero)` if
/// one is moving now, else how long until one moves again; `None` with
/// nothing gliding. The host asks after building its frame, and requests
/// the next frame or a redraw then, as it does for its other motion.
pub(crate) fn next_wake(widths: &Widths) -> Option<Duration> {
    widths
        .borrow()
        .values()
        .filter(|entry| entry.overflow > 1.0)
        .map(|entry| {
            position(entry.started.elapsed().as_secs_f32(), entry.overflow)
                .1
                .unwrap_or_default()
        })
        .min()
}

pub(crate) type Widths = Rc<RefCell<HashMap<u64, Measurement>>>;

/// Keys for bookmarks, apart from tab ids.
pub(crate) fn bookmark_key(id: u64) -> u64 {
    (1 << 40) | id
}

/// How wide `text` is in the text style it's drawn in here.
fn text_width(
    window: &Window,
    text: SharedString,
    font: Font,
    size: Pixels,
    letter_spacing: Option<Pixels>,
) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let run = TextRun {
        len: text.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing,
    };
    f32::from(
        window
            .text_system()
            .shape_line(text, size, &[run], None)
            .width,
    )
}

/// A label that fills its place, cut off at rest and gliding while
/// `hovered` if it doesn't fit. `key` names its place in `widths`.
pub(crate) fn label(
    text: impl Into<SharedString>,
    key: u64,
    hovered: bool,
    widths: &Widths,
) -> AnyElement {
    let text: SharedString = text.into();
    let container = div().relative().min_w(px(0.0)).flex_1().overflow_hidden();
    if !hovered {
        widths.borrow_mut().remove(&key);
        return container
            .child(div().truncate().child(text))
            .into_any_element();
    }
    let record = widths.clone();
    let measured = text.clone();
    // Measured where it's drawn, in the font and size it's drawn in; only
    // while hovered, as shaping text costs.
    let measure = canvas(
        move |bounds, window, _| {
            let style = window.text_style();
            let font = style.font();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let letter_spacing = style.letter_spacing;
            let mut record = record.borrow_mut();
            let previous = record.get(&key);
            let matching = previous.filter(|entry| {
                entry.text == measured
                    && entry.font == font
                    && entry.font_size == font_size
                    && entry.letter_spacing == letter_spacing
            });
            let available_width = f32::from(bounds.size.width);
            if matching.is_some_and(|entry| entry.available_width == available_width) {
                return;
            }
            let text_width = matching.map(|entry| entry.text_width).unwrap_or_else(|| {
                text_width(
                    window,
                    measured.clone(),
                    font.clone(),
                    font_size,
                    letter_spacing,
                )
            });
            let overflow = text_width - available_width;
            let changed = previous.is_none_or(|entry| (entry.overflow - overflow).abs() > 1.0);
            // A resize mid-glide keeps its place in the cycle.
            let started = previous.map_or_else(Instant::now, |entry| entry.started);
            record.insert(
                key,
                Measurement {
                    text: measured.clone(),
                    font,
                    font_size,
                    letter_spacing,
                    text_width,
                    available_width,
                    overflow,
                    started,
                },
            );
            // Just found out it doesn't fit: draw again, gliding.
            if changed && overflow > 1.0 {
                window.refresh();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    let container = container.child(measure);
    let (overflow, elapsed) = widths
        .borrow()
        .get(&key)
        .map_or((0.0, 0.0), |entry| {
            (entry.overflow, entry.started.elapsed().as_secs_f32())
        });
    if overflow <= 1.0 {
        return container
            .child(div().truncate().child(text))
            .into_any_element();
    }
    // Read off the clock rather than animated by GPUI, which would draw
    // the whole window every frame for as long as the pointer rests here,
    // pauses included; see [`next_wake`].
    let (offset, _) = position(elapsed, overflow);
    container
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .ml(px(-offset))
                .child(text),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_rests_glides_rests_and_returns() {
        let overflow = 36.0;
        let travel = overflow + 6.0;
        let glide = travel / SPEED;
        assert_eq!(position(0.0, overflow), (0.0, Some(Duration::from_secs_f32(HOLD_START))));
        let (mid, moving) = position(HOLD_START + glide / 2.0, overflow);
        assert!(mid > 0.0 && mid < travel && moving.is_none());
        let (held, wait) = position(HOLD_START + glide + 0.1, overflow);
        assert_eq!(held, travel);
        assert!(wait.is_some_and(|wait| wait > Duration::ZERO));
        let (back, moving) = position(HOLD_START + glide + HOLD_END + RETURN / 2.0, overflow);
        assert!(back > 0.0 && back < travel && moving.is_none());
        // Round again.
        let total = HOLD_START + glide + HOLD_END + RETURN;
        assert_eq!(position(total + 0.01, overflow).0, 0.0);
    }
}
