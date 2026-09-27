//! Small browser-owned prompts, drawn with Vampir controls in a GPUI dialog
//! window so they remain above a live WebKit page.

use async_channel::{Receiver, Sender};
use gpui::{
    App, Context, Entity, Render, Window, WindowBounds, WindowKind, WindowOptions, canvas, div,
    prelude::*, px, size,
};

const WIDTH: f32 = 420.0;
const PADDING: f32 = 20.0;
const GAP: f32 = 14.0;

/// A first guess at the height the dialog needs, close enough that fitting
/// it to what it really holds (see `render`) barely moves it: the title,
/// the message at about 55 characters a line, the field, the buttons.
fn estimated_height(message: &str, field: bool) -> f32 {
    let lines = message
        .split('\n')
        .map(|line| (line.chars().count() as f32 / 55.0).ceil().max(1.0))
        .sum::<f32>();
    let message = if message.is_empty() { 0.0 } else { lines * 18.0 + GAP };
    let field = if field { 32.0 + GAP } else { 0.0 };
    2.0 * PADDING + 22.0 + GAP + message + field + 32.0
}
use vampir::{
    ButtonVariant, ControlHost, ControlState, InputStyle, Palette, TextInput, lighting, ui_font,
};

pub(crate) enum Kind {
    Prompt(String),
    Confirm,
    Alert,
}

struct Dialog {
    controls: ControlState,
    palette: Palette,
    title: String,
    message: String,
    button: String,
    alert: bool,
    input: Option<Entity<TextInput>>,
    answer: Sender<Option<String>>,
}

impl ControlHost for Dialog {
    fn control_state(&self) -> &ControlState {
        &self.controls
    }
    fn control_state_mut(&mut self) -> &mut ControlState {
        &mut self.controls
    }
}

impl Dialog {
    fn finish(&mut self, accepted: bool, window: &mut Window, cx: &mut Context<Self>) {
        let answer = accepted.then(|| {
            self.input
                .as_ref()
                .map_or_else(String::new, |input| input.read(cx).text())
        });
        let _ = self.answer.try_send(answer);
        window.remove_window();
    }
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let palette = self.palette;
        let input = self.input.clone();
        let title = self.title.clone();
        let message = self.message.clone();
        let button = self.button.clone();
        let alert = self.alert;
        let cancel = (!alert).then(|| {
            div().min_w(px(110.0)).flex_none().child(vampir::button(
                "dialog-cancel",
                "Cancel",
                ButtonVariant::Soft,
                true,
                palette,
                cx,
                |this: &mut Dialog, window, cx| this.finish(false, window, cx),
            ))
        });
        let submit = div().min_w(px(110.0)).flex_none().child(vampir::button(
            "dialog-submit",
            &button,
            ButtonVariant::Primary,
            true,
            palette,
            cx,
            |this: &mut Dialog, window, cx| this.finish(true, window, cx),
        ));
        // The window takes the height of what it holds, however long the
        // message runs, rather than a fixed one that cut the buttons off.
        let fit = canvas(
            |bounds, window, _| {
                let wanted = f32::from(bounds.size.height) + 2.0 * PADDING;
                if (f32::from(window.viewport_size().height) - wanted).abs() > 0.5 {
                    window.resize(size(px(WIDTH), px(wanted)));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let content = div()
            .relative()
            .flex_none()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .child(fit);
        vampir::root(div().id("app-dialog"), self, cx)
            .size_full()
            .p(px(PADDING))
            .flex()
            .flex_col()
            .font_family(ui_font())
            .text_size(px(13.0))
            .text_color(palette.text_primary)
            .bg(palette.field_surface)
            .on_action(
                cx.listener(|this, _: &vampir::keyboard::Dismiss, window, cx| {
                    this.finish(false, window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &vampir::text_input::Enter, window, cx| {
                    this.finish(true, window, cx);
                }),
            )
            .child(
                content
                    .child(
                        div()
                            .text_size(px(17.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when(!message.is_empty(), |el| {
                        el.child(div().text_color(palette.text_secondary).child(message))
                    })
                    .children(
                        input
                            .as_ref()
                            .map(|input| vampir::text_field(input, palette, window, cx)),
                    )

                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.0))
                            .children(cancel)
                            .child(submit),
                    ),
            )
            .shadow(lighting::panel(palette.is_dark))
    }
}

pub(crate) fn open(
    cx: &mut App,
    palette: Palette,
    title: impl Into<String>,
    message: impl Into<String>,
    button: impl Into<String>,
    kind: Kind,
) -> Receiver<Option<String>> {
    let (sender, receiver) = async_channel::bounded(1);
    let alert = matches!(kind, Kind::Alert);
    let input = match kind {
        Kind::Prompt(initial) => Some(cx.new(|cx| {
            let mut input = TextInput::new(cx, "", false, InputStyle::from_palette(palette, 13.0));
            input.set_text(&initial, cx);
            input
        })),
        Kind::Confirm | Kind::Alert => None,
    };
    let title = title.into();
    let message = message.into();
    let height = estimated_height(&message, input.is_some());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(WIDTH), px(height)), cx)),
        kind: WindowKind::Dialog,
        titlebar: None,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    let focus = input
        .as_ref()
        .map(|input| input.read(cx).focus_handle.clone());
    let result = cx.open_window(options, move |window, cx| {
        if let Some(focus) = &focus {
            window.focus(focus, cx);
        }
        cx.new(|_| Dialog {
            controls: ControlState::default(),
            palette,
            title,
            message,
            button: button.into(),
            alert,
            input,
            answer: sender,
        })
    });
    if result.is_err() {
        // Dropping the sender makes the awaiting caller cancel.
    }
    receiver
}
