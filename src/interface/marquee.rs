//! Labels too long for their place: cut off at rest, and on hover they
//! glide along to show the rest, pause, and come back, as the iOS lock
//! screen does with a song title. In place of tooltips.

use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt, AnyElement, ElementId, Font, Pixels, SharedString, TextRun, Window,
    canvas, div, prelude::*, px,
};

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
    let overflow = widths
        .borrow()
        .get(&key)
        .map_or(0.0, |entry| entry.overflow);
    if overflow <= 1.0 {
        return container
            .child(div().truncate().child(text))
            .into_any_element();
    }
    let travel = overflow + 6.0;
    let glide = travel / SPEED;
    let total = HOLD_START + glide + HOLD_END + RETURN;
    container
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .child(text)
                .with_animation(
                    ElementId::NamedInteger("marquee".into(), key),
                    Animation::new(Duration::from_secs_f32(total)).repeat(),
                    move |el, t| {
                        let at = t * total;
                        let offset = if at < HOLD_START {
                            0.0
                        } else if at < HOLD_START + glide {
                            let p = (at - HOLD_START) / glide;
                            // Eased at both ends, like a hand scrolling.
                            travel * smooth(p)
                        } else if at < HOLD_START + glide + HOLD_END {
                            travel
                        } else {
                            // Back to the start, briskly but not in a jump.
                            let p = (at - HOLD_START - glide - HOLD_END) / RETURN;
                            travel * (1.0 - smooth(p.min(1.0)))
                        };
                        el.ml(px(-offset))
                    },
                ),
        )
        .into_any_element()
}
