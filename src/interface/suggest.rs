//! Address field suggestions: as you type, the field completes the site
//! you most likely mean, and a list under it offers matching history and
//! bookmarks, most visited first (history can be removed from there), then
//! searches: what you typed, and your search engine's suggestions. Like the tab switcher, the list covers the page with a still
//! of it, because GPUI can't draw over a live WebKit view.

use std::{collections::HashMap, ops::Range, sync::Arc};

use gpui::{
    AnyElement, Bounds, Context, FontWeight, Image, ImageFormat, MouseButton, Pixels, SharedString, StyledText, Window,
    div, img, prelude::*, px,
};
use unicode_segmentation::UnicodeSegmentation;
use vampir::{Palette, color, lighting};
use wry::WebViewExtMacOS;

use crate::{
    Browser, BrowserEvent, Chrome, Page,
    icons::{Icon, icon},
    native, settings, site_icon,
};

/// Search suggestions shown at most, and history and bookmarks.
const REMOTE_ROWS: usize = 4;
const LOCAL_ROWS: usize = 5;

#[derive(Clone, PartialEq)]
pub(crate) enum Suggestion {
    /// A page from history or bookmarks.
    Page { url: String, title: String, bookmark: bool },
    Search(String),
}

impl Suggestion {
    /// What the field shows when the row is picked with the arrow keys.
    fn fill(&self) -> &str {
        match self {
            Suggestion::Page { url, .. } => url,
            Suggestion::Search(query) => query,
        }
    }
}

pub(crate) struct SuggestState {
    /// What was typed, without the completion.
    typed: String,
    /// Whatever the search engine suggested, and for what.
    remote: Vec<String>,
    remote_for: String,
    rows: Vec<Suggestion>,
    highlight: Option<usize>,
    /// The highlight was moved there with the arrow keys, rather than being
    /// the completion the field shows.
    picked: bool,
    /// A still of the page it covers, which is hidden meanwhile.
    snapshot: Option<Arc<Image>>,
    snapshot_request: Option<std::time::Instant>,
    /// The page's still has come (or couldn't be had): the list can show
    /// without half of it hidden behind the live page.
    covered: bool,
}

/// An address without its scheme and `www.`, as it's matched against what's
/// typed and completed.
fn bare(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.strip_prefix("www.").unwrap_or(rest)
}

/// Relevance before visit frequency: a word at the start of a title or
/// hostname should beat an unrelated page that happens to contain the same
/// letters in a long path. A subsequence still finds abbreviated names.
fn field_score(word: &str, field: &str) -> Option<i32> {
    let field = field.to_lowercase();
    if field.starts_with(word) {
        return Some(120);
    }
    if field.match_indices(word).any(|(at, _)| {
        field[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric())
    }) {
        return Some(105);
    }
    if field.contains(word) {
        return Some(75);
    }
    if word.chars().count() < 2 {
        return None;
    }
    vampir::fuzzy_score(word, &field).map(|score| (35 + score).clamp(1, 65))
}

pub(crate) fn page_score(query: &str, title: &str, url: &str) -> Option<i32> {
    query.split_whitespace().try_fold(0, |total, word| {
        let title = field_score(word, title);
        let address = field_score(word, bare(url)).map(|score| score - 8);
        Some(total + title.into_iter().chain(address).max()?)
    })
}

/// Shared ordering for page matches in the address field and tab switcher.
pub(crate) fn page_rank(score: i32, bookmark: bool, visits: u32, recent: u64) -> (i32, bool, u32, u64) {
    (score, bookmark, visits, recent)
}

/// Byte ranges to emphasize in a suggestion. Prefer a whole substring; for
/// fuzzy matches, emphasize the individual letters that formed the match.
fn matched_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let same = |a: char, b: char| a.to_lowercase().eq(b.to_lowercase());
    let mut ranges = Vec::new();
    for word in query.split_whitespace() {
        let needle: Vec<char> = word.chars().collect();
        if needle.is_empty() { continue; }
        let positions = (0..=chars.len().saturating_sub(needle.len()))
            .find(|&start| start + needle.len() <= chars.len()
                && needle.iter().enumerate().all(|(offset, &c)| same(chars[start + offset].1, c)))
            .map(|start| (start..start + needle.len()).collect::<Vec<_>>())
            .or_else(|| {
                let mut found = Vec::new();
                let mut from = 0;
                for &c in &needle {
                    let index = (from..chars.len()).find(|&i| same(chars[i].1, c))?;
                    found.push(index);
                    from = index + 1;
                }
                Some(found)
            });
        if let Some(positions) = positions {
            for index in positions {
                let start = chars[index].0;
                let end = chars.get(index + 1).map_or(text.len(), |next| next.0);
                ranges.push(start..end);
            }
        }
    }
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut() && range.start <= last.end {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn emphasized(text: String, query: &str) -> StyledText {
    StyledText::new(text.clone()).with_highlights(
        matched_ranges(&text, query).into_iter().map(|range| (range, FontWeight::BOLD.into()))
    )
}

/// The engine's suggestion service, in the OpenSearch suggestions format
/// or near enough. Engines without one borrow DuckDuckGo's, which keeps
/// no record of who asked.
pub(crate) fn endpoint(settings: &settings::Settings) -> String {
    let engine = settings.search_engine.as_str();
    let ddg = "https://duckduckgo.com/ac/?q=%s&type=list";
    let instance = |id: &str, path: &str| {
        settings
            .instances
            .get(id)
            .map(|base| base.trim().trim_end_matches('/'))
            .filter(|base| !base.is_empty())
            .map(|base| format!("{base}{path}"))
    };
    match engine {
        "google" | "google_web" => {
            "https://suggestqueries.google.com/complete/search?client=firefox&q=%s".into()
        }
        "youtube" => {
            "https://suggestqueries.google.com/complete/search?client=firefox&ds=yt&q=%s".into()
        }
        "bing" => "https://api.bing.com/osjson.aspx?query=%s".into(),
        "brave" => "https://search.brave.com/api/suggest?q=%s".into(),
        "startpage" => "https://www.startpage.com/osuggestions?q=%s".into(),
        "ecosia" => "https://ac.ecosia.org/autocomplete?q=%s&type=list".into(),
        "yahoo" => "https://search.yahoo.com/sugg/os?command=%s&output=fxjson".into(),
        "yandex" => "https://suggest.yandex.com/suggest-ff.cgi?part=%s".into(),
        "qwant" => "https://api.qwant.com/v3/suggest?q=%s".into(),
        "wikipedia" => {
            "https://en.wikipedia.org/w/api.php?action=opensearch&format=json&search=%s".into()
        }
        "searxng" => instance("searxng", "/autocompleter?q=%s").unwrap_or_else(|| ddg.into()),
        "whoogle" => instance("whoogle", "/autocomplete?q=%s").unwrap_or_else(|| ddg.into()),
        _ => ddg.into(),
    }
}

/// Reads suggestions from the shapes services answer in: OpenSearch's
/// `[query, [suggestions…]]`, a list of `{phrase}` objects, or Qwant's
/// `{data: {items: [{value}]}}`.
pub(crate) fn parse(body: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let strings = |list: &serde_json::Value, key: Option<&str>| -> Vec<String> {
        list.as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match key {
                        Some(key) => item.get(key)?.as_str(),
                        None => item.as_str(),
                    })
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    if let Some(list) = value.get(1).filter(|v| v.is_array()) {
        return strings(list, None);
    }
    if value.as_array().is_some() {
        return strings(&value, Some("phrase"));
    }
    strings(&value["data"]["items"], Some("value"))
}

fn fetch(template: &str, query: &str) -> Vec<String> {
    let url = template.replace("%s", &settings::encode(query));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(3)))
        .build()
        .into();
    agent
        .get(&url)
        .call()
        .ok()
        .and_then(|mut response| response.body_mut().read_to_string().ok())
        .map(|body| parse(&body))
        .unwrap_or_default()
}

impl Browser {
    /// Enter takes the selected suggestion, including the automatically
    /// selected best local match. Without one it submits the typed address.
    pub(crate) fn submitted_address(&self, typed: String) -> String {
        self.suggest.as_ref()
            .and_then(|state| state.highlight.and_then(|index| state.rows.get(index)))
            .map_or(typed, |row| row.fill().to_owned())
    }

    /// Pages from history and bookmarks matching `typed`, best first, and
    /// what the field should complete to, if one's address starts with it.
    fn local_matches(&self, typed: &str) -> (Option<(String, String, String)>, Vec<Suggestion>) {
        let lower = typed.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        if words.is_empty() {
            return (None, Vec::new());
        }
        struct Found {
            url: String,
            title: String,
            bookmark: bool,
            score: i32,
            visits: u32,
            recent: u64,
            /// The address without its scheme and `www.`, lowercased.
            address: String,
        }
        let bookmarks = self.bookmarks();
        let history = self.history();
        let mut found: Vec<Found> = Vec::new();
        // Where each address is in `found`, to combine a bookmark and its visits.
        let mut at: HashMap<String, usize> = HashMap::new();
        let entry = |url: &str, title: &str, bookmark: bool, score: i32, visits: u32, recent: u64| Found {
            url: url.to_owned(),
            title: title.to_owned(),
            bookmark,
            score,
            visits,
            recent,
            address: bare(url).to_lowercase(),
        };
        for bookmark in bookmarks.links() {
            let Some(url) = bookmark.url.as_deref() else {
                continue;
            };
            if let Some(score) = page_score(&lower, &bookmark.title, url)
                && !at.contains_key(url)
            {
                at.insert(url.to_owned(), found.len());
                found.push(entry(url, &bookmark.title, true, score, 0, 0));
            }
        }
        for visit in history.recent(usize::MAX) {
            let Some(score) = page_score(&lower, &visit.title, &visit.url) else {
                continue;
            };
            match at.get(visit.url.as_str()) {
                Some(&index) => {
                    let item = &mut found[index];
                    if score > item.score {
                        item.title = visit.title.clone();
                        item.score = score;
                    }
                    item.visits = visit.visits;
                    item.recent = visit.last_visit;
                }
                None => {
                    at.insert(visit.url.clone(), found.len());
                    found.push(entry(&visit.url, &visit.title, false, score, visit.visits, visit.last_visit));
                }
            }
        }
        found.sort_by_key(|item| std::cmp::Reverse(page_rank(item.score, item.bookmark, item.visits, item.recent)));
        let bookmarked = |url: &str| found.iter().any(|f| f.url == url && f.bookmark);

        // Completing: a single word the start of an address, finishing at
        // the host unless what's typed already reaches into the path.
        let mut completion = None;
        if words.len() == 1 && !typed.contains(char::is_whitespace) {
            for entry in &found {
                if !entry.address.starts_with(&lower) {
                    continue;
                }
                let address = bare(&entry.url);
                let end = if lower.contains('/') {
                    address.len()
                } else {
                    address.find('/').unwrap_or(address.len())
                };
                let full = address[..end].trim_end_matches('/');
                if full.len() >= typed.len() {
                    let target = if lower.contains('/') {
                        entry.url.clone()
                    } else {
                        url::Url::parse(&entry.url)
                            // The site's front page, port and all.
                            .map(|u| match u.scheme() {
                                "http" | "https" => format!("{}/", u.origin().ascii_serialization()),
                                scheme => format!("{scheme}://{}/", u.host_str().unwrap_or_default()),
                            })
                            .unwrap_or_else(|_| entry.url.clone())
                    };
                    completion = Some((full.to_owned(), target, entry.title.clone()));
                    break;
                }
            }
        }
        // The page the field completes to leads, then the rest.
        let mut rows: Vec<Suggestion> = completion
            .as_ref()
            .map(|(_, target, title)| Suggestion::Page {
                url: target.clone(),
                title: title.clone(),
                bookmark: bookmarked(target),
            })
            .into_iter()
            .collect();
        rows.extend(
            found
                .into_iter()
                .filter(|entry| {
                    completion.as_ref().is_none_or(|(_, target, _)| {
                        target.trim_end_matches('/') != entry.url.trim_end_matches('/')
                    })
                })
                .take(LOCAL_ROWS)
                .map(|entry| Suggestion::Page {
                    url: entry.url,
                    title: entry.title,
                    bookmark: entry.bookmark,
                }),
        );
        (completion, rows)
    }

    /// The rows for what's typed: history and bookmarks, then searches.
    fn rows_for(&self, typed: &str, local: Vec<Suggestion>) -> Vec<Suggestion> {
        let mut rows = local;
        rows.push(Suggestion::Search(typed.trim().to_owned()));
        let lower = typed.trim().to_lowercase();
        if let Some(state) = &self.suggest {
            rows.extend(
                state
                    .remote
                    .iter()
                    .filter(|s| s.to_lowercase() != lower)
                    .take(REMOTE_ROWS)
                    .cloned()
                    .map(Suggestion::Search),
            );
        }
        rows
    }

    /// Something was typed in the address field (not set by us): suggest,
    /// and complete inline when characters were added at the end.
    pub(crate) fn address_edited(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.address.read(cx).focus_handle.is_focused(window) {
            return;
        }
        if text.trim().is_empty() {
            self.close_suggestions(cx);
            return;
        }
        // Typed on since: a later edit is on its way; this one is stale, and
        // completing it would write over what came after.
        if self.address.read(cx).content.as_ref() != text {
            return;
        }
        let grew = self
            .suggest
            .as_ref()
            .is_some_and(|s| text.len() > s.typed.len() && text.starts_with(&s.typed))
            || (self.suggest.is_none() && !text.is_empty());
        let (completion, local) = self.local_matches(&text);
        // Whether Enter would open the first row: the field holds its address.
        let mut completed = false;
        if let Some((full, _, _)) = completion {
            completed = full.eq_ignore_ascii_case(&text);
            if grew && full.to_lowercase().starts_with(&text.to_lowercase()) && full.len() > text.len() {
                // What's typed keeps its case; the rest is selected, so the
                // next key replaces it and Delete drops it.
                let tail = &full[text.len()..];
                let shown = format!("{text}{tail}");
                let steps = tail.graphemes(true).count();
                self.address.update(cx, |input, cx| input.set_text(&shown, cx));
                let focus = self.address.read(cx).focus_handle.clone();
                for _ in 0..steps {
                    focus.dispatch_action(&vampir::text_input::SelectLeft, window, cx);
                }
                completed = true;
            }
        }
        // Enter opens what the field shows: the completion, when it shows
        // one; otherwise what was typed. Other matches are a press of ↓
        // away. (Opening the best match regardless took `github.com/new`
        // to `github.com/notifications`, and a completion deleted with ⌫
        // to it anyway.)
        let highlight = completed.then_some(0);
        let private = self.current().private;
        let remote_wanted = self.settings.search_suggestions && !private;
        let rows = self.rows_for(&text, local);
        let state = self.suggest.get_or_insert_with(|| SuggestState {
            typed: String::new(),
            remote: Vec::new(),
            remote_for: String::new(),
            rows: Vec::new(),
            highlight: None,
            picked: false,
            snapshot: None,
            snapshot_request: None,
            covered: false,
        });
        state.typed = text.clone();
        state.highlight = highlight;
        state.picked = false;
        state.rows = rows;
        if remote_wanted && state.remote_for != text {
            let template = endpoint(&self.settings);
            let sender = self.sender.clone();
            let query = text.clone();
            std::thread::spawn(move || {
                let found = fetch(&template, &query);
                let _ = sender.try_send(BrowserEvent::Suggestions(query, found));
            });
        }
        self.cover_page_for_suggestions();
        cx.notify();
    }

    /// The search engine answered for `query`; kept only if that's still
    /// what's typed.
    pub(crate) fn remote_suggestions(
        &mut self,
        query: String,
        found: Vec<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &mut self.suggest else {
            return;
        };
        if state.typed != query {
            return;
        }
        state.remote = found;
        state.remote_for = query;
        self.rebuild_suggestions(cx);
    }

    /// The rows again for what's typed, without completing again.
    fn rebuild_suggestions(&mut self, cx: &mut Context<Self>) {
        let Some(typed) = self.suggest.as_ref().map(|s| s.typed.clone()) else {
            return;
        };
        let (_, local) = self.local_matches(&typed);
        let rows = self.rows_for(&typed, local);
        if let Some(state) = &mut self.suggest {
            state.rows = rows;
            state.highlight = state.highlight.filter(|&h| h < state.rows.len());
        }
        cx.notify();
    }

    /// Takes a page out of history from its row (the × or ⇧⌫); bookmarks
    /// stay, being bookmarks.
    pub(crate) fn remove_suggestion(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let Some(state) = &self.suggest else {
            return;
        };
        let Some(index) = index.or(state.highlight) else {
            return;
        };
        let Some(Suggestion::Page { url, bookmark: false, .. }) = state.rows.get(index).cloned() else {
            return;
        };
        let typed = state.typed.clone();
        // Every visit to that page, and any to the site's front page it
        // stood for.
        {
            let mut history = self.history();
            history.remove(&url);
            history.remove(url.trim_end_matches('/'));
            history.save();
        }
        // The field showed that page (completed, or picked with the arrows):
        // it goes back to what was typed.
        let shown = self.address.read(cx).content.to_string();
        let showed_it = shown != typed
            && (shown == url
                || bare(&url)
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(shown.trim_end_matches('/')));
        if showed_it {
            self.address.update(cx, |input, cx| input.set_text(&typed, cx));
        }
        if let Some(state) = &mut self.suggest {
            state.highlight = match state.highlight {
                _ if showed_it => None,
                Some(h) if h > index => Some(h - 1),
                Some(h) if h == index => None,
                other => other,
            };
        }
        self.rebuild_suggestions(cx);
        self.refresh_other_windows(cx);
    }

    /// Asks for a still of the page, so the list can be drawn over it.
    fn cover_page_for_suggestions(&mut self) {
        let Some(state) = &mut self.suggest else {
            return;
        };
        if state.snapshot_request.is_some() {
            return;
        }
        let request = std::time::Instant::now();
        state.snapshot_request = Some(request);
        let tab = &self.tabs[self.selected];
        if let (Some(view), Page::Web) = (&tab.view, tab.page) {
            let sender = self.sender.clone();
            let id = tab.id;
            native::snapshot(&view.webview(), move |jpeg| {
                let _ = sender.try_send(BrowserEvent::SuggestSnapshot(id, request, jpeg));
            });
        }
    }

    pub(crate) fn suggest_snapshot(&mut self, tab: u64, request: std::time::Instant, jpeg: Option<Vec<u8>>, cx: &mut Context<Self>) {
        if self.current().id != tab {
            return;
        }
        let Some(state) = &mut self.suggest else {
            return;
        };
        if state.snapshot_request != Some(request) {
            return;
        }
        state.snapshot = jpeg.map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes)));
        state.covered = true;
        if state.snapshot.is_some()
            && let Some(view) = &self.tabs[self.selected].view
        {
            let _ = view.set_visible(false);
        }
        cx.notify();
    }

    pub(crate) fn close_suggestions(&mut self, cx: &mut Context<Self>) {
        if self.suggest.take().is_none() {
            return;
        }
        self.show_page_if_uncovered();
        cx.notify();
    }

    pub(crate) fn suggestions_open(&self) -> bool {
        self.suggest.is_some()
    }

    /// Whether ⇧⌫ has a page to take out of history: one picked with the
    /// arrow keys, not a bookmark.
    pub(crate) fn suggestion_removable(&self) -> bool {
        self.suggest.as_ref().is_some_and(|state| {
            state.picked
                && state
                    .highlight
                    .and_then(|index| state.rows.get(index))
                    .is_some_and(|row| matches!(row, Suggestion::Page { bookmark: false, .. }))
        })
    }

    /// The arrow keys: walk the rows, showing each in the field; above the
    /// first is what was typed.
    pub(crate) fn move_suggestion(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(state) = &mut self.suggest else {
            return;
        };
        let count = state.rows.len() as isize;
        if count == 0 {
            return;
        }
        // -1 stands for the typed text.
        let current = state.highlight.map_or(-1, |h| h as isize);
        let next = (current + by).clamp(-1, count - 1);
        state.highlight = (next >= 0).then_some(next as usize);
        state.picked = state.highlight.is_some();
        let text = match state.highlight {
            Some(index) => state.rows[index].fill().to_owned(),
            None => state.typed.clone(),
        };
        self.address.update(cx, |input, cx| input.set_text(&text, cx));
        cx.notify();
    }

    fn choose_suggestion(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(text) = self.suggest.as_ref().and_then(|s| s.rows.get(index)).map(|row| row.fill().to_owned()) else {
            return;
        };
        let _ = self.sender.try_send(BrowserEvent::Address(text));
        self.close_suggestions(cx);
    }

    /// The still of the page while the list covers it, in place of the page.
    pub(crate) fn suggestion_backdrop(&self) -> Option<AnyElement> {
        let snapshot = self.suggest.as_ref()?.snapshot.clone()?;
        Some(img(snapshot).absolute().top_0().left_0().size_full().into_any_element())
    }

    /// The list, hung from the address field's `anchor`.
    pub(crate) fn suggestion_list(
        &mut self,
        anchor: Bounds<Pixels>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let chrome = Chrome::new(palette);
        let state = self.suggest.as_ref()?;
        if state.rows.is_empty() {
            return None;
        }
        // Over a web page, wait for its still: until then the page would
        // hide whatever part of the list lies over it.
        let tab = self.current();
        if tab.page == Page::Web && tab.view.is_some() && !state.covered {
            return None;
        }
        let engine = settings::engine(&self.settings.search_engine).map_or("Search", |e| e.name);
        let highlight = state.highlight;
        let typed = state.typed.clone();
        let mut list = div().flex().flex_col().py(px(6.0));
        let mut last_section = "";
        for (index, row) in state.rows.iter().enumerate() {
            let section = match row {
                Suggestion::Page { .. } => "History and Bookmarks",
                Suggestion::Search(_) => "search",
            };
            if section != last_section {
                if !last_section.is_empty() {
                    list = list.child(div().mx(px(14.0)).my(px(5.0)).h(px(1.0)).bg(chrome.line));
                }
                let title: SharedString = if section == "search" {
                    format!("{engine} Suggestions").into()
                } else {
                    section.into()
                };
                list = list.child(
                    div()
                        .px(px(16.0))
                        .pt(px(4.0))
                        .pb(px(4.0))
                        .text_size(px(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(palette.text_secondary)
                        .child(title),
                );
                last_section = section;
            }
            let (leading, title, detail): (AnyElement, String, Option<&str>) = match row {
                Suggestion::Page { url, title, .. } => (
                    site_icon(self.shown_favicon(url), url, 18.0, false, palette),
                    if title.trim().is_empty() { bare(url).to_owned() } else { title.clone() },
                    Some(bare(url).trim_end_matches('/')),
                ),
                Suggestion::Search(query) => (
                    div()
                        .size(px(18.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(Icon::Search, 13.0, palette.text_secondary))
                        .into_any_element(),
                    query.clone(),
                    None,
                ),
            };
            let bookmark = matches!(row, Suggestion::Page { bookmark: true, .. });
            let removable = matches!(row, Suggestion::Page { bookmark: false, .. });
            let selected = highlight == Some(index);
            list = list.child(
                div()
                    .id(("suggestion", index))
                    .group("suggestion-row")
                    .mx(px(6.0))
                    .h(px(if detail.is_some() { 40.0 } else { 32.0 }))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(10.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .when(selected, |el| el.bg(palette.soft_fill))
                    .when(!selected, |el| el.hover(move |s| s.bg(chrome.wash)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.choose_suggestion(index, cx);
                        }),
                    )
                    .child(leading)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .items_baseline()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(gpui::relative(0.6))
                                    .truncate()
                                    .text_color(palette.text_primary)
                                    .child(emphasized(title, &typed)),
                            )
                            .when_some(detail, |el, detail| {
                                el.child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .truncate()
                                        .text_size(px(12.0))
                                        .text_color(palette.text_secondary)
                                        .child(emphasized(format!("— {detail}"), &typed)),
                                )
                            }),
                    )
                    .when(bookmark, |el| el.child(icon(Icon::Bookmark, 12.0, palette.text_secondary)))
                    // Out of history, from the pointer or the keyboard (⇧⌫).
                    .when(removable, |el| {
                        el.child(
                            div()
                                .id(("suggestion-remove", index))
                                .size(px(22.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.0))
                                .opacity(if selected { 1.0 } else { 0.0 })
                                .group_hover("suggestion-row", |s| s.opacity(1.0))
                                .hover(move |s| s.bg(chrome.wash))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        window.prevent_default();
                                        cx.stop_propagation();
                                        this.remove_suggestion(Some(index), cx);
                                    }),
                                )
                                .child(icon(Icon::Close, 10.0, palette.text_secondary)),
                        )
                    }),
            );
        }
        Some(
            div()
                .absolute()
                .top(anchor.origin.y + anchor.size.height + px(6.0))
                .left(anchor.origin.x)
                .w(anchor.size.width)
                .child(
                    div()
                        .id("suggestions")
                        .occlude()
                        .flex()
                        .flex_col()
                        .rounded(px(12.0))
                        .bg(chrome.raised)
                        .border_1()
                        .border_color(color::with_alpha(palette.field_border_strong, 0.5))
                        .shadow(lighting::panel(palette.is_dark))
                        .overflow_hidden()
                        .child(list),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_suggestion_shape() {
        assert_eq!(parse(r#"["rus",["rust","russia"]]"#), ["rust", "russia"]);
        assert_eq!(parse(r#"[{"phrase":"rust"},{"phrase":"rusty"}]"#), ["rust", "rusty"]);
        assert_eq!(
            parse(r#"{"status":"success","data":{"items":[{"value":"rust"}]}}"#),
            ["rust"]
        );
        assert!(parse("not json").is_empty());
    }

    #[test]
    fn bare_addresses_drop_scheme_and_www() {
        assert_eq!(bare("https://www.youtube.com/watch"), "youtube.com/watch");
        assert_eq!(bare("http://example.org"), "example.org");
    }

    #[test]
    fn fuzzy_pages_rank_title_and_host_prefixes() {
        assert!(page_score("hack", "Hacker News", "https://news.ycombinator.com/")
            > page_score("hack", "Unrelated", "https://example.org/a-hack"));
        assert!(page_score("hn", "Hacker News", "https://news.ycombinator.com/").is_some());
        assert!(page_score("hacker news", "Hacker News", "https://news.ycombinator.com/").is_some());
        assert!(page_score("xyz", "Hacker News", "https://news.ycombinator.com/").is_none());
    }

    #[test]
    fn highlights_substrings_and_fuzzy_letters() {
        assert_eq!(matched_ranges("Hacker News", "hack"), vec![0..4]);
        let title = "Topcoat is pushing the boundary with Rust";
        let marked: String = matched_ranges(title, "tw")
            .iter().map(|range| &title[range.clone()]).collect();
        assert_eq!(marked.to_lowercase(), "tw");
        assert_eq!(matched_ranges("Café", "fé"), vec![2..5]);
    }
}
