//! Address field suggestions: as you type, the field completes the site
//! you most likely mean, and a list under it offers matching history and
//! bookmarks (history can be removed from there), then searches: what you
//! typed, and your search engine's suggestions, ahead of loose fuzzy matches.
//! Like the tab switcher, the list covers the page with a still
//! of it, because GPUI can't draw over a live WebKit view.

use std::{
    collections::HashMap,
    ops::Range,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui::{
    Animation, AnimationExt, AnyElement, Bounds, Context, FontWeight,
    MouseButton, Pixels, SharedString, StyledText, Task, Window, div, img, prelude::*, px,
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
const ROWS_PER_SITE: usize = 2;
const REMOTE_DELAY: Duration = Duration::from_millis(180);
const DDG_ENDPOINT: &str = "https://duckduckgo.com/ac/?q=%s&type=list";

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
    /// Keep local matches when only the network suggestions change.
    local: Vec<Suggestion>,
    remote_worker: Option<RemoteWorker>,
    /// Replacing this task cancels the previous debounce timer.
    remote_later: Option<Task<()>>,
    rows: Vec<Suggestion>,
    highlight: Option<usize>,
    /// The highlight was moved there with the arrow keys, rather than being
    /// the completion the field shows.
    picked: bool,
    /// A still of the page it covers, which is hidden meanwhile.
    snapshot: Option<native::Still>,
    snapshot_request: Option<std::time::Instant>,
    /// The page's still has come (or couldn't be had): the list can show
    /// without half of it hidden behind the live page.
    covered: bool,
}

/// An address without its scheme and `www.`, as it's matched against what's
/// typed and completed.
fn bare(url: &str) -> &str {
    crate::history::bare_address(url)
}

/// Relevance before visit frequency: a word at the start of a title or
/// hostname should beat an unrelated page that happens to contain the same
/// letters in a long path. A subsequence still finds abbreviated names.
fn field_score(word: &str, field: &str) -> Option<i32> {
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
    fuzzy_score_lower(word, field).map(|score| (35 + score).clamp(1, 65))
}

/// `vampir::fuzzy_score` for a query and a candidate already lowercased:
/// the same subsequence scoring, without lowercasing (and allocating) both
/// again for every field of every page on every keystroke.
fn fuzzy_score_lower(query: &str, candidate: &str) -> Option<i32> {
    if query.chars().all(char::is_whitespace) {
        return Some(0);
    }
    let mut haystack = candidate.chars().enumerate();
    let mut score = 0i32;
    let mut last_index: Option<usize> = None;
    let mut previous = None;
    for needle in query.chars() {
        if needle.is_whitespace() {
            continue;
        }
        loop {
            let (index, ch) = haystack.next()?;
            let starts_word = previous.is_none_or(|previous: char| {
                previous.is_whitespace() || previous == '-' || previous == '_'
            });
            previous = Some(ch);
            if ch != needle {
                continue;
            }
            // Starting a word is the strongest signal that this is the
            // match the person meant.
            score += if starts_word { 12 } else { 2 };
            if last_index == Some(index.saturating_sub(1)) {
                score += 6;
            }
            last_index = Some(index);
            break;
        }
    }
    // Among equally good matches, the shortest candidate is the one that
    // most nearly *is* the query.
    Some(score - (candidate.chars().count() as i32 / 8))
}

#[cfg(test)]
fn page_score(query: &str, title: &str, url: &str) -> Option<i32> {
    // Fold each field once per page, rather than again for every query word.
    let title = title.to_lowercase();
    let address = bare(url).to_lowercase();
    page_score_fields(query, &title, &address)
}

pub(crate) fn page_score_fields(query: &str, title: &str, address: &str) -> Option<i32> {
    query.split_whitespace().try_fold(0, |total, word| {
        let title = field_score(word, title);
        // Address scores carry an eight-point penalty. A title prefix
        // already wins, and a title word boundary only loses to an address
        // prefix; avoid the slower address/fuzzy scan in those cases.
        if title == Some(120) {
            return Some(total + 120);
        }
        if title == Some(105) {
            return Some(total + if address.starts_with(word) { 112 } else { 105 });
        }
        let address = field_score(word, address).map(|score| score - 8);
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
    let ddg = DDG_ENDPOINT;
    let instance = |id: &str, path: &str| {
        settings
            .instances
            .get(id)
            .map(|base| base.trim().trim_end_matches('/'))
            .filter(|base| !base.is_empty())
            .map(|base| format!("{base}{path}"))
    };
    match engine {
        "duckduckgo" | "duckduckgo_lite" | "duckduckgo_html" => ddg.into(),
        "google" | "google_web" => {
            "https://suggestqueries.google.com/complete/search?client=firefox&q=%s".into()
        }
        "youtube" => {
            "https://suggestqueries.google.com/complete/search?client=firefox&ds=yt&q=%s".into()
        }
        "bing" => "https://api.bing.com/osjson.aspx?query=%s".into(),
        "kagi" => "https://kagisuggest.com/api/autosuggest?q=%s".into(),
        "startpage" => "https://www.startpage.com/osuggestions?q=%s".into(),
        "ecosia" => "https://ac.ecosia.org/autocomplete?q=%s&type=list".into(),
        "yahoo" => "https://search.yahoo.com/sugg/os?command=%s&output=fxjson".into(),
        "yandex" => "https://suggest.yandex.com/suggest-ff.cgi?part=%s".into(),
        "qwant" => "https://api.qwant.com/v3/suggest?q=%s&locale=en_US&version=2".into(),
        "baidu" => "https://www.baidu.com/sugrec?ie=utf-8&json=1&prod=pc&wd=%s".into(),
        "naver" => "https://ac.search.naver.com/nx/ac?q=%s&r_format=json&st=0".into(),
        "seznam" => {
            "https://suggest.seznam.cz/fulltext/cs?phrase=%s&format=json-2&highlight=0&count=6".into()
        }
        "swisscows" => "https://api.swisscows.com/suggest?query=%s".into(),
        "marginalia" => "https://marginalia-search.com/suggest/?partial=%s".into(),
        // Reference sites expose search APIs rather than query completion:
        // document titles and crate names make useful searches on those sites.
        "mdn" => "https://developer.mozilla.org/api/v1/search?q=%s&locale=en-US".into(),
        "crates" | "docs_rs" => "https://crates.io/api/v1/crates?q=%s&per_page=5".into(),
        "wikipedia" => {
            "https://en.wikipedia.org/w/api.php?action=opensearch&format=json&search=%s".into()
        }
        "searxng" => instance("searxng", "/autocompleter?q=%s").unwrap_or_else(|| ddg.into()),
        "whoogle" => instance("whoogle", "/autocomplete?q=%s").unwrap_or_else(|| ddg.into()),
        "fourget" => instance("fourget", "/api/v1/ac.php?s=%s&scraper=ddg").unwrap_or_else(|| ddg.into()),
        "yacy" => instance("yacy", "/suggest.json?query=%s&count=5").unwrap_or_else(|| ddg.into()),
        // No usable anonymous completion API was found for these providers.
        // Keep the documented fallback instead of calling obsolete, paid,
        // authenticated, or autocomplete-prohibited services.
        "mojeek" | "metager" | "presearch" | "ghostery" | "stract"
        | "yep" | "perplexity" | "you" | "phind" | "wiby" | "github" | "stackoverflow"
        | "openstreetmap" | "librey" | "custom" => ddg.into(),
        _ => ddg.into(),
    }
}

/// Reads suggestions from the shapes services answer in: OpenSearch's
/// OpenSearch tuples, plain lists, and provider-specific JSON records.
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
        if value.as_array().is_some_and(|items| items.iter().all(|item| item.is_string())) {
            return strings(&value, None);
        }
        return strings(&value, Some("phrase"));
    }
    if value.get("g").is_some() {
        return strings(&value["g"], Some("q"));
    }
    if let Some(items) = value["items"][0].as_array() {
        return items.iter().filter_map(|item| item[0].as_str()).map(str::to_owned).collect();
    }
    if let Some(items) = value["result"].as_array() {
        return items.iter()
            .filter(|item| item["itemType"] == "ItemType.TEXT")
            .filter_map(|item| {
                let parts = item["text"].as_array()?;
                let text: String = parts.iter().filter_map(|part| part["text"].as_str()).collect();
                (!text.is_empty()).then_some(text)
            })
            .collect();
    }
    if value.get("documents").is_some() {
        return strings(&value["documents"], Some("title"));
    }
    if value.get("crates").is_some() {
        return strings(&value["crates"], Some("name"));
    }
    strings(&value["data"]["items"], Some("value"))
}

fn fetch(agent: &ureq::Agent, template: &str, query: &str) -> Vec<String> {
    let url = template.replace("%s", &settings::encode(query));
    agent
        .get(&url)
        .call()
        .ok()
        .and_then(|mut response| response.body_mut().read_to_string().ok())
        .map(|body| parse(&body))
        .unwrap_or_default()
}

/// A suggestion session has one worker and one reusable HTTP client.
/// While a request is in flight, later edits replace queued work; typing
/// quickly never creates one thread and connection pool per character.
struct RemoteWorker {
    requests: Rc<crate::background::LatestWorker<(String, String)>>,
    cancelled: Arc<AtomicBool>,
}

impl RemoteWorker {
    fn new(sender: async_channel::Sender<BrowserEvent>) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let mut agent = None;
        let requests = Rc::new(crate::background::LatestWorker::new(
            "search-suggestions",
            move |(template, query): (String, String)| {
                if worker_cancelled.load(Ordering::Acquire) {
                    return;
                }
                let agent = agent.get_or_insert_with(|| {
                    ureq::Agent::config_builder()
                        .timeout_global(Some(Duration::from_secs(3)))
                        .user_agent(concat!("Vamprowser/", env!("CARGO_PKG_VERSION"), " (https://github.com/404oops/vamprowser)"))
                        .build()
                        .into()
                });
                let found = fetch(agent, &template, &query);
                if !worker_cancelled.load(Ordering::Acquire) {
                    let _ = sender.try_send(BrowserEvent::Suggestions(query, found));
                }
            },
        ));
        Self {
            requests,
            cancelled,
        }
    }
}

impl Drop for RemoteWorker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// A host prefix or a verbatim title indicates navigation. Merely containing
/// the query in a page title should leave the search action first.
fn navigation_match(query: &str, title: &str, address: &str) -> bool {
    title.trim() == query
        || (!query.contains(char::is_whitespace) && address.starts_with(query))
}

fn site(address: &str) -> &str {
    address.split(['/', '?', '#']).next().unwrap_or(address)
}

/// Keep fuzzy abbreviations for short names, but require literal query words
/// for phrases so scattered letters in long titles don't fill the list.
fn address_page_score(query: &str, title: &str, address: &str) -> Option<i32> {
    if title.trim() == query {
        return page_score_fields(query, title, address).map(|score| score + 240);
    }
    if query.split_whitespace().count() > 1 {
        query.split_whitespace().try_fold(0, |total, word| {
            let title = field_score(word, title).filter(|score| *score >= 75);
            let address = field_score(word, address)
                .filter(|score| *score >= 75).map(|score| score - 8);
            Some(total + title.into_iter().chain(address).max()?)
        })
    } else {
        page_score_fields(query, title, address)
    }
}

fn local_matches(
    typed: &str,
    bookmarks: &crate::bookmarks::Bookmarks,
    history: &crate::history::History,
) -> (Option<(String, String, String)>, Vec<Suggestion>) {
    let lower = typed.trim().to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.is_empty() {
        return (None, Vec::new());
    }
    struct Found<'a> {
        url: &'a str,
        title: &'a str,
        bookmark: bool,
        score: i32,
        visits: u32,
        recent: u64,
        order: usize,
        address: &'a str,
        navigation: bool,
    }
    let mut found: Vec<Found> = Vec::new();
    // Where each address is in `found`, to combine a bookmark and its visits.
    let mut at: HashMap<&str, usize> = HashMap::new();
    fn entry<'a>(
        url: &'a str,
        title: &'a str,
        bookmark: bool,
        score: i32,
        visits: u32,
        recent: u64,
        order: usize,
        address: &'a str,
        navigation: bool,
    ) -> Found<'a> {
        Found {
            url,
            title,
            bookmark,
            score,
            visits,
            recent,
            order,
            address,
            navigation,
        }
    }
    let bookmark_fields = bookmarks.scoring_fields();
    for (bookmark, (title_lower, address_lower)) in
        bookmarks.links().into_iter().zip(bookmark_fields.iter())
    {
        let Some(url) = bookmark.url.as_deref() else {
            continue;
        };
        if let Some(score) = address_page_score(&lower, title_lower, address_lower)
            && !at.contains_key(url)
        {
            at.insert(url, found.len());
            found.push(entry(
                url,
                &bookmark.title,
                true,
                score,
                0,
                0,
                found.len(),
                address_lower,
                navigation_match(&lower, title_lower, address_lower),
            ));
        }
    }
    for visit in history.recent(usize::MAX) {
        let (title_lower, address_lower) = visit.search_fields();
        let Some(score) = address_page_score(&lower, title_lower, address_lower) else {
            continue;
        };
        match at.get(visit.url.as_str()) {
            Some(&index) => {
                let item = &mut found[index];
                if score > item.score {
                    item.title = &visit.title;
                    item.score = score;
                }
                item.navigation |= navigation_match(&lower, title_lower, address_lower);
                item.visits = visit.visits;
                item.recent = visit.last_visit;
            }
            None => {
                at.insert(&visit.url, found.len());
                found.push(entry(
                    &visit.url,
                    &visit.title,
                    false,
                    score,
                    visit.visits,
                    visit.last_visit,
                    found.len(),
                    address_lower,
                    navigation_match(&lower, title_lower, address_lower),
                ));
            }
        }
    }
    let compare = |a: &Found<'_>, b: &Found<'_>| {
        b.navigation.cmp(&a.navigation)
            .then_with(|| page_rank(b.score, b.bookmark, b.visits, b.recent)
                .cmp(&page_rank(a.score, a.bookmark, a.visits, a.recent)))
            .then(a.order.cmp(&b.order))
    };
    let mut site_visits: HashMap<&str, u64> = HashMap::new();
    for entry in &found {
        *site_visits.entry(site(entry.address)).or_default() += u64::from(entry.visits);
    }
    let completion_rank = |entry: &Found<'_>| {
        let host = site(entry.address);
        (
            host == lower || host.split('.').next() == Some(lower.as_str()),
            site_visits.get(host).copied().unwrap_or_default(),
            entry.visits,
        )
    };
    let bookmarked = |url: &str| found.iter().any(|f| f.url == url && f.bookmark);

    // Completing: a single word the start of an address, finishing at
    // the host unless what's typed already reaches into the path.
    let completion = if words.len() == 1 && !typed.contains(char::is_whitespace) {
        found
            .iter()
            .filter(|entry| {
                let address = bare(entry.url);
                let end = if lower.contains('/') {
                    address.len()
                } else {
                    address.find('/').unwrap_or(address.len())
                };
                address[..end].trim_end_matches('/').len() >= typed.len()
                    && entry.address.starts_with(&lower)
            })
            .min_by(|a, b| completion_rank(b).cmp(&completion_rank(a)).then_with(|| compare(a, b)))
            .map(|entry| {
                let address = bare(entry.url);
                let end = if lower.contains('/') {
                    address.len()
                } else {
                    address.find('/').unwrap_or(address.len())
                };
                let full = address[..end].trim_end_matches('/');
                let target = if lower.contains('/') {
                    entry.url.to_owned()
                } else {
                    url::Url::parse(entry.url)
                        .map(|u| match u.scheme() {
                            "http" | "https" => format!("{}/", u.origin().ascii_serialization()),
                            scheme => format!("{scheme}://{}/", u.host_str().unwrap_or_default()),
                        })
                        .unwrap_or_else(|_| entry.url.to_owned())
                };
                (full.to_owned(), target, entry.title.to_owned())
            })
    } else {
        None
    };
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
    found.retain(|entry| {
        completion.as_ref().is_none_or(|(_, target, _)| {
            target.trim_end_matches('/') != entry.url.trim_end_matches('/')
        })
    });
    // Apply the site quota before the row limit: a busy site must not crowd
    // out equally relevant pages from other sites. Count the completion too.
    found.sort_unstable_by(compare);
    let mut site_rows: HashMap<String, usize> = HashMap::new();
    if let Some((_, target, _)) = &completion {
        site_rows.insert(site(&bare(target).to_lowercase()).to_owned(), 1);
    }
    found.retain(|entry| {
        let count = site_rows.entry(site(entry.address).to_owned()).or_default();
        *count += 1;
        *count <= ROWS_PER_SITE
    });
    found.truncate(LOCAL_ROWS.saturating_sub(rows.len()));
    rows.extend(found.into_iter().map(|entry| Suggestion::Page {
        url: entry.url.to_owned(),
        title: entry.title.to_owned(),
        bookmark: entry.bookmark,
    }));
    (completion, rows)
}

/// Strong navigation matches lead; searches precede other history matches.
fn suggestion_rows(typed: &str, local: Vec<Suggestion>, remote: &[String]) -> Vec<Suggestion> {
    let lower = typed.trim().to_lowercase();
    let (mut rows, other): (Vec<_>, Vec<_>) = local.into_iter().partition(|row| {
        let Suggestion::Page { title, url, .. } = row else { return false; };
        navigation_match(&lower, &title.to_lowercase(), &bare(url).to_lowercase())
    });
    rows.push(Suggestion::Search(typed.trim().to_owned()));
    rows.extend(remote.iter()
        .filter(|s| s.to_lowercase() != lower)
        .take(REMOTE_ROWS)
        .cloned()
        .map(Suggestion::Search));
    rows.extend(other);
    rows
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
        local_matches(typed, &self.bookmarks(), &self.history())
    }

    /// Navigation matches, searches for the current query, then other pages.
    fn rows_for(&self, typed: &str, local: Vec<Suggestion>) -> Vec<Suggestion> {
        let remote = self.suggest.as_ref()
            .filter(|state| state.remote_for == typed)
            .map_or(&[][..], |state| state.remote.as_slice());
        suggestion_rows(typed, local, remote)
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
        let rows = self.rows_for(&text, local.clone());
        let state = self.suggest.get_or_insert_with(|| SuggestState {
            typed: String::new(),
            remote: Vec::new(),
            remote_for: String::new(),
            local: Vec::new(),
            remote_worker: None,
            remote_later: None,
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
        state.local = local;
        state.rows = rows;
        state.remote_later = None;
        if remote_wanted && state.remote_for != text {
            let template = endpoint(&self.settings);
            let worker = state
                .remote_worker
                .get_or_insert_with(|| RemoteWorker::new(self.sender.clone()));
            let requests = worker.requests.clone();
            let query = text.clone();
            state.remote_later = Some(cx.spawn(async move |_, cx| {
                cx.background_executor().timer(REMOTE_DELAY).await;
                requests.submit((template, query));
            }));
        } else if !remote_wanted {
            state.remote_worker = None;
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
        let selected = state.highlight.and_then(|index| state.rows.get(index)).cloned();
        state.remote = found;
        state.remote_for = query;
        let typed = state.typed.clone();
        let local = state.local.clone();
        let rows = self.rows_for(&typed, local);
        if let Some(state) = &mut self.suggest {
            state.rows = rows;
            state.highlight = selected.and_then(|row| state.rows.iter().position(|item| *item == row));
        }
        cx.notify();
    }

    /// The rows again for what's typed, without completing again.
    fn rebuild_suggestions(&mut self, cx: &mut Context<Self>) {
        let Some(typed) = self.suggest.as_ref().map(|s| s.typed.clone()) else {
            return;
        };
        let (_, local) = self.local_matches(&typed);
        let rows = self.rows_for(&typed, local.clone());
        if let Some(state) = &mut self.suggest {
            state.local = local;
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
            history.save_in_background();
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

    pub(crate) fn suggest_snapshot(&mut self, tab: u64, request: std::time::Instant, image: Option<Arc<gpui::RenderImage>>, cx: &mut Context<Self>) {
        if self.current().id != tab {
            return;
        }
        let Some(state) = &mut self.suggest else {
            return;
        };
        if state.snapshot_request != Some(request) {
            return;
        }
        state.snapshot = image.map(native::Still::new);
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
        let snapshot = self.suggest.as_ref()?.snapshot.as_ref()?.image();
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
                        .child(list)
                        .with_animation(
                            "suggestions-enter",
                            Animation::new(crate::slowed(Duration::from_millis(160)))
                                .with_easing(vampir::ease_out_cubic),
                            |panel, t| panel.opacity(t).mt(px(5.0 * (1.0 - t))),
                        ),
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
    fn reads_regional_reference_and_plain_list_suggestions() {
        assert_eq!(parse(r#"["rust","rustdesk"]"#), ["rust", "rustdesk"]);
        assert_eq!(parse(r#"{"g":[{"q":"rust语言"},{"q":"rustdesk"},{}]}"#), ["rust语言", "rustdesk"]);
        assert_eq!(parse(r#"{"items":[[["rust"],["rusty 뜻"]]]}"#), ["rust", "rusty 뜻"]);
        assert_eq!(parse(r#"{"result":[{"itemType":"ItemType.TEXT","text":[{"text":"rust"},{"text":" paint"}]},{"itemType":"ItemType.LINK","text":[{"text":"omit"}]}]}"#), ["rust paint"]);
        assert_eq!(parse(r#"{"documents":[{"title":"Rust to WebAssembly"},{}]}"#), ["Rust to WebAssembly"]);
        assert_eq!(parse(r#"{"crates":[{"name":"serde"},{"name":"serde_json"}]}"#), ["serde", "serde_json"]);
        for body in ["{}", "[]", r#"{"items":[null]}"#, r#"{"result":[{}]}"#, r#"{"g":false}"#] {
            assert!(parse(body).is_empty(), "{body}");
        }
    }

    #[test]
    fn every_engine_has_an_explicit_suggestion_source_or_fallback() {
        let native = [
            ("duckduckgo", "duckduckgo.com"), ("duckduckgo_lite", "duckduckgo.com"),
            ("duckduckgo_html", "duckduckgo.com"), ("kagi", "kagisuggest.com"),
            ("startpage", "www.startpage.com"),
            ("ecosia", "ac.ecosia.org"), ("qwant", "api.qwant.com"),
            ("swisscows", "api.swisscows.com"), ("google", "suggestqueries.google.com"),
            ("google_web", "suggestqueries.google.com"), ("bing", "api.bing.com"),
            ("yahoo", "search.yahoo.com"), ("yandex", "suggest.yandex.com"),
            ("baidu", "www.baidu.com"), ("naver", "ac.search.naver.com"),
            ("seznam", "suggest.seznam.cz"), ("marginalia", "marginalia-search.com"),
            ("wikipedia", "en.wikipedia.org"), ("mdn", "developer.mozilla.org"),
            ("crates", "crates.io"), ("docs_rs", "crates.io"),
            ("youtube", "suggestqueries.google.com"),
        ];
        let hosted = ["searxng", "whoogle", "fourget", "yacy"];
        let fallback = ["mojeek", "metager", "presearch", "ghostery", "stract",
            "yep", "perplexity", "you", "phind", "wiby", "github", "stackoverflow",
            "openstreetmap", "librey", "custom"];
        let mut settings = settings::Settings::default();
        for engine in settings::ENGINES {
            settings.search_engine = engine.id.into();
            let template = endpoint(&settings);
            assert_eq!(template.matches("%s").count(), 1, "{}", engine.id);
            if let Some((_, host)) = native.iter().find(|(id, _)| *id == engine.id) {
                assert_eq!(url::Url::parse(&template).unwrap().host_str(), Some(*host), "{}", engine.id);
            } else {
                assert!(hosted.contains(&engine.id) || fallback.contains(&engine.id),
                    "Choose a suggestion source for the new engine: {}", engine.id);
                assert_eq!(template, DDG_ENDPOINT, "{}", engine.id);
            }
        }
    }

    #[test]
    fn self_hosted_suggestions_keep_the_configured_instance_path() {
        let mut settings = settings::Settings::default();
        for (engine, path) in [
            ("searxng", "/autocompleter?q=%s"), ("whoogle", "/autocomplete?q=%s"),
            ("fourget", "/api/v1/ac.php?s=%s&scraper=ddg"), ("yacy", "/suggest.json?query=%s&count=5"),
        ] {
            settings.search_engine = engine.into();
            settings.instances.insert(engine.into(), " https://search.example/base/ ".into());
            assert_eq!(endpoint(&settings), format!("https://search.example/base{path}"));
            settings.instances.insert(engine.into(), "  ".into());
            assert_eq!(endpoint(&settings), DDG_ENDPOINT);
        }
    }

    #[test]
    fn kagi_uses_its_own_suggestion_service() {
        let settings = settings::Settings {
            search_engine: "kagi".into(),
            ..Default::default()
        };
        assert_eq!(endpoint(&settings), "https://kagisuggest.com/api/autosuggest?q=%s");
    }

    #[test]
    fn searches_lead_loose_fuzzy_matches() {
        let bookmarks = crate::bookmarks::Bookmarks::new(vec![
            crate::bookmarks::Node::link("Hacker News", "https://news.ycombinator.com/"),
        ]);
        let (_, local) = local_matches("hn", &bookmarks, &crate::history::History::default());
        assert_eq!(local.len(), 1);
        let rows = suggestion_rows("hn", local, &["hn".into(), "hn jobs".into()]);
        assert_eq!(rows[0].fill(), "hn");
        assert_eq!(rows[1].fill(), "hn jobs");
        assert_eq!(rows[2].fill(), "https://news.ycombinator.com/");
    }

    #[test]
    fn verbatim_titles_lead_searches_and_fuzzy_phrases_are_filtered() {
        let bookmarks = crate::bookmarks::Bookmarks::new(vec![
            crate::bookmarks::Node::link("Rust Docs", "https://rust-lang.org/"),
            crate::bookmarks::Node::link("Rust Download Console", "https://example.org/"),
        ]);
        let (_, local) = local_matches("rust docs", &bookmarks, &crate::history::History::default());
        assert_eq!(local.len(), 1);
        let rows = suggestion_rows("rust docs", local, &["rust docs examples".into()]);
        assert_eq!(rows[0].fill(), "https://rust-lang.org/");
        assert_eq!(rows[1].fill(), "rust docs");
        assert_eq!(rows[2].fill(), "rust docs examples");
    }

    #[test]
    fn ordinary_queries_search_first_and_drop_unrelated_frequent_pages() {
        let bookmarks = crate::bookmarks::Bookmarks::default();
        let mut history = crate::history::History::default();
        for _ in 0..20 {
            history.record("https://x.com/post/1", "what a joke — feedback makes you pure");
        }
        history.record("https://kagi.com/search?q=what+does", "what does the @ mean in python - Kagi Search");
        let (completion, local) = local_matches("what does", &bookmarks, &history);
        assert!(completion.is_none());
        assert_eq!(local.len(), 1);
        let rows = suggestion_rows("what does", local, &["what does larp mean".into()]);
        assert_eq!(rows[0].fill(), "what does");
        assert_eq!(rows[1].fill(), "what does larp mean");
        assert!(rows[2].fill().starts_with("https://kagi.com/"));
    }

    #[test]
    fn exact_site_name_beats_a_more_frequent_prefix() {
        let bookmarks = crate::bookmarks::Bookmarks::default();
        let mut history = crate::history::History::default();
        for _ in 0..20 {
            history.record("https://xbox.com/games", "Xbox");
        }
        history.record("https://x.com/post/1", "A post");
        let (completion, local) = local_matches("x", &bookmarks, &history);
        assert_eq!(completion.unwrap().1, "https://x.com/");
        let rows = suggestion_rows("x", local, &[]);
        assert_eq!(rows[0].fill(), "https://x.com/");
    }

    #[test]
    fn quick_prefix_uses_site_visits_across_pages() {
        let bookmarks = crate::bookmarks::Bookmarks::default();
        let mut history = crate::history::History::default();
        for _ in 0..3 {
            history.record("https://your-site.com/", "Your Site");
        }
        for i in 0..6 {
            history.record(&format!("https://www.youtube.com/watch?v={i}"), "A video");
        }
        let (completion, local) = local_matches("you", &bookmarks, &history);
        assert_eq!(completion.unwrap().1, "https://www.youtube.com/");
        let rows = suggestion_rows("you", local, &[]);
        assert_eq!(rows[0].fill(), "https://www.youtube.com/");
        assert_eq!(rows.iter().filter(|row| row.fill().contains("youtube.com")).count(), ROWS_PER_SITE);
    }

    #[test]
    fn verbatim_title_survives_a_full_list_of_partial_matches() {
        let bookmarks = crate::bookmarks::Bookmarks::default();
        let mut history = crate::history::History::default();
        history.record("https://docs.example/exact", "Rust Docs");
        for i in 0..10 {
            history.record(&format!("https://site{i}.example/"), "Rust Docs and More");
        }
        let (_, local) = local_matches("rust docs", &bookmarks, &history);
        let rows = suggestion_rows("rust docs", local, &[]);
        assert_eq!(rows[0].fill(), "https://docs.example/exact");
        assert_eq!(rows[1].fill(), "rust docs");
    }

    #[test]
    fn site_quota_leaves_room_for_other_relevant_pages() {
        let bookmarks = crate::bookmarks::Bookmarks::default();
        let mut history = crate::history::History::default();
        history.record("https://docs.example/", "Learn Rust");
        for i in 0..20 {
            history.record(&format!("https://x.com/post/{i}"), "Learn Rust");
        }
        let (_, local) = local_matches("learn rust", &bookmarks, &history);
        assert_eq!(local.len(), ROWS_PER_SITE + 1);
        assert!(local.iter().any(|row| row.fill() == "https://docs.example/"));
    }

    #[test]
    fn bare_addresses_drop_scheme_and_www() {
        assert_eq!(bare("https://www.youtube.com/watch"), "youtube.com/watch");
        assert_eq!(bare("http://example.org"), "example.org");
    }

    #[test]
    fn local_ranking_keeps_ties_in_tree_order_and_merges_history() {
        let bookmarks = crate::bookmarks::Bookmarks::new(
            (0..12)
                .map(|i| {
                    crate::bookmarks::Node::link("Rust Docs", format!("https://example{i}.org/"))
                })
                .collect(),
        );
        let mut history = crate::history::History::default();
        history.record("https://example8.org/", "Rust Docs");
        let (completion, rows) = local_matches("rust docs", &bookmarks, &history);
        assert!(completion.is_none());
        let urls: Vec<&str> = rows.iter().map(Suggestion::fill).collect();
        assert_eq!(
            urls,
            [
                "https://example8.org/",
                "https://example0.org/",
                "https://example1.org/",
                "https://example2.org/",
                "https://example3.org/"
            ]
        );
        assert!(
            rows.iter()
                .all(|row| matches!(row, Suggestion::Page { bookmark: true, .. }))
        );
    }

    #[test]
    fn address_completion_finds_prefix_beyond_the_top_local_rows() {
        let mut nodes: Vec<_> = (0..12)
            .map(|i| crate::bookmarks::Node::link("Rust", format!("https://example{i}.org/")))
            .collect();
        nodes.push(crate::bookmarks::Node::link(
            "Documentation",
            "https://rust-lang.org/learn",
        ));
        let bookmarks = crate::bookmarks::Bookmarks::new(nodes);
        let history = crate::history::History::default();
        let (completion, rows) = local_matches("rust", &bookmarks, &history);
        assert_eq!(
            completion,
            Some((
                "rust-lang.org".into(),
                "https://rust-lang.org/".into(),
                "Documentation".into()
            ))
        );
        assert_eq!(rows.len(), LOCAL_ROWS);
        assert_eq!(rows[0].fill(), "https://rust-lang.org/");
        let (completion, rows) = local_matches("rust-lang.org/le", &bookmarks, &history);
        assert_eq!(completion.unwrap().1, "https://rust-lang.org/learn");
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn fuzzy_pages_rank_title_and_host_prefixes() {
        assert!(page_score("hack", "Hacker News", "https://news.ycombinator.com/")
            > page_score("hack", "Unrelated", "https://example.org/a-hack"));
        assert!(page_score("hn", "Hacker News", "https://news.ycombinator.com/").is_some());
        assert!(page_score("hacker news", "Hacker News", "https://news.ycombinator.com/").is_some());
        assert!(page_score("xyz", "Hacker News", "https://news.ycombinator.com/").is_none());
        assert_eq!(
            page_score("rust", "Rust documentation", "https://rust-lang.org/"),
            Some(120)
        );
        assert_eq!(
            page_score("rust", "Learn Rust", "https://rust-lang.org/"),
            Some(112)
        );
        assert_eq!(
            page_score("rust", "Learn Rust", "https://example.org/rust"),
            Some(105)
        );
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
