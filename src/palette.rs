//! The tab switcher (⌘K): open tabs first, then bookmarks, history and
//! commands matching what's typed, and a search for it. It covers the
//! page with a still of it, because GPUI can't draw over a live WebKit
//! view; the live view comes back when the switcher closes.

use std::{collections::HashSet, sync::Arc};

use gpui::{
    AnyElement, Context, Image, ImageFormat, MouseButton, SharedString, Window, div, img,
    prelude::*, px,
};
use vampir::{Palette, color, lighting};
use wry::WebViewExtMacOS;

use crate::{
    Browser, BrowserEvent, Chrome, Page, PaletteState,
    commands::{Command, Place},
    history,
    icons::{Icon, icon},
    native, site_icon, text_input,
};

/// What a row does when chosen.
#[derive(Clone)]
enum Choice {
    Tab(u64),
    Link(String),
    Search(String),
    Run(Command),
}

struct Row {
    choice: Choice,
    title: String,
    detail: String,
    kind: &'static str,
    /// The address its icon comes from, if it has a site.
    url: Option<String>,
    glyph: Icon,
}

const MAX_ROWS: usize = 12;
const OPEN: std::time::Duration = std::time::Duration::from_millis(180);
const CLOSE: std::time::Duration = std::time::Duration::from_millis(120);

impl Browser {
    pub(crate) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open.get() {
            self.close_palette(cx);
        } else {
            self.open_palette(window, cx);
        }
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = text_input(
            cx,
            "Switch to a tab, find a bookmark or page, or run a command",
            "",
        );
        let changes = cx.observe(&input, |this, _, cx| {
            this.caret_epoch = std::time::Instant::now();
            if let Some(palette) = &mut this.palette {
                palette.highlight = 0;
            }
            cx.notify();
        });
        self.palette = Some(PaletteState {
            input: input.clone(),
            highlight: 0,
            snapshot: None,
            opened: std::time::Instant::now(),
            closing: None,
            _changes: changes,
        });
        self.palette_open.set(true);
        let tab = self.current();
        // The live page stays up until its still arrives, so nothing
        // flashes blank.
        if let (Some(view), Page::Web) = (&tab.view, tab.page) {
            let _ = view.focus_parent();
            let sender = self.sender.clone();
            let id = tab.id;
            native::snapshot(&view.webview(), move |jpeg| {
                let _ = sender.try_send(BrowserEvent::Snapshot(id, jpeg));
            });
        }
        let focus = input.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// The page's still has arrived (or couldn't be taken): swap the live
    /// page out for it.
    pub(crate) fn palette_snapshot(&mut self, tab: u64, jpeg: Option<Vec<u8>>, cx: &mut Context<Self>) {
        // A still of a tab no longer in front (asked for before a switch) is
        // no use behind anything.
        if self.current().id != tab {
            return;
        }
        let Some(palette) = self.palette.as_mut().filter(|p| p.closing.is_none()) else {
            return;
        };
        palette.snapshot = jpeg.map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes)));
        // It can only be seen from now, with the page's still behind it: its
        // fade starts here, not at the key press, or it would pop in half
        // faded.
        palette.opened = std::time::Instant::now();
        if let Some(view) = &self.tabs[self.selected].view {
            let _ = view.set_visible(false);
        }
        cx.notify();
    }

    /// Starts the switcher fading out; [`Self::finish_closing_palette`]
    /// puts the live page back once it has.
    pub(crate) fn close_palette(&mut self, cx: &mut Context<Self>) {
        let Some(palette) = &mut self.palette else {
            return;
        };
        if palette.closing.is_some() {
            return;
        }
        palette.closing = Some(std::time::Instant::now());
        self.palette_open.set(false);
        cx.notify();
    }

    fn finish_closing_palette(&mut self) {
        self.palette = None;
        self.show_page_if_uncovered();
    }

    /// How far the switcher is in, from 0 (gone) to 1 (fully open), as it
    /// eases in on opening and out on closing.
    fn palette_presence(state: &PaletteState) -> f32 {
        match state.closing {
            Some(at) => 1.0 - (at.elapsed().as_secs_f32() / crate::slowed(CLOSE).as_secs_f32()).min(1.0),
            None => {
                let t = (state.opened.elapsed().as_secs_f32() / crate::slowed(OPEN).as_secs_f32()).min(1.0);
                1.0 - (1.0 - t).powi(3)
            }
        }
    }

    /// Whether the switcher is fading in or out, and so needs frames; once
    /// it sits open, nothing in it moves until something is typed.
    pub(crate) fn palette_animating(&self) -> bool {
        self.palette
            .as_ref()
            .is_some_and(|p| p.closing.is_some() || p.opened.elapsed() < crate::slowed(OPEN))
    }

    pub(crate) fn move_palette(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.palette_rows(cx).len() as isize;
        if let Some(palette) = &mut self.palette
            && count > 0
        {
            palette.highlight = (palette.highlight as isize + by).rem_euclid(count) as usize;
            cx.notify();
        }
    }

    pub(crate) fn choose_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(highlight) = self.palette.as_ref().map(|p| p.highlight) else {
            return;
        };
        self.choose_row(highlight, window, cx);
    }

    fn choose_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.palette_rows(cx);
        let Some(row) = rows.into_iter().nth(index) else {
            return;
        };
        self.close_palette(cx);
        // A link replaces the start page, and otherwise gets a tab of its own.
        let place = if self.current().page == Page::Start {
            Place::Here
        } else {
            Place::NewTab
        };
        match row.choice {
            Choice::Tab(id) => self.run(Command::SelectTabId(id), window, cx),
            Choice::Link(url) => self.open_link(&url, place, window, cx),
            Choice::Search(text) => {
                let url = crate::settings::destination(&text, &self.settings);
                self.open_link(&url, place, window, cx);
            }
            Choice::Run(command) => self.run(command, window, cx),
        }
    }

    fn palette_rows(&self, cx: &Context<Self>) -> Vec<Row> {
        let Some(palette) = &self.palette else {
            return Vec::new();
        };
        let content = palette.input.read(cx).content.clone();
        let query = content.trim();
        let lower = query.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        // One buffer for every entry's lowercased text, as this runs on
        // each key typed.
        let mut haystack = String::new();
        let mut matches = |title: &str, url: &str| {
            history::haystack_into(&mut haystack, title, url);
            history::matches_words(&haystack, &words)
        };
        let bookmarks = self.bookmarks();
        let history = self.history();
        let mut rows = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            if !matches(&tab.title, &tab.url) {
                continue;
            }
            seen.insert(&tab.url);
            rows.push(Row {
                choice: Choice::Tab(tab.id),
                title: tab.title.clone(),
                detail: if tab.page == Page::Web {
                    tab.url.clone()
                } else {
                    String::new()
                },
                kind: if index == self.selected {
                    "Current tab"
                } else if tab.private {
                    "Private tab"
                } else {
                    "Tab"
                },
                url: (tab.page == Page::Web).then(|| tab.url.clone()),
                glyph: match tab.page {
                    Page::Web => Icon::File,
                    Page::Start => Icon::Home,
                    Page::Settings => Icon::Gear,
                    Page::Downloads => Icon::Download,
                    Page::Bookmarks => Icon::Bookmark,
                },
            });
        }
        if !query.is_empty() {
            for (bookmark, path) in bookmarks.links_with_paths() {
                let Some(url) = bookmark.url.as_deref() else {
                    continue;
                };
                if seen.contains(url) || !matches(&bookmark.title, url) {
                    continue;
                }
                seen.insert(url);
                rows.push(Row {
                    choice: Choice::Link(url.to_owned()),
                    title: bookmark.title.clone(),
                    // Where it's filed, if not on the bar.
                    detail: if path.is_empty() { url.to_owned() } else { format!("{path} — {url}") },
                    kind: "Bookmark",
                    url: Some(url.to_owned()),
                    glyph: Icon::Bookmark,
                });
            }
            for visit in history.search(query, 8) {
                if !seen.insert(&visit.url) {
                    continue;
                }
                rows.push(Row {
                    choice: Choice::Link(visit.url.clone()),
                    title: if visit.title.is_empty() {
                        visit.url.clone()
                    } else {
                        visit.title.clone()
                    },
                    detail: visit.url.clone(),
                    kind: "History",
                    url: Some(visit.url.clone()),
                    glyph: Icon::File,
                });
            }
            for command in Command::palette() {
                if let Some(label) = command.palette_label()
                    && history::matches_words(&label.to_lowercase(), &words)
                {
                    rows.push(Row {
                        choice: Choice::Run(command),
                        title: label.to_owned(),
                        detail: String::new(),
                        kind: "Command",
                        url: None,
                        glyph: Icon::Gear,
                    });
                }
            }
            // Room kept for searching, however much else matches.
            rows.truncate(MAX_ROWS - 1);
            let engine =
                crate::settings::engine(&self.settings.search_engine).map_or("the web", |e| e.name);
            // A path would have `destination` look on disk for it; call it
            // an address and let choosing it find out.
            let is_address = query.starts_with('/')
                || query.starts_with("~/")
                || query.contains('.')
                || !crate::settings::destination(query, &self.settings).contains(query);
            rows.push(Row {
                choice: Choice::Search(query.to_owned()),
                title: if is_address && !query.contains(' ') {
                    format!("Open {query}")
                } else {
                    format!("Search {engine} for “{query}”")
                },
                detail: String::new(),
                kind: "Go",
                url: None,
                glyph: Icon::Search,
            });
        }
        rows.truncate(MAX_ROWS);
        rows
    }

    pub(crate) fn palette_overlay(
        &mut self,
        palette: Palette,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = Chrome::new(palette);
        if self
            .palette
            .as_ref()
            .and_then(|p| p.closing)
            .is_some_and(|at| at.elapsed() >= crate::slowed(CLOSE))
        {
            self.finish_closing_palette();
            return div().into_any_element();
        }
        let rows = self.palette_rows(cx);
        let Some(state) = &self.palette else {
            return div().into_any_element();
        };
        let presence = Self::palette_presence(state);
        let highlight = state.highlight.min(rows.len().saturating_sub(1));
        let snapshot = state.snapshot.clone();
        let input = state.input.clone();
        let caret = self.caret_color(palette.accent);
        input.update(cx, |input, _| {
            input.restyle(palette);
            input.style.cursor_color = caret;
        });
        let mut list = div().flex().flex_col().gap(px(3.0)).p(px(8.0));
        if rows.is_empty() {
            list = list.child(
                div()
                    .px(px(16.0))
                    .py(px(12.0))
                    .text_color(palette.text_secondary)
                    .child("Nothing matches."),
            );
        }
        for (index, row) in rows.iter().enumerate() {
            let selected = index == highlight;
            let leading: AnyElement = match &row.url {
                Some(url) => site_icon(self.shown_favicon(url), url, 18.0, false, palette),
                None => div()
                    .size(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(row.glyph, 14.0, palette.text_secondary))
                    .into_any_element(),
            };
            list = list.child(
                div()
                    .id(("palette-row", index))
                    .h(px(if row.detail.is_empty() { 38.0 } else { 46.0 }))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(12.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .when(selected, |el| el.bg(palette.soft_fill))
                    .when(!selected, |el| el.hover(move |s| s.bg(chrome.wash)))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.choose_row(index, window, cx)),
                    )
                    .child(leading)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .truncate()
                                    .text_color(palette.text_primary)
                                    .child(SharedString::from(row.title.clone())),
                            )
                            .when(!row.detail.is_empty(), |el| {
                                el.child(
                                    div()
                                        .truncate()
                                        .text_size(px(11.5))
                                        .text_color(palette.text_secondary)
                                        .child(SharedString::from(row.detail.clone())),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(palette.text_secondary)
                            .child(row.kind),
                    ),
            );
        }
        let card = div()
            .id("palette-card")
            .occlude()
            .w(px(640.0))
            .max_w_full()
            .flex()
            .flex_col()
            .rounded(px(14.0))
            .bg(chrome.raised)
            .border_1()
            .border_color(color::with_alpha(palette.field_border_strong, 0.5))
            .shadow(lighting::panel(palette.is_dark))
            .overflow_hidden()
            .child(
                div()
                    .h(px(56.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(20.0))
                    .child(icon(Icon::Search, 16.0, palette.text_secondary))
                    .child(div().flex_1().min_w(px(0.0)).child(input)),
            )
            .child(div().h(px(1.0)).bg(chrome.line))
            .child(list)
            .child(div().h(px(1.0)).bg(chrome.line))
            .child(
                div()
                    .h(px(34.0))
                    .flex()
                    .items_center()
                    .gap(px(18.0))
                    .px(px(20.0))
                    .text_size(px(11.0))
                    .text_color(palette.text_secondary)
                    .child("↑↓ to move")
                    .child("↩ to open")
                    .child("esc to close")
                    .child(div().flex_1())
                    .child("@k, @g, @w… search one engine"),
            );
        div()
            .id("palette-overlay")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(palette.backdrop)
            .when_some(snapshot, |el, image| {
                el.child(img(image).absolute().top_0().left_0().size_full())
            })
            .child(
                div()
                    .id("palette-scrim")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(palette.scrim())
                    .opacity(presence)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.close_palette(cx)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top(px(36.0 - 14.0 * (1.0 - presence)))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .px(px(24.0))
                    .opacity(presence)
                    .child(card),
            )
            .into_any_element()
    }
}
