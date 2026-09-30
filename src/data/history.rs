//! Pages visited, for the start page, the tab switcher's search and the
//! settings' history list. One entry per URL, most recent first, saved in
//! `~/Library/Application Support/Vamprowser/history.json`.

use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::HashSet,
    sync::{Arc, atomic::{AtomicBool, Ordering}},
};

use crate::state::{data_path, now_secs, write_atomic};

/// Older entries fall off the end past this many.
const MAX_ENTRIES: usize = 5000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visit {
    pub url: String,
    pub title: String,
    /// Seconds since the Unix epoch.
    pub last_visit: u64,
    pub visits: u32,
    /// "title url" lowercased, kept so searching as you type doesn't
    /// lowercase thousands of entries on every key.
    #[serde(skip)]
    lower: String,
    #[serde(skip)]
    lower_title_end: usize,
}

impl Visit {
    fn relower(&mut self) {
        let mut lower = std::mem::take(&mut self.lower);
        self.lower_title_end = haystack_into(&mut lower, &self.title, &self.url);
        self.lower = lower;
    }

    /// Cached title and URL fields for fuzzy address-field/switcher scoring.
    pub(crate) fn search_fields(&self) -> (&str, &str) {
        let prefix = self.url.len() - bare_address(&self.url).len();
        (
            &self.lower[..self.lower_title_end],
            &self.lower[self.lower_title_end + 1 + prefix..],
        )
    }
}

/// An address without the scheme and `www.`, as suggestions complete it.
pub(crate) fn bare_address(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.strip_prefix("www.").unwrap_or(rest)
}

/// "title url" lowercased into `buffer`, replacing what it held: the text
/// [`matches_words`] looks in. Reusing one buffer spares an allocation per
/// entry when matching a whole list. Returns where the title ends.
pub fn haystack_into(buffer: &mut String, title: &str, url: &str) -> usize {
    buffer.clear();
    if title.is_ascii() && url.is_ascii() {
        buffer.push_str(title);
        buffer.push(' ');
        buffer.push_str(url);
        buffer.make_ascii_lowercase();
        return title.len();
    }
    buffer.push_str(&title.to_lowercase());
    let title_end = buffer.len();
    buffer.push(' ');
    buffer.push_str(&url.to_lowercase());
    title_end
}

/// Whether `haystack_lower` (already lowercased) contains every one of
/// `words` (lowercased too): how history, bookmarks and tabs are matched
/// against what's typed.
pub fn matches_words(haystack_lower: &str, words: &[&str]) -> bool {
    words.iter().all(|w| haystack_lower.contains(w))
}

#[derive(Default)]
pub struct History {
    entries: Vec<Visit>,
    dirty: bool,
    /// Bumped on every change, so views built from the history can tell
    /// when theirs is stale.
    revision: u64,
    /// The last [`History::frequent`] answer: (revision, count, indices
    /// into `entries`). The start page asks for it on every frame.
    frequent: RefCell<Option<(u64, usize, Vec<usize>)>>,
    writer: Option<crate::background::LatestWorker<Vec<Visit>>>,
    save_failed: Arc<AtomicBool>,
}

fn history_path() -> Option<std::path::PathBuf> {
    data_path("history.json")
}

impl History {
    pub fn load() -> Self {
        let mut entries: Vec<Visit> = crate::state::load_json(history_path()).unwrap_or_default();
        for visit in &mut entries {
            visit.relower();
        }
        Self {
            entries,
            ..Self::default()
        }
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.revision += 1;
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Writes the file if anything changed since the last save.
    pub fn save(&mut self) {
        if cfg!(test) {
            return;
        }
        // Quit, export and explicit removals need a durable file. Wait for
        // older periodic writes first so they cannot resurrect removed rows.
        if let Some(writer) = &self.writer {
            writer.flush();
        }
        if !self.dirty && !self.save_failed.load(Ordering::Relaxed) {
            return;
        }
        let Some(path) = history_path() else {
            return;
        };
        if let Ok(data) = serde_json::to_vec(&self.entries)
            && write_atomic(&path, &data).is_ok()
        {
            self.dirty = false;
            self.save_failed.store(false, Ordering::Relaxed);
        }
    }

    /// Periodic serialization and disk work must not interrupt scrolling,
    /// typing or WebKit callbacks on the foreground thread.
    pub(crate) fn save_periodically(&mut self) {
        if cfg!(test) || (!self.dirty && !self.save_failed.load(Ordering::Relaxed)) {
            return;
        }
        let Some(path) = history_path() else { return };
        let writer = self.writer.get_or_insert_with(|| {
            let failed = self.save_failed.clone();
            crate::background::LatestWorker::new("history-writer", move |entries: Vec<Visit>| {
                let saved = serde_json::to_vec(&entries)
                    .is_ok_and(|bytes| write_atomic(&path, &bytes).is_ok());
                failed.store(!saved, Ordering::Relaxed);
                if !saved {
                    eprintln!("Could not save browsing history");
                }
            })
        });
        // Leave the search cache on the main thread: the file stores only
        // these fields, so duplicating every lowercased string is unnecessary.
        let entries = self.entries.iter().map(|visit| Visit {
            url: visit.url.clone(),
            title: visit.title.clone(),
            last_visit: visit.last_visit,
            visits: visit.visits,
            lower: String::new(),
            lower_title_end: 0,
        }).collect();
        writer.submit(entries);
        self.dirty = false;
    }

    /// Records a visit, moving the page to the top.
    pub fn record(&mut self, url: &str, title: &str) {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return;
        }
        let mut visit = match self.entries.iter().position(|v| v.url == url) {
            Some(at) => self.entries.remove(at),
            None => Visit {
                url: url.to_owned(),
                title: String::new(),
                last_visit: 0,
                visits: 0,
                lower: String::new(),
                lower_title_end: 0,
            },
        };
        visit.visits += 1;
        visit.last_visit = now_secs();
        if !title.trim().is_empty() {
            visit.title = title.to_owned();
        }
        visit.relower();
        self.entries.insert(0, visit);
        self.entries.truncate(MAX_ENTRIES);
        self.changed();
    }

    /// A title arriving after the load it belongs to.
    pub fn retitle(&mut self, url: &str, title: &str) {
        if let Some(visit) = self.entries.iter_mut().find(|v| v.url == url)
            && !title.trim().is_empty()
            && visit.title != title
        {
            visit.title = title.to_owned();
            visit.relower();
            self.changed();
        }
    }

    pub fn recent(&self, count: usize) -> &[Visit] {
        &self.entries[..count.min(self.entries.len())]
    }

    /// The most visited pages, one per site so a single site can't fill it.
    /// Remembered until the history changes.
    pub fn frequent(&self, count: usize) -> Vec<&Visit> {
        let mut cache = self.frequent.borrow_mut();
        let fresh = cache
            .as_ref()
            .is_some_and(|(revision, asked, _)| *revision == self.revision && *asked == count);
        if !fresh {
            let mut by_visits: Vec<usize> = (0..self.entries.len()).collect();
            by_visits.sort_by(|&a, &b| {
                let (a, b) = (&self.entries[a], &self.entries[b]);
                b.visits
                    .cmp(&a.visits)
                    .then(b.last_visit.cmp(&a.last_visit))
            });
            let mut sites = HashSet::new();
            let picked = by_visits
                .into_iter()
                .filter(|&i| sites.insert(crate::favicon::site_key(&self.entries[i].url)))
                .take(count)
                .collect();
            *cache = Some((self.revision, count, picked));
        }
        let (_, _, picked) = cache.as_ref().expect("filled above");
        picked.iter().map(|&i| &self.entries[i]).collect()
    }

    /// Entries whose title or URL contain every word of `query`.
    pub fn search(&self, query: &str, count: usize) -> Vec<&Visit> {
        let lower = query.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        self.entries
            .iter()
            .filter(|v| matches_words(&v.lower, &words))
            .take(count)
            .collect()
    }

    pub fn remove(&mut self, url: &str) {
        let before = self.entries.len();
        self.entries.retain(|v| v.url != url);
        if self.entries.len() != before {
            self.changed();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.changed();
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisits_move_to_the_top_and_count() {
        let mut history = History::default();
        history.record("https://a.org/", "A");
        history.record("https://b.org/", "B");
        history.record("https://a.org/", "");
        let recent = history.recent(10);
        assert_eq!(recent[0].url, "https://a.org/");
        assert_eq!(recent[0].visits, 2);
        assert_eq!(recent[0].title, "A");
        assert_eq!(history.recent(usize::MAX).len(), 2);
        history.record("vamp://settings", "Settings");
        assert_eq!(history.recent(usize::MAX).len(), 2);
    }

    #[test]
    fn search_matches_every_word_and_frequent_is_one_per_site() {
        let mut history = History::default();
        history.record("https://docs.rs/gpui", "gpui - Rust");
        history.record("https://docs.rs/wry", "wry - Rust");
        history.record("https://docs.rs/wry", "wry - Rust");
        history.record("https://example.org/", "Example");
        assert_eq!(history.search("rust wry", 10).len(), 1);
        let frequent = history.frequent(10);
        assert_eq!(frequent.len(), 2);
        assert_eq!(frequent[0].url, "https://docs.rs/wry");
        // Remembered, but not past a change.
        assert_eq!(history.frequent(10).len(), 2);
        history.record("https://other.net/", "Other");
        assert_eq!(history.frequent(10).len(), 3);
        assert_eq!(history.frequent(1).len(), 1);
    }

    #[test]
    fn search_ignores_case_and_follows_new_titles() {
        let mut history = History::default();
        history.record("https://example.org/", "");
        assert!(history.search("Example.ORG", 10).len() == 1);
        assert!(history.search("welcome", 10).is_empty());
        history.retitle("https://example.org/", "Welcome Home");
        assert_eq!(history.search("home WELCOME", 10).len(), 1);
        assert!(matches_words("a b", &[]));
    }

    #[test]
    fn cached_scoring_fields_preserve_unicode_lowercase_and_title_changes() {
        let mut history = History::default();
        history.record("https://www.example.org/İ", "ΟΣ İ");
        let visit = &history.recent(1)[0];
        assert_eq!(visit.search_fields(), ("ος i\u{307}", "example.org/i\u{307}"));
        assert_eq!(history.search("ος", 1).len(), 1);
        history.retitle("https://www.example.org/İ", "New title");
        assert_eq!(history.recent(1)[0].search_fields().0, "new title");
    }
}
