//! Pages visited, for the start page, the tab switcher's search and the
//! settings' history list. One entry per URL, most recent first, saved in
//! `~/Library/Application Support/Vamprowser/history.json`.

use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::HashSet};

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
}

impl Visit {
    fn relower(&mut self) {
        let mut lower = std::mem::take(&mut self.lower);
        haystack_into(&mut lower, &self.title, &self.url);
        self.lower = lower;
    }
}

/// "title url" lowercased into `buffer`, replacing what it held: the text
/// [`matches_words`] looks in. Reusing one buffer spares an allocation per
/// entry when matching a whole list.
pub fn haystack_into(buffer: &mut String, title: &str, url: &str) {
    buffer.clear();
    buffer.extend(title.chars().flat_map(char::to_lowercase));
    buffer.push(' ');
    buffer.extend(url.chars().flat_map(char::to_lowercase));
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

    /// Writes the file if anything changed since the last save.
    pub fn save(&mut self) {
        if !self.dirty || cfg!(test) {
            return;
        }
        let Some(path) = history_path() else {
            return;
        };
        if let Ok(data) = serde_json::to_vec(&self.entries)
            && write_atomic(&path, &data).is_ok()
        {
            self.dirty = false;
        }
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
}
