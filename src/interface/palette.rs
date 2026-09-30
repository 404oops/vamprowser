//! The tab switcher (⌘K): open tabs first, then bookmarks, history and
//! commands matching what's typed, and a search for it. It covers the
//! page with a still of it, because GPUI can't draw over a live WebKit
//! view; the live view comes back when the switcher closes.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use gpui::{
    AnyElement, Context, Image, ImageFormat, MouseButton, SharedString, Window, div,
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
    suggest::{page_rank, page_score_fields},
};

/// What a row does when chosen.
#[derive(Clone)]
enum Choice {
    Tab(u64),
    Link(String),
    Search(String),
    Run(Command),
}

#[derive(Clone)]
struct Row {
    choice: Choice,
    title: String,
    detail: String,
    kind: &'static str,
    /// The address its icon comes from, if it has a site.
    url: Option<String>,
    glyph: Icon,
}

struct RankedPage<'a> {
    url: &'a str,
    title: &'a str,
    path: String,
    score: i32,
    bookmark: bool,
    visits: u32,
    recent: u64,
    order: usize,
}

fn rank_pages(mut pages: Vec<RankedPage<'_>>, count: usize) -> Vec<Row> {
    let compare = |a: &RankedPage<'_>, b: &RankedPage<'_>| {
        page_rank(b.score, b.bookmark, b.visits, b.recent)
            .cmp(&page_rank(a.score, a.bookmark, a.visits, a.recent))
            .then(a.order.cmp(&b.order))
    };
    if pages.len() > count {
        if count == 0 {
            pages.clear();
        } else {
            pages.select_nth_unstable_by(count, compare);
            pages.truncate(count);
        }
    }
    pages.sort_unstable_by(compare);
    pages
        .into_iter()
        .map(|page| Row {
            choice: Choice::Link(page.url.to_owned()),
            title: page.title.to_owned(),
            detail: if page.path.is_empty() {
                page.url.to_owned()
            } else {
                format!("{} — {}", page.path, page.url)
            },
            kind: if page.bookmark { "Bookmark" } else { "History" },
            url: Some(page.url.to_owned()),
            glyph: if page.bookmark {
                Icon::Bookmark
            } else {
                Icon::File
            },
        })
        .collect()
}

struct CachedTab {
    id: u64,
    title: String,
    url: String,
    page: Page,
    private: bool,
}

pub(crate) struct RowsCache {
    query: SharedString,
    history: u64,
    bookmarks: u64,
    selected: usize,
    tabs: Vec<CachedTab>,
    settings: crate::settings::Settings,
    rows: Arc<[Row]>,
}

impl RowsCache {
    fn fresh(&self, browser: &Browser, query: &str, history: u64, bookmarks: u64) -> bool {
        self.query.as_ref() == query
            && self.history == history
            && self.bookmarks == bookmarks
            && self.selected == browser.selected
            && self.settings == browser.settings
            && self.tabs.len() == browser.tabs.len()
            && self.tabs.iter().zip(&browser.tabs).all(|(cached, tab)| {
                cached.id == tab.id
                    && cached.title == tab.title
                    && cached.url == tab.url
                    && cached.page == tab.page
                    && cached.private == tab.private
            })
    }
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
        self.close_suggestions(cx);
        self.close_bookmark_menu(cx);
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
        let request = std::time::Instant::now();
        self.palette = Some(PaletteState {
            input: input.clone(),
            highlight: 0,
            snapshot: None,
            snapshot_request: request,
            opened: request,
            closing: None,
            _changes: changes,
            rows: Default::default(),
        });
        self.palette_open.set(true);
        let tab = self.current();
        // The live page stays up until its still arrives, so nothing
        // flashes blank.
        if let (Some(view), Page::Web) = (&tab.view, tab.page) {
            let sender = self.sender.clone();
            let id = tab.id;
            native::snapshot(&view.webview(), move |jpeg| {
                let _ = sender.try_send(BrowserEvent::Snapshot(id, request, jpeg));
            });
        }
        self.focus_text_input(input, false, window, cx);
    }

    /// The page's still has arrived (or couldn't be taken): swap the live
    /// page out for it.
    pub(crate) fn palette_snapshot(&mut self, tab: u64, request: std::time::Instant, jpeg: Option<Vec<u8>>, cx: &mut Context<Self>) {
        // A still of a tab no longer in front (asked for before a switch) is
        // no use behind anything.
        if self.current().id != tab {
            return;
        }
        let Some(palette) = self.palette.as_mut().filter(|p| p.closing.is_none() && p.snapshot_request == request) else {
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
    /// schedules the page to be drawn again once it has.
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

    fn finish_closing_palette(&mut self, cx: &mut Context<Self>) {
        self.palette = None;
        // The page canvas restores a web view after applying its bounds.
        cx.notify();
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
        let Some(choice) = rows.get(index).map(|row| row.choice.clone()) else {
            return;
        };
        self.close_palette(cx);
        // A link replaces the start page, and otherwise gets a tab of its own.
        let place = if self.current().page == Page::Start {
            Place::Here
        } else {
            Place::NewTab
        };
        match choice {
            Choice::Tab(id) => self.run(Command::SelectTabId(id), window, cx),
            Choice::Link(url) => self.open_link(&url, place, window, cx),
            Choice::Search(text) => {
                let url = crate::settings::destination(&text, &self.settings);
                self.open_link(&url, place, window, cx);
            }
            Choice::Run(command) => self.run(command, window, cx),
        }
    }

    fn palette_rows(&self, cx: &Context<Self>) -> Arc<[Row]> {
        let Some(palette) = &self.palette else {
            return Arc::from([]);
        };
        let content = palette.input.read(cx).content.clone();
        let query = content.trim();
        let bookmarks = self.bookmarks();
        let history = self.history();
        if let Some(cache) = palette.rows.borrow().as_ref()
            && cache.fresh(self, &content, history.revision(), bookmarks.revision())
        {
            return cache.rows.clone();
        }
        let lower = query.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        // One buffer for every entry's lowercased text, as this runs on
        // each key typed.
        let mut haystack = String::new();
        let mut matches = |title: &str, url: &str| {
            history::haystack_into(&mut haystack, title, url);
            history::matches_words(&haystack, &words)
        };
        let mut rows = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let tab_limit = if query.is_empty() {
            MAX_ROWS
        } else {
            MAX_ROWS - 1
        };
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
            if rows.len() == tab_limit {
                break;
            }
        }
        if !query.is_empty() {
            let mut pages: Vec<RankedPage> = Vec::new();
            let count = (MAX_ROWS - 1).saturating_sub(rows.len());
            if count > 0 {
                let bookmark_fields = bookmarks.scoring_fields();
                let mut at: HashMap<&str, usize> = HashMap::new();
                for ((bookmark, path), (title_lower, address_lower)) in bookmarks
                    .links_with_paths()
                    .into_iter()
                    .zip(bookmark_fields.iter())
                {
                    let Some(url) = bookmark.url.as_deref() else {
                        continue;
                    };
                    let Some(score) = page_score_fields(&lower, title_lower, address_lower) else {
                        continue;
                    };
                    if seen.contains(url) || at.contains_key(url) {
                        continue;
                    }
                    at.insert(url, pages.len());
                    pages.push(RankedPage {
                        url,
                        title: &bookmark.title,
                        path,
                        order: pages.len(),
                        score,
                        bookmark: true,
                        visits: 0,
                        recent: 0,
                    });
                }
                for visit in history.recent(usize::MAX) {
                    let (title_lower, address_lower) = visit.search_fields();
                    let Some(score) = page_score_fields(&lower, title_lower, address_lower) else {
                        continue;
                    };
                    if seen.contains(visit.url.as_str()) {
                        continue;
                    }
                    if let Some(&index) = at.get(visit.url.as_str()) {
                        let page = &mut pages[index];
                        if score > page.score {
                            page.score = score;
                            page.title = &visit.title;
                        }
                        page.visits = visit.visits;
                        page.recent = visit.last_visit;
                    } else {
                        at.insert(&visit.url, pages.len());
                        pages.push(RankedPage {
                            url: &visit.url,
                            title: if visit.title.is_empty() {
                                &visit.url
                            } else {
                                &visit.title
                            },
                            path: String::new(),
                            order: pages.len(),
                            score,
                            bookmark: false,
                            visits: visit.visits,
                            recent: visit.last_visit,
                        });
                    }
                }
            }
            rows.extend(rank_pages(pages, count));
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
        let rows: Arc<[Row]> = rows.into();
        *palette.rows.borrow_mut() = Some(RowsCache {
            query: content,
            history: history.revision(),
            bookmarks: bookmarks.revision(),
            selected: self.selected,
            tabs: self
                .tabs
                .iter()
                .map(|tab| CachedTab {
                    id: tab.id,
                    title: tab.title.clone(),
                    url: tab.url.clone(),
                    page: tab.page,
                    private: tab.private,
                })
                .collect(),
            settings: self.settings.clone(),
            rows: rows.clone(),
        });
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
            self.finish_closing_palette(cx);
            return div().into_any_element();
        }
        let rows = self.palette_rows(cx);
        let Some(state) = &self.palette else {
            return div().into_any_element();
        };
        let presence = Self::palette_presence(state);
        let highlight = state.highlight.min(rows.len().saturating_sub(1));
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
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .relative()
            .overflow_hidden()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_page_ranking_keeps_frequency_and_stable_ties() {
        let names = [
            "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q",
            "r", "s", "t",
        ];
        let pages = names
            .into_iter()
            .enumerate()
            .map(|(order, name)| RankedPage {
                url: name,
                title: name,
                path: String::new(),
                score: 120,
                bookmark: false,
                visits: (order % 3) as u32,
                recent: 0,
                order,
            })
            .collect();
        let rows = rank_pages(pages, 6);
        assert_eq!(
            rows.iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["c", "f", "i", "l", "o", "r"]
        );
    }

    #[test]
    fn full_tab_list_leaves_no_page_rows_and_bookmark_details_survive() {
        let make = || {
            vec![RankedPage {
                url: "https://example.org/",
                title: "Example",
                path: "Work › Docs".into(),
                score: 120,
                bookmark: true,
                visits: 0,
                recent: 0,
                order: 0,
            }]
        };
        assert!(rank_pages(make(), 0).is_empty());
        let rows = rank_pages(make(), 12);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "Bookmark");
        assert_eq!(rows[0].detail, "Work › Docs — https://example.org/");
    }
}
