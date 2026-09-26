//! Everything a person can change, saved in
//! `~/Library/Application Support/Vamprowser/settings.json`, and the search
//! engines and toolbar items the settings choose between.

use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};
use url::Url;

use crate::state::HOME;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

/// Where the window's colour comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TintMode {
    /// The active tab's page, or its icon.
    #[default]
    Page,
    /// One hue, whatever the page.
    Fixed,
    /// Grey.
    Neutral,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Startup {
    /// The tabs open when the browser last quit.
    Restore,
    Home,
    /// The start page; ⌘⇧T brings back the windows open when it quit.
    #[default]
    StartPage,
}

/// What a new tab shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewTabPage {
    /// The browser's own start page: favourites and recent sites.
    #[default]
    StartPage,
    Home,
    Blank,
}

/// Sections shown on the start page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartSection {
    Favorites,
    Frequent,
    Recent,
    Closed,
}

impl StartSection {
    pub const ALL: [Self; 4] = [Self::Favorites, Self::Frequent, Self::Recent, Self::Closed];

    pub fn label(self) -> &'static str {
        match self {
            Self::Favorites => "Favorites",
            Self::Frequent => "Frequently visited",
            Self::Recent => "Recently visited",
            Self::Closed => "Recently closed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StartPageSections {
    pub favorites: bool,
    pub frequent: bool,
    pub recent: bool,
    pub closed: bool,
}

impl Default for StartPageSections {
    fn default() -> Self {
        Self {
            favorites: true,
            frequent: true,
            recent: true,
            closed: true,
        }
    }
}

impl StartPageSections {
    pub fn shown(&self, section: StartSection) -> bool {
        match section {
            StartSection::Favorites => self.favorites,
            StartSection::Frequent => self.frequent,
            StartSection::Recent => self.recent,
            StartSection::Closed => self.closed,
        }
    }

    pub fn toggle(&mut self, section: StartSection) {
        let shown = match section {
            StartSection::Favorites => &mut self.favorites,
            StartSection::Frequent => &mut self.frequent,
            StartSection::Recent => &mut self.recent,
            StartSection::Closed => &mut self.closed,
        };
        *shown = !*shown;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabPlacement {
    #[default]
    End,
    AfterCurrent,
}

/// What happens when a page opens a new window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PopupPolicy {
    #[default]
    NewTab,
    BackgroundTab,
    Block,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protection {
    Off,
    /// Known trackers are blocked when loaded by another site.
    #[default]
    Standard,
    /// Trackers and ad networks everywhere, and every third-party cookie.
    Strict,
}

/// What a site asking for the camera, microphone or screen gets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SitePermission {
    /// WebKit asks, per site.
    #[default]
    Ask,
    Allow,
    Block,
}

impl SitePermission {
    pub const ALL: [SitePermission; 3] = [
        SitePermission::Ask,
        SitePermission::Allow,
        SitePermission::Block,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SitePermission::Ask => "Ask",
            SitePermission::Allow => "Allow",
            SitePermission::Block => "Block",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserAgent {
    #[default]
    Default,
    Safari,
    Chrome,
    Firefox,
    Custom,
}

impl UserAgent {
    pub const ALL: [UserAgent; 5] = [
        UserAgent::Default,
        UserAgent::Safari,
        UserAgent::Chrome,
        UserAgent::Firefox,
        UserAgent::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UserAgent::Default => "Default",
            UserAgent::Safari => "Safari",
            UserAgent::Chrome => "Chrome",
            UserAgent::Firefox => "Firefox",
            UserAgent::Custom => "Custom",
        }
    }

    /// The string sent, or `None` to leave WebKit's own.
    pub fn string(self, custom: &str) -> Option<String> {
        const MAC: &str = "Macintosh; Intel Mac OS X 10_15_7";
        match self {
            UserAgent::Default => None,
            UserAgent::Safari => Some(format!(
                "Mozilla/5.0 ({MAC}) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15"
            )),
            UserAgent::Chrome => Some(format!(
                "Mozilla/5.0 ({MAC}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36"
            )),
            UserAgent::Firefox => Some(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:131.0) Gecko/20100101 Firefox/131.0"
                    .into(),
            ),
            UserAgent::Custom => Some(custom.trim().to_owned()).filter(|s| !s.is_empty()),
        }
    }
}

/// A button the toolbar can carry. `Address` is the address field itself:
/// items before it sit on its left, items after it on its right.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolbarItem {
    Back,
    Forward,
    Reload,
    Home,
    Address,
    NewTab,
    PrivateTab,
    BookmarksBar,
    Sidebar,
    CommandPalette,
    CopyLink,
    Downloads,
    Media,
    Extensions,
    Settings,
}

impl ToolbarItem {
    /// Every item, in the default order.
    pub const ALL: [ToolbarItem; 15] = [
        ToolbarItem::Back,
        ToolbarItem::Forward,
        ToolbarItem::Reload,
        ToolbarItem::Home,
        ToolbarItem::Address,
        ToolbarItem::Extensions,
        ToolbarItem::BookmarksBar,
        ToolbarItem::Sidebar,
        ToolbarItem::NewTab,
        ToolbarItem::PrivateTab,
        ToolbarItem::CommandPalette,
        ToolbarItem::CopyLink,
        ToolbarItem::Downloads,
        ToolbarItem::Media,
        ToolbarItem::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ToolbarItem::Back => "Back",
            ToolbarItem::Forward => "Forward",
            ToolbarItem::Reload => "Reload",
            ToolbarItem::Home => "Home",
            ToolbarItem::Address => "Address field",
            ToolbarItem::NewTab => "New tab",
            ToolbarItem::PrivateTab => "New private tab",
            ToolbarItem::BookmarksBar => "Bookmarks bar",
            ToolbarItem::Sidebar => "Vertical tabs",
            ToolbarItem::CommandPalette => "Switch tabs",
            ToolbarItem::CopyLink => "Copy link",
            ToolbarItem::Downloads => "Downloads",
            ToolbarItem::Media => "Download media",
            ToolbarItem::Extensions => "Extension buttons",
            ToolbarItem::Settings => "Settings",
        }
    }

    fn shown_by_default(self) -> bool {
        matches!(
            self,
            ToolbarItem::Back
                | ToolbarItem::Forward
                | ToolbarItem::Reload
                | ToolbarItem::Address
                | ToolbarItem::Extensions
                | ToolbarItem::BookmarksBar
                | ToolbarItem::Sidebar
                | ToolbarItem::Media
                | ToolbarItem::Settings
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarEntry {
    pub item: ToolbarItem,
    pub shown: bool,
}

fn default_toolbar() -> Vec<ToolbarEntry> {
    ToolbarItem::ALL
        .iter()
        .map(|&item| ToolbarEntry {
            item,
            shown: item.shown_by_default(),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Appearance
    pub scheme: SchemeChoice,
    pub tint: TintMode,
    /// The hue for [`TintMode::Fixed`], in degrees.
    pub fixed_hue: f64,
    /// Scales how much colour the window takes: 0 grey, 1 as sampled, 2 vivid.
    pub intensity: f64,
    pub show_favicons: bool,
    /// Default page zoom, 1 being 100%.
    pub page_zoom: f64,
    pub toolbar: Vec<ToolbarEntry>,

    // Startup and tabs
    /// Saved under a new name: before, restoring the last session was the
    /// default, and a file from then has it whether or not it was chosen.
    #[serde(rename = "on_launch")]
    pub startup: Startup,
    /// The old name's value, read and never written: kept if it's a choice
    /// that wasn't the old default.
    #[serde(rename = "startup", skip_serializing)]
    pub legacy_startup: Option<Startup>,
    pub home_page: String,
    pub new_tab_page: NewTabPage,
    pub start_page_sections: StartPageSections,
    pub tab_placement: TabPlacement,
    /// Minutes a background tab can go unused before its page is unloaded
    /// to save memory (it reloads when shown); 0 keeps every page loaded.
    pub sleep_tabs_after: u32,
    pub popups: PopupPolicy,

    // Search
    pub search_engine: String,
    /// Base URLs for self-hosted engines (SearXNG, Whoogle, …), by engine id.
    pub instances: HashMap<String, String>,
    /// A search URL with `%s` where the query goes.
    pub custom_search: String,

    // Privacy
    pub protection: Protection,
    pub block_third_party_cookies: bool,
    pub https_only: bool,
    pub global_privacy_control: bool,
    pub remember_history: bool,
    /// Sends what's typed in the address field to the search engine for
    /// suggestions.
    pub search_suggestions: bool,
    /// Checks addons.mozilla.org daily for newer versions of extensions.
    pub auto_update_extensions: bool,
    pub javascript: bool,
    pub block_autoplay: bool,
    pub camera: SitePermission,
    pub microphone: SitePermission,
    pub screen_capture: SitePermission,

    // Downloads
    pub download_dir: String,

    // Advanced
    pub user_agent: UserAgent,
    pub custom_user_agent: String,
    pub web_inspector: bool,
    pub swipe_navigation: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scheme: SchemeChoice::System,
            tint: TintMode::Page,
            fixed_hue: 268.0,
            intensity: 1.0,
            show_favicons: true,
            page_zoom: 1.0,
            toolbar: default_toolbar(),
            startup: Startup::StartPage,
            legacy_startup: None,
            home_page: HOME.into(),
            new_tab_page: NewTabPage::StartPage,
            start_page_sections: StartPageSections::default(),
            tab_placement: TabPlacement::End,
            sleep_tabs_after: 60,
            popups: PopupPolicy::NewTab,
            search_engine: "duckduckgo".into(),
            instances: HashMap::new(),
            custom_search: String::new(),
            protection: Protection::Standard,
            block_third_party_cookies: true,
            https_only: false,
            global_privacy_control: true,
            remember_history: true,
            search_suggestions: true,
            auto_update_extensions: true,
            javascript: true,
            block_autoplay: false,
            camera: SitePermission::Ask,
            microphone: SitePermission::Ask,
            screen_capture: SitePermission::Ask,
            download_dir: "~/Downloads".into(),
            user_agent: UserAgent::Default,
            custom_user_agent: String::new(),
            web_inspector: false,
            swipe_navigation: true,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    crate::state::data_path("settings.json")
}

/// A path as typed, with a leading `~/` standing for the home folder.
pub fn expand_home(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME");
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        (None, Some(home)) if path == "~" => PathBuf::from(home),
        _ => PathBuf::from(path),
    }
}

impl Settings {
    pub fn load() -> Self {
        let mut settings: Settings = crate::state::load_json(settings_path()).unwrap_or_default();
        if let Some(legacy) = settings.legacy_startup.take()
            && legacy != Startup::Restore
        {
            settings.startup = legacy;
        }
        settings.normalise();
        settings
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = settings_path().ok_or_else(|| std::io::Error::other("HOME is unset"))?;
        crate::state::write_atomic(&path, &serde_json::to_vec_pretty(self)?)
    }

    /// Repairs what a hand-edited or older file might get wrong: every
    /// toolbar item listed exactly once, the address field always shown.
    pub fn normalise(&mut self) {
        let mut seen = Vec::new();
        self.toolbar.retain(|entry| {
            let fresh = !seen.contains(&entry.item);
            seen.push(entry.item);
            fresh
        });
        for item in ToolbarItem::ALL {
            if !seen.contains(&item) {
                self.toolbar.push(ToolbarEntry {
                    item,
                    shown: item == ToolbarItem::Address || item == ToolbarItem::Media,
                });
            }
        }
        for entry in &mut self.toolbar {
            if entry.item == ToolbarItem::Address {
                entry.shown = true;
            }
        }
        self.intensity = self.intensity.clamp(0.0, 2.0);
        self.page_zoom = self.page_zoom.clamp(0.5, 3.0);
        if engine(&self.search_engine).is_none() {
            self.search_engine = Settings::default().search_engine;
        }
    }

    /// The toolbar's shown items either side of the address field.
    pub fn toolbar_sides(&self) -> (Vec<ToolbarItem>, Vec<ToolbarItem>) {
        let shown: Vec<ToolbarItem> = self
            .toolbar
            .iter()
            .filter(|e| e.shown)
            .map(|e| e.item)
            .collect();
        let split = shown
            .iter()
            .position(|&i| i == ToolbarItem::Address)
            .unwrap_or(shown.len());
        (
            shown[..split].to_vec(),
            shown[split.saturating_add(1).min(shown.len())..].to_vec(),
        )
    }

    /// Moves a toolbar entry one place earlier or later.
    pub fn move_toolbar_item(&mut self, item: ToolbarItem, later: bool) {
        let Some(at) = self.toolbar.iter().position(|e| e.item == item) else {
            return;
        };
        let to = if later { at + 1 } else { at.wrapping_sub(1) };
        if to < self.toolbar.len() {
            self.toolbar.swap(at, to);
        }
    }

    /// Where downloads go: the folder chosen, or `~/Downloads` if none is.
    pub fn download_dir(&self) -> PathBuf {
        let dir = self.download_dir.trim();
        expand_home(if dir.is_empty() { "~/Downloads" } else { dir })
    }

    /// The URL a search for `query` goes to with `engine_id`.
    pub fn search_url(&self, engine_id: &str, query: &str) -> String {
        let fallback = || format!("https://duckduckgo.com/?q={}", encode(query));
        let Some(engine) = engine(engine_id) else {
            return fallback();
        };
        let template = if engine.id == "custom" {
            self.custom_search.trim()
        } else {
            engine.template
        };
        if !template.contains("%s") {
            return fallback();
        }
        let url = template.replace("%s", &encode(query));
        if engine.self_hosted {
            let base = self
                .instances
                .get(engine.id)
                .map(|s| s.trim().trim_end_matches('/'))
                .filter(|s| !s.is_empty());
            let Some(base) = base else {
                return fallback();
            };
            let base = if base.contains("://") {
                base.to_owned()
            } else {
                format!("https://{base}")
            };
            return url.replace("{instance}", &base);
        }
        url
    }
}

pub fn encode(query: &str) -> String {
    url::form_urlencoded::byte_serialize(query.as_bytes()).collect()
}

/// A search engine: `%s` is the query, `{instance}` a self-hosted one's
/// base URL.
pub struct SearchEngine {
    pub id: &'static str,
    pub name: &'static str,
    /// Typed as `@keyword query` in the address field to search it once.
    pub keyword: &'static str,
    pub template: &'static str,
    /// Needs a base URL of the user's own instance.
    pub self_hosted: bool,
}

const fn engine_def(
    id: &'static str,
    name: &'static str,
    keyword: &'static str,
    template: &'static str,
) -> SearchEngine {
    SearchEngine {
        id,
        name,
        keyword,
        template,
        self_hosted: false,
    }
}

const fn hosted(
    id: &'static str,
    name: &'static str,
    keyword: &'static str,
    template: &'static str,
) -> SearchEngine {
    SearchEngine {
        id,
        name,
        keyword,
        template,
        self_hosted: true,
    }
}

/// Every engine offered, grouped roughly: privacy-minded, independent
/// indexes, mainstream, regional, answer engines, niche and small-web,
/// reference, self-hosted, and one of your own.
pub const ENGINES: &[SearchEngine] = &[
    engine_def(
        "duckduckgo",
        "DuckDuckGo",
        "ddg",
        "https://duckduckgo.com/?q=%s",
    ),
    engine_def(
        "duckduckgo_lite",
        "DuckDuckGo Lite",
        "ddgl",
        "https://lite.duckduckgo.com/lite/?q=%s",
    ),
    engine_def(
        "duckduckgo_html",
        "DuckDuckGo HTML",
        "ddgh",
        "https://html.duckduckgo.com/html/?q=%s",
    ),
    engine_def("kagi", "Kagi", "k", "https://kagi.com/search?q=%s"),
    engine_def(
        "brave",
        "Brave Search",
        "br",
        "https://search.brave.com/search?q=%s",
    ),
    engine_def(
        "startpage",
        "Startpage",
        "sp",
        "https://www.startpage.com/sp/search?query=%s",
    ),
    engine_def(
        "mullvad_leta",
        "Mullvad Leta",
        "leta",
        "https://leta.mullvad.net/search?q=%s&engine=brave",
    ),
    engine_def(
        "ecosia",
        "Ecosia",
        "eco",
        "https://www.ecosia.org/search?q=%s",
    ),
    engine_def("qwant", "Qwant", "qw", "https://www.qwant.com/?q=%s"),
    engine_def(
        "mojeek",
        "Mojeek",
        "mj",
        "https://www.mojeek.com/search?q=%s",
    ),
    engine_def(
        "swisscows",
        "Swisscows",
        "sc",
        "https://swisscows.com/en/web?query=%s",
    ),
    engine_def(
        "metager",
        "MetaGer",
        "mg",
        "https://metager.org/meta/meta.ger3?eingabe=%s",
    ),
    engine_def(
        "presearch",
        "Presearch",
        "ps",
        "https://presearch.com/search?q=%s",
    ),
    engine_def(
        "ghostery",
        "Ghostery Private Search",
        "gho",
        "https://ghosterysearch.com/search?q=%s",
    ),
    engine_def("stract", "Stract", "st", "https://stract.com/search?q=%s"),
    engine_def("yep", "Yep", "yep", "https://yep.com/web?q=%s"),
    engine_def(
        "google",
        "Google",
        "g",
        "https://www.google.com/search?q=%s",
    ),
    engine_def(
        "google_web",
        "Google (web results only)",
        "gw",
        "https://www.google.com/search?udm=14&q=%s",
    ),
    engine_def("bing", "Bing", "b", "https://www.bing.com/search?q=%s"),
    engine_def(
        "yahoo",
        "Yahoo",
        "y",
        "https://search.yahoo.com/search?p=%s",
    ),
    engine_def(
        "yandex",
        "Yandex",
        "ya",
        "https://yandex.com/search/?text=%s",
    ),
    engine_def("baidu", "Baidu", "bd", "https://www.baidu.com/s?wd=%s"),
    engine_def(
        "naver",
        "Naver",
        "nv",
        "https://search.naver.com/search.naver?query=%s",
    ),
    engine_def("seznam", "Seznam", "sz", "https://search.seznam.cz/?q=%s"),
    engine_def(
        "perplexity",
        "Perplexity",
        "pp",
        "https://www.perplexity.ai/search?q=%s",
    ),
    engine_def("you", "You.com", "you", "https://you.com/search?q=%s"),
    engine_def("phind", "Phind", "ph", "https://www.phind.com/search?q=%s"),
    engine_def(
        "marginalia",
        "Marginalia",
        "mar",
        "https://search.marginalia.nu/search?query=%s",
    ),
    engine_def("wiby", "Wiby", "wiby", "https://wiby.me/?q=%s"),
    engine_def(
        "wikipedia",
        "Wikipedia",
        "w",
        "https://en.wikipedia.org/wiki/Special:Search?search=%s",
    ),
    engine_def("github", "GitHub", "gh", "https://github.com/search?q=%s"),
    engine_def(
        "stackoverflow",
        "Stack Overflow",
        "so",
        "https://stackoverflow.com/search?q=%s",
    ),
    engine_def(
        "mdn",
        "MDN Web Docs",
        "mdn",
        "https://developer.mozilla.org/en-US/search?q=%s",
    ),
    engine_def(
        "crates",
        "crates.io",
        "crates",
        "https://crates.io/search?q=%s",
    ),
    engine_def(
        "docs_rs",
        "docs.rs",
        "docs",
        "https://docs.rs/releases/search?query=%s",
    ),
    engine_def(
        "youtube",
        "YouTube",
        "yt",
        "https://www.youtube.com/results?search_query=%s",
    ),
    engine_def(
        "openstreetmap",
        "OpenStreetMap",
        "osm",
        "https://www.openstreetmap.org/search?query=%s",
    ),
    hosted("searxng", "SearXNG", "sx", "{instance}/search?q=%s"),
    hosted("whoogle", "Whoogle", "wh", "{instance}/search?q=%s"),
    hosted("fourget", "4get", "4g", "{instance}/web?s=%s"),
    hosted("librey", "LibreY", "ly", "{instance}/search.php?q=%s"),
    hosted(
        "yacy",
        "YaCy",
        "yacy",
        "{instance}/yacysearch.html?query=%s",
    ),
    engine_def("custom", "Custom…", "custom", ""),
];

pub fn engine(id: &str) -> Option<&'static SearchEngine> {
    ENGINES.iter().find(|e| e.id == id)
}

/// Whether a URL is on this machine or the local network, where plain HTTP
/// is normal and HTTPS-only must not interfere.
pub fn is_local(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => {
            host == "localhost" || host.ends_with(".local") || host.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => true,
    }
}

/// Accepts a URL or turns plain words into a web search. `@keyword words`
/// searches another engine once. Local and script URLs are intentionally
/// not interpreted as commands typed into the address bar.
pub fn destination(input: &str, settings: &Settings) -> String {
    let input = input.trim();
    if input.is_empty() {
        return settings.home_page.clone();
    }
    // The browser's own pages.
    if input.to_ascii_lowercase().starts_with("vamp://") {
        return input.to_owned();
    }
    let upgrade = |mut url: Url| -> String {
        if settings.https_only && url.scheme() == "http" && !is_local(&url) {
            let _ = url.set_scheme("https");
        }
        url.into()
    };
    if let Ok(url) = Url::parse(input)
        && matches!(url.scheme(), "http" | "https")
    {
        return upgrade(url);
    }
    // Other schemes a browser opens as they are: local files, blank pages,
    // inline data. A path from the Finder or a terminal is a file too.
    if let Ok(url) = Url::parse(input)
        && matches!(url.scheme(), "file" | "about" | "data" | "blob" | "webkit-extension")
    {
        return url.into();
    }
    if input.starts_with('/') || input.starts_with("~/") {
        let path = expand_home(input);
        if path.exists()
            && let Ok(url) = Url::from_file_path(&path)
        {
            return url.into();
        }
    }
    if let Some(rest) = input.strip_prefix('@')
        && let Some((keyword, query)) = rest.split_once(char::is_whitespace)
        && let Some(engine) = ENGINES
            .iter()
            .find(|e| e.keyword.eq_ignore_ascii_case(keyword))
        && !query.trim().is_empty()
    {
        return settings.search_url(engine.id, query.trim());
    }
    if !input.contains(char::is_whitespace)
        && (input.contains('.') || input.eq_ignore_ascii_case("localhost") || input.contains(':'))
    {
        let scheme = if input.starts_with("localhost") || input.starts_with("127.") {
            "http"
        } else {
            "https"
        };
        if let Ok(url) = Url::parse(&format!("{scheme}://{input}"))
            && url.host().is_some()
            && url
                .host_str()
                .is_some_and(|h| h.contains('.') || h == "localhost")
        {
            return upgrade(url);
        }
    }
    settings.search_url(&settings.search_engine, input)
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_files_and_blank_pages_open_directly() {
        let settings = Settings::default();
        assert_eq!(destination("file:///tmp/a.html", &settings), "file:///tmp/a.html");
        assert_eq!(destination("about:blank", &settings), "about:blank");
        assert_eq!(destination("/tmp", &settings), "file:///tmp");
    }

    use super::*;

    #[test]
    fn address_or_search() {
        let settings = Settings::default();
        assert_eq!(
            destination(" example.org ", &settings),
            "https://example.org/"
        );
        assert_eq!(
            destination("http://localhost:8000/a", &settings),
            "http://localhost:8000/a"
        );
        assert_eq!(
            destination("localhost:3000", &settings),
            "http://localhost:3000/"
        );
        assert!(destination("two words", &settings).contains("q=two+words"));
        assert!(
            destination("javascript:alert(1)", &settings).starts_with("https://duckduckgo.com/")
        );
        assert_eq!(destination("", &settings), HOME);
    }

    #[test]
    fn keywords_search_another_engine_once() {
        let settings = Settings::default();
        assert_eq!(
            destination("@k rust gpui", &settings),
            "https://kagi.com/search?q=rust+gpui"
        );
        // An unknown keyword is just words.
        assert!(destination("@nope thing", &settings).starts_with("https://duckduckgo.com/"));
    }

    #[test]
    fn self_hosted_engines_use_the_instance_and_fall_back_without_one() {
        let mut settings = Settings {
            search_engine: "searxng".into(),
            ..Settings::default()
        };
        assert!(destination("cats", &settings).starts_with("https://duckduckgo.com/"));
        settings
            .instances
            .insert("searxng".into(), "search.example.net/".into());
        assert_eq!(
            destination("cats & dogs", &settings),
            "https://search.example.net/search?q=cats+%26+dogs"
        );
    }

    #[test]
    fn custom_engine_needs_a_placeholder() {
        let mut settings = Settings {
            search_engine: "custom".into(),
            custom_search: "https://find.example/?term=%s".into(),
            ..Settings::default()
        };
        assert_eq!(
            destination("x y", &settings),
            "https://find.example/?term=x+y"
        );
        settings.custom_search = "https://find.example/".into();
        assert!(destination("x", &settings).starts_with("https://duckduckgo.com/"));
    }

    #[test]
    fn https_only_upgrades_everything_but_local_addresses() {
        let settings = Settings {
            https_only: true,
            ..Settings::default()
        };
        assert_eq!(
            destination("http://example.org/", &settings),
            "https://example.org/"
        );
        assert_eq!(
            destination("http://192.168.1.4/", &settings),
            "http://192.168.1.4/"
        );
        assert_eq!(
            destination("http://printer.local/", &settings),
            "http://printer.local/"
        );
    }

    #[test]
    fn toolbar_is_repaired_and_split_around_the_address_field() {
        let mut settings = Settings {
            toolbar: vec![
                ToolbarEntry {
                    item: ToolbarItem::Reload,
                    shown: true,
                },
                ToolbarEntry {
                    item: ToolbarItem::Reload,
                    shown: false,
                },
                ToolbarEntry {
                    item: ToolbarItem::Address,
                    shown: false,
                },
                ToolbarEntry {
                    item: ToolbarItem::Settings,
                    shown: true,
                },
            ],
            ..Settings::default()
        };
        settings.normalise();
        assert_eq!(settings.toolbar.len(), ToolbarItem::ALL.len());
        let (left, right) = settings.toolbar_sides();
        assert_eq!(left, [ToolbarItem::Reload]);
        assert_eq!(right, [ToolbarItem::Settings, ToolbarItem::Media]);
        settings.move_toolbar_item(ToolbarItem::Settings, false);
        let (left, right) = settings.toolbar_sides();
        assert_eq!(left, [ToolbarItem::Reload, ToolbarItem::Settings]);
        assert_eq!(right, [ToolbarItem::Media]);
    }

    #[test]
    fn every_engine_has_a_query_slot_and_unique_id() {
        for engine in ENGINES {
            assert!(
                engine.id == "custom" || engine.template.contains("%s"),
                "{}",
                engine.id
            );
            assert_eq!(
                engine.self_hosted,
                engine.template.contains("{instance}"),
                "{}",
                engine.id
            );
            assert_eq!(ENGINES.iter().filter(|e| e.id == engine.id).count(), 1);
            assert_eq!(
                ENGINES
                    .iter()
                    .filter(|e| e.keyword == engine.keyword)
                    .count(),
                1,
                "{}",
                engine.keyword
            );
        }
    }

    #[test]
    fn download_folder_expands_home_and_defaults_when_blank() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let mut settings = Settings {
            download_dir: "  ".into(),
            ..Settings::default()
        };
        assert_eq!(settings.download_dir(), home.join("Downloads"));
        settings.download_dir = "~/Desktop/in".into();
        assert_eq!(settings.download_dir(), home.join("Desktop/in"));
        settings.download_dir = "/tmp/x".into();
        assert_eq!(settings.download_dir(), PathBuf::from("/tmp/x"));
    }

    #[test]
    fn missing_fields_take_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"https_only":true}"#).unwrap();
        assert!(settings.https_only);
        assert_eq!(settings.search_engine, "duckduckgo");
        assert!(StartSection::ALL
            .into_iter()
            .all(|section| settings.start_page_sections.shown(section)));
    }

    #[test]
    fn start_page_sections_round_trip() {
        let mut settings = Settings::default();
        settings.start_page_sections.toggle(StartSection::Recent);
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(!restored.start_page_sections.shown(StartSection::Recent));
        assert!(restored.start_page_sections.shown(StartSection::Frequent));
    }
}
