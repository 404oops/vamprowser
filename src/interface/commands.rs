//! Everything the browser can be asked to do, from any direction — the
//! keyboard, the menu bar, a context menu, the tab switcher, a toolbar
//! button — as one [`Command`], carried out in one place.

use gpui::{App, ClipboardItem, Context, Pixels, Point, Window};
use wry::WebViewExtMacOS;

use crate::{
    Browser, Page, PingTarget, TabTarget,
    icons::Icon,
    media,
    settings::{Protection, SitePermission, StartSection, ToolbarItem},
};

/// A settings section, for opening settings where something lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    General,
    Appearance,
    Tabs,
    Search,
    Privacy,
    History,
    Downloads,
    Toolbar,
    Extensions,
    Advanced,
    About,
}

impl Section {
    pub const ALL: [Section; 11] = [
        Section::General,
        Section::Appearance,
        Section::Tabs,
        Section::Search,
        Section::Privacy,
        Section::History,
        Section::Downloads,
        Section::Toolbar,
        Section::Extensions,
        Section::Advanced,
        Section::About,
    ];

    /// Its part of a `vamp://settings/…` address.
    pub fn slug(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Appearance => "appearance",
            Section::Tabs => "tabs",
            Section::Search => "search",
            Section::Privacy => "privacy",
            Section::History => "history",
            Section::Downloads => "downloads",
            Section::Toolbar => "toolbar",
            Section::Extensions => "extensions",
            Section::Advanced => "advanced",
            Section::About => "about",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Section> {
        Section::ALL
            .into_iter()
            .find(|s| s.slug().eq_ignore_ascii_case(slug))
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Appearance => "Appearance",
            Section::Tabs => "Tabs",
            Section::Search => "Search",
            Section::Privacy => "Privacy & Security",
            Section::History => "History",
            Section::Downloads => "Downloads",
            Section::Toolbar => "Toolbar",
            Section::Extensions => "Extensions",
            Section::Advanced => "Advanced",
            Section::About => "About",
        }
    }
}

/// Where a link opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Here,
    NewTab,
    BackgroundTab,
    PrivateTab,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    NewTab,
    NewPrivateTab,
    NewWindow,
    NewPrivateWindow,
    CloseWindow,
    CloseTab,
    CloseTabId(u64),
    CloseOtherTabs(u64),
    CloseTabsToRight(u64),
    ReopenClosedTab,
    ReopenClosedWindow,
    Minimize,
    Zoom,
    /// Stops the page loading.
    Stop,
    ShowWebInspector,
    Find,
    FindNext,
    FindPrevious,
    CloseFind,
    DuplicateTab(u64),
    ReloadTab(u64),
    ToggleTabMute(u64),
    SelectTab(usize),
    SelectTabId(u64),
    SelectLastTab,
    NextTab,
    PreviousTab,
    FocusAddress,
    Back,
    Forward,
    Reload,
    /// Empties the cache of what the page loaded, then reloads it from the
    /// network.
    EraseCacheAndReload,
    Home,
    Print,
    BookmarkPage,
    BookmarkTab(u64),
    ToggleBookmarksBar,
    ToggleStartSection(StartSection),
    ToggleVerticalTabs,
    ToggleMinimalMode,
    ToggleReaderMode,
    CheckExtensionUpdates,
    ToggleCompactTabs,
    SwitchTabs,
    Settings(Section),
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Open(String, Place),
    Copy(String),
    CopyLink,
    CopyCleanLink,
    CopyMarkdownLink,
    PasteAndGo,
    /// Bookmarks and folders, by id (see [`crate::bookmarks`]).
    RenameBookmark(u64),
    EditBookmarkUrl(u64),
    DeleteBookmark(u64),
    MoveBookmark(u64, isize),
    /// Several bookmarks and folders at once: a selection in the manager.
    MoveBookmarksTo(Vec<u64>, Option<u64>),
    DeleteBookmarks(Vec<u64>),
    OpenBookmarks(Vec<u64>),
    /// Into a folder, or with `None`, onto the bookmarks bar.
    MoveBookmarkTo(u64, Option<u64>),
    /// A new folder inside another, or on the bar.
    NewBookmarkFolder(Option<u64>),
    /// Files a bookmark in a new folder beside where it is.
    FileInNewFolder(u64),
    OpenBookmarkFolder(u64),
    ShowBookmarks,
    ImportBookmarks(crate::bookmarks::Source),
    ImportBookmarksFile,
    ExportBookmarks,
    ForgetPage(String),
    ClearHistory,
    ClearBrowsingData,
    SetSitePermission(String, usize, Option<SitePermission>),
    SetSiteProtection(String, Option<Protection>),
    SetSiteCookies(String, Option<bool>),
    ClearSiteData(String, crate::cache::SiteDataKind),
    ClearFileLocalStorage(String),
    /// The whole profile, logins and all, to an archive.
    ExportBrowserData,
    /// An archive in place of the profile, relaunching.
    ImportBrowserData,
    OpenDownloads,
    ResetToolbar,
    MakeDefaultBrowser,
    ExtensionAction(String),
    InstallExtension,
    ShowDownloads,
    InspectMedia,
    DownloadMedia(String, std::sync::Arc<media::Info>, media::Choice, bool),
    OpenDownload(u64),
    RevealDownload(u64),
    RemoveDownload(u64),
    ClearDownloads,
    /// Content rules finished compiling; apply them to every tab.
    ApplyRules,
    ClosePalette,
    /// Walks the address field's suggestions.
    SuggestMove(isize),
    DismissSuggestions,
    CloseBookmarkMenu,
    RemoveSuggestion,
    PaletteMove(isize),
    PaletteChoose,
}

impl Command {
    /// The name the tab switcher lists a command under, for those worth
    /// finding by typing.
    pub fn palette_label(&self) -> Option<&'static str> {
        Some(match self {
            Command::NewTab => "New Tab",
            Command::NewWindow => "New Window",
            Command::NewPrivateWindow => "New Private Window",
            Command::CloseWindow => "Close Window",
            Command::NewPrivateTab => "New Private Tab",
            Command::ReopenClosedTab => "Reopen Closed Tab",
            Command::ReopenClosedWindow => "Reopen Closed Window",
            Command::Minimize => "Minimize",
            Command::Zoom => "Zoom",
            Command::Stop => "Stop Loading",
            Command::EraseCacheAndReload => "Erase Cache and Reload",
            Command::ShowWebInspector => "Show Web Inspector",
            Command::Find => "Find in Page…",
            Command::FindNext => "Find Next",
            Command::FindPrevious => "Find Previous",
            Command::CloseFind => "Close Find Bar",
            Command::CloseTab => "Close Tab",
            Command::ToggleBookmarksBar => "Toggle Bookmarks Bar",
            Command::ToggleVerticalTabs => "Toggle Vertical Tabs",
            Command::ToggleMinimalMode => "Minimal Mode",
            Command::ToggleReaderMode => "Reader Mode",
            Command::CheckExtensionUpdates => "Check for Extension Updates",
            Command::ToggleCompactTabs => "Toggle Compact Sidebar",
            Command::BookmarkPage => "Bookmark This Page",
            Command::Settings(Section::General) => "Settings",
            Command::Settings(Section::Search) => "Settings: Search Engine",
            Command::Settings(Section::Privacy) => "Settings: Privacy & Security",
            Command::Settings(Section::History) => "Show History",
            Command::Settings(Section::Extensions) => "Manage Extensions",
            Command::InstallExtension => "Install Extension…",
            Command::Settings(Section::Toolbar) => "Customize Toolbar",
            Command::Settings(Section::Appearance) => "Settings: Appearance",
            Command::Settings(Section::Tabs) => "Settings: Tabs",
            Command::Settings(Section::Downloads) => "Settings: Downloads",
            Command::Settings(Section::Advanced) => "Settings: Advanced",
            Command::Settings(Section::About) => "About Vamprowser",
            Command::ClearHistory => "Clear History",
            Command::ClearBrowsingData => "Clear Cookies and Website Data",
            Command::ExportBrowserData => "Export Browsing Data…",
            Command::ImportBrowserData => "Import Browsing Data…",
            Command::OpenDownloads => "Open Downloads Folder",
            Command::CopyLink => "Copy Link",
            Command::CopyCleanLink => "Copy Link Without Tracking",
            Command::CopyMarkdownLink => "Copy Link as Markdown",
            Command::ZoomIn => "Zoom In",
            Command::ZoomOut => "Zoom Out",
            Command::ZoomReset => "Actual Size",
            Command::Print => "Print…",
            Command::ShowDownloads => "Show Downloads",
            Command::InspectMedia => "Download Media…",
            Command::Home => "Go Home",
            Command::MakeDefaultBrowser => "Make Vamprowser the Default Browser",
            _ => return None,
        })
    }

    /// Whether it changes something every window shows: bookmarks, history,
    /// downloads, the toolbar. The rest (tabs, navigation, the switcher and
    /// suggestions, zoom) stay within their window.
    fn touches_shared(&self) -> bool {
        matches!(
            self,
            Command::BookmarkPage
                | Command::BookmarkTab(_)
                | Command::RenameBookmark(_)
                | Command::EditBookmarkUrl(_)
                | Command::DeleteBookmark(_)
                | Command::MoveBookmark(..)
                | Command::MoveBookmarkTo(..)
                | Command::MoveBookmarksTo(..)
                | Command::DeleteBookmarks(_)
                | Command::NewBookmarkFolder(_)
                | Command::FileInNewFolder(_)
                | Command::ImportBookmarks(_)
                | Command::ImportBookmarksFile
                | Command::ForgetPage(_)
                | Command::ClearHistory
                | Command::ClearBrowsingData
                | Command::ResetToolbar
                | Command::RemoveDownload(_)
                | Command::ClearDownloads
                | Command::InstallExtension
        )
    }

    /// Commands the tab switcher offers.
    pub fn palette() -> Vec<Command> {
        vec![
            Command::NewTab,
            Command::NewWindow,
            Command::NewPrivateWindow,
            Command::ReopenClosedTab,
            Command::ReopenClosedWindow,
            Command::CloseTab,
            Command::BookmarkPage,
            Command::ToggleBookmarksBar,
            Command::ToggleVerticalTabs,
            Command::ToggleMinimalMode,
            Command::ToggleReaderMode,
            Command::ToggleCompactTabs,
            Command::Settings(Section::General),
            Command::Settings(Section::Search),
            Command::Settings(Section::Privacy),
            Command::Settings(Section::History),
            Command::Settings(Section::Extensions),
            Command::InstallExtension,
            Command::Settings(Section::Toolbar),
            Command::Settings(Section::Appearance),
            Command::Settings(Section::Tabs),
            Command::Settings(Section::Downloads),
            Command::Settings(Section::Advanced),
            Command::Settings(Section::About),
            Command::CopyLink,
            Command::CopyCleanLink,
            Command::CopyMarkdownLink,
            Command::ZoomIn,
            Command::ZoomOut,
            Command::ZoomReset,
            Command::Find,
            Command::Stop,
            Command::EraseCacheAndReload,
            Command::ShowWebInspector,
            Command::Print,
            Command::Home,
            Command::ShowDownloads,
            Command::OpenDownloads,
            Command::ClearHistory,
            Command::ClearBrowsingData,
            Command::ExportBrowserData,
            Command::ImportBrowserData,
            Command::MakeDefaultBrowser,
        ]
    }
}

/// Query parameters that exist to track who clicked what.
const TRACKING_PARAMS: &[&str] = &[
    "fbclid",
    "gclid",
    "dclid",
    "gbraid",
    "wbraid",
    "msclkid",
    "mc_cid",
    "mc_eid",
    "igshid",
    "yclid",
    "twclid",
    "ttclid",
    "li_fat_id",
    "_hsenc",
    "_hsmi",
    "mkt_tok",
    "oly_anon_id",
    "oly_enc_id",
    "vero_id",
    "spm",
    "ref_src",
    "ref_url",
];

/// `url` without `utm_*` and other click-tracking parameters.
pub fn clean_link(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_owned();
    };
    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            !key.starts_with("utm_") && !TRACKING_PARAMS.contains(&key.as_str())
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(kept);
    }
    parsed.into()
}

fn ping_for_command(command: &Command, web: bool) -> Option<PingTarget> {
    match command {
        Command::Back => Some(PingTarget::Toolbar(ToolbarItem::Back)),
        Command::Forward => Some(PingTarget::Toolbar(ToolbarItem::Forward)),
        Command::Reload | Command::EraseCacheAndReload | Command::Stop => {
            Some(PingTarget::Toolbar(ToolbarItem::Reload))
        }
        Command::Home => Some(PingTarget::Toolbar(ToolbarItem::Home)),
        Command::NewTab => Some(PingTarget::Toolbar(ToolbarItem::NewTab)),
        Command::NewPrivateTab => Some(PingTarget::Toolbar(ToolbarItem::PrivateTab)),
        Command::ToggleBookmarksBar => Some(PingTarget::Toolbar(ToolbarItem::BookmarksBar)),
        Command::ToggleVerticalTabs => Some(PingTarget::Toolbar(ToolbarItem::Sidebar)),
        Command::SwitchTabs => Some(PingTarget::Toolbar(ToolbarItem::CommandPalette)),
        Command::ShowDownloads => Some(PingTarget::Toolbar(ToolbarItem::Downloads)),
        Command::InspectMedia => Some(PingTarget::Toolbar(ToolbarItem::Media)),
        Command::Settings(_) => Some(PingTarget::Toolbar(ToolbarItem::Settings)),
        Command::CopyLink if web => Some(PingTarget::CopyLink),
        Command::CopyCleanLink | Command::CopyMarkdownLink if web => Some(PingTarget::Omnibox),
        Command::PasteAndGo => Some(PingTarget::Omnibox),
        Command::BookmarkPage if web => Some(PingTarget::BookmarkPage),
        _ => None,
    }
}

impl Browser {
    pub(crate) fn run(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        // A window closing with its last tab has nothing to act on.
        if self.tabs.is_empty() {
            return;
        }
        // Bookmarks, downloads and history show in every window.
        let shared = command.touches_shared();
        self.run_here(command, window, cx);
        if shared {
            self.refresh_other_windows(cx);
        }
    }

    fn close_tabs(&mut self, ids: Vec<u64>, window: &mut Window, cx: &mut Context<Self>) {
        for id in ids {
            if let Some(index) = self.index_of(id) {
                self.close_tab(index, window, cx);
            }
        }
    }

    fn run_here(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        let pinged = ping_for_command(&command, self.current().page == Page::Web);
        if pinged.is_some() {
            self.ping = (pinged, self.ping.1 + 1);
            // The bar starts at once, before WebKit reports anything.
            if matches!(
                command,
                Command::Back | Command::Forward | Command::Reload | Command::EraseCacheAndReload
            ) && self.current().page == Page::Web
            {
                let index = self.selected;
                self.tabs[index].loading = Some((std::time::Instant::now(), None));
            }
            cx.notify();
        }
        match command {
            Command::NewTab => self.new_tab(false, window, cx),
            Command::SetSitePermission(host, slot, choice) => {
                self.change_site_control(&host, |site| site.set_permission(slot, choice), false, cx);
            }
            Command::SetSiteProtection(host, choice) => {
                self.change_site_control(&host, |site| site.protection = choice, true, cx);
            }
            Command::SetSiteCookies(host, choice) => {
                self.change_site_control(&host, |site| site.third_party_cookies = choice, true, cx);
            }
            Command::ClearSiteData(host, kind) => {
                if let Some(view) = &self.current().view {
                    crate::cache::clear_site_data(&view.webview(), host, kind);
                }
            }
            Command::ClearFileLocalStorage(url) => {
                if let Some(view) = &self.current().view {
                    crate::cache::clear_file_local_storage(&view.webview(), &url);
                }
            }
            Command::ToggleStartSection(section) => {
                self.settings.start_page_sections.toggle(section);
                self.save_settings(cx);
            }
            Command::NewWindow => self.open_window(false, cx),
            Command::NewPrivateWindow => self.open_window(true, cx),
            Command::CloseWindow => window.remove_window(),
            Command::NewPrivateTab => self.new_tab(true, window, cx),
            Command::CloseTab => self.close_tab(self.selected, window, cx),
            Command::CloseTabId(id) => {
                if let Some(index) = self.index_of(id) {
                    self.close_tab(index, window, cx);
                }
            }
            Command::CloseOtherTabs(id) => {
                let others = self.tabs.iter().map(|t| t.id).filter(|&t| t != id).collect();
                self.close_tabs(others, window, cx);
            }
            Command::CloseTabsToRight(id) => {
                if let Some(index) = self.index_of(id) {
                    let right = self.tabs[index + 1..].iter().map(|t| t.id).collect();
                    self.close_tabs(right, window, cx);
                }
            }
            Command::ReopenClosedTab => {
                // Then the windows closed, once this one's tabs run out.
                if let Some(closed) = self.recently_closed.pop() {
                    let target = TabTarget::of(closed.page, &closed.url);
                    self.open_tab(target, closed.private, false, window, cx);
                } else {
                    self.reopen_closed_window(cx);
                }
            }
            Command::ReopenClosedWindow => {
                self.reopen_closed_window(cx);
            }
            Command::DuplicateTab(id) => {
                if let Some(tab) = self.tabs.iter().find(|t| t.id == id) {
                    let target = TabTarget::of(tab.page, &tab.url);
                    let private = tab.private;
                    self.open_tab(target, private, false, window, cx);
                }
            }
            Command::ReloadTab(id) => {
                if let Some(view) = self
                    .tabs
                    .iter()
                    .find(|t| t.id == id)
                    .and_then(|t| t.view.clone())
                {
                    let _ = view.reload();
                }
            }
            Command::ToggleTabMute(id) => self.toggle_tab_mute(id, cx),
            Command::SelectTab(index) => {
                if index < self.tabs.len() {
                    self.select(index, cx);
                }
            }
            Command::SelectTabId(id) => {
                if let Some(index) = self.index_of(id) {
                    self.select(index, cx);
                }
            }
            Command::SelectLastTab => self.select(self.tabs.len() - 1, cx),
            Command::NextTab => self.select((self.selected + 1) % self.tabs.len(), cx),
            Command::PreviousTab => {
                self.select((self.selected + self.tabs.len() - 1) % self.tabs.len(), cx)
            }
            Command::FocusAddress => self.focus_address(window, cx),
            Command::Back => {
                if let Some(view) = self.current().view.clone() {
                    let _ = view.go_back();
                }
            }
            Command::Forward => {
                if let Some(view) = self.current().view.clone() {
                    let _ = view.go_forward();
                }
            }
            Command::Reload => {
                if let Some(view) = self.current().view.clone() {
                    let _ = view.reload();
                }
            }
            Command::EraseCacheAndReload => {
                if let Some(view) = self.current().view.clone()
                    && self.current().page == Page::Web
                {
                    crate::cache::erase_and_reload(&wry::WebViewExtMacOS::webview(&*view));
                }
            }
            Command::Stop => self.stop_loading(cx),
            Command::Minimize => window.minimize_window(),
            Command::Zoom => window.zoom_window(),
            Command::ShowWebInspector => self.show_web_inspector(),
            Command::Find => self.open_find(window, cx),
            // With the bar closed, ⌘G opens it on the last search.
            Command::FindNext | Command::FindPrevious if !self.find.open => self.open_find(window, cx),
            Command::FindNext => self.find_step(false, cx),
            Command::FindPrevious => self.find_step(true, cx),
            Command::CloseFind => self.close_find(window, cx),
            Command::Home => {
                let home = self.settings.home_page.clone();
                self.navigate(&home, window, cx);
            }
            Command::Print => {
                if let Some(view) = self.current().view.clone() {
                    let _ = view.print();
                }
            }
            Command::BookmarkPage => self.toggle_bookmark(window, cx),
            Command::BookmarkTab(id) => {
                if let Some(tab) = self.tabs.iter().find(|t| t.id == id && t.page == Page::Web) {
                    let (url, title) = (tab.url.clone(), tab.title.clone());
                    if self.bookmarks().find_url(&url).is_none() {
                        let folder = self.last_bookmark_folder;
                        self.add_bookmark(url, title, folder);
                        cx.notify();
                    }
                }
            }
            command @ (Command::RenameBookmark(_)
            | Command::EditBookmarkUrl(_)
            | Command::DeleteBookmark(_)
            | Command::MoveBookmark(..)
            | Command::MoveBookmarkTo(..)
            | Command::MoveBookmarksTo(..)
            | Command::DeleteBookmarks(_)
            | Command::OpenBookmarks(_)
            | Command::NewBookmarkFolder(_)
            | Command::FileInNewFolder(_)
            | Command::OpenBookmarkFolder(_)
            | Command::ShowBookmarks
            | Command::ImportBookmarks(_)
            | Command::ImportBookmarksFile
            | Command::ExportBookmarks) => self.run_bookmark_command(command, window, cx),
            Command::ToggleBookmarksBar => self.toggle_bookmarks_bar(cx),
            Command::ToggleVerticalTabs => self.toggle_vertical_tabs(cx),
            Command::ToggleMinimalMode => self.toggle_minimal(window, cx),
            Command::ToggleReaderMode => {
                if self.current().page == Page::Web {
                    if let Some(view) = &self.current().view {
                        let _ = view.evaluate_script(crate::reader::TOGGLE_SCRIPT);
                    }
                }
            }
            Command::CheckExtensionUpdates => self.check_extension_updates(true, cx),
            Command::ToggleCompactTabs => self.toggle_compact_vertical_tabs(cx),
            Command::SwitchTabs => self.toggle_palette(window, cx),
            Command::Settings(section) => self.open_settings(section, window, cx),
            Command::ZoomIn => self.zoom_by(1.1, cx),
            Command::ZoomOut => self.zoom_by(1.0 / 1.1, cx),
            Command::ZoomReset => self.zoom_by(0.0, cx),
            Command::Open(url, place) => self.open_link(&url, place, window, cx),
            Command::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Command::CopyLink => {
                let url = self.current().url.clone();
                if self.current().page == Page::Web {
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                }
            }
            Command::CopyCleanLink => {
                if self.current().page == Page::Web {
                    let url = clean_link(&self.current().url);
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                }
            }
            Command::CopyMarkdownLink => {
                let tab = self.current();
                if tab.page == Page::Web {
                    let text = format!("[{}]({})", tab.title.replace(['[', ']'], ""), tab.url);
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            Command::PasteAndGo => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.navigate(text.trim(), window, cx);
                }
            }
            Command::ForgetPage(url) => {
                self.history().remove(&url);
                self.history().save_in_background();
                cx.notify();
            }
            Command::ClearHistory => self.confirm_then(
                "Clear all history?".into(),
                "Every page you've visited is forgotten. This can't be undone.".into(),
                "Clear History",
                cx,
                |this, cx| {
                    this.history().clear();
                    this.history().save_in_background();
                    // Closed tabs and windows, kept for ⌘⇧T, go too.
                    this.common.closed_windows.borrow_mut().clear();
                    this.recently_closed.clear();
                    this.for_other_windows(cx, |browser, _| browser.recently_closed.clear());
                    this.common.save_state();
                    this.refresh_other_windows(cx);
                    cx.notify();
                },
            ),
            Command::ClearBrowsingData => self.confirm_then(
                "Clear cookies and website data?".into(),
                "You'll be signed out of every site, and their caches and storage are emptied. This can't be undone.".into(),
                "Clear",
                cx,
                |this, cx| this.clear_website_data(cx),
            ),
            Command::ExportBrowserData => {
                self.persist();
                self.for_other_windows(cx, |browser, _| browser.persist());
                self.history().save();
                self.common.state_writer.flush();
                let sender = self.common.anywhere.clone();
                let dir = self.settings.download_dir();
                cx.spawn_in(window, async move |_, cx| {
                    let Ok(asked) = cx.update(|_, cx| cx.prompt_for_new_path(&dir, Some("Vamprowser Data.zip"))) else {
                        return;
                    };
                    let Ok(Ok(Some(path))) = asked.await else {
                        return;
                    };
                    let _ = sender.try_send(crate::BrowserEvent::Notice("Exporting…".into()));
                    std::thread::spawn(move || {
                        let result = crate::sitedata::export(&path).map(|_| path);
                        let _ = sender.try_send(crate::BrowserEvent::Exported(result));
                    });
                })
                .detach();
            }
            Command::ImportBrowserData => {
                let sender = self.sender.clone();
                cx.spawn_in(window, async move |_, cx| {
                    let Some(path) = choose_file(cx, "Import").await else {
                        return;
                    };
                    let _ = sender.try_send(crate::BrowserEvent::ImportChosen(path));
                })
                .detach();
            }
            Command::OpenDownloads => {
                let dir = self.settings.download_dir();
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::process::Command::new("open").arg(dir).spawn();
            }
            Command::ResetToolbar => {
                self.settings.toolbar = crate::settings::Settings::default().toolbar;
                self.save_settings(cx);
            }
            Command::MakeDefaultBrowser => {
                crate::interop::make_default_browser();
                self.notice = Some(
                    "macOS will ask you to confirm. If nothing happens, run Vamprowser from its app bundle."
                        .into(),
                );
                cx.notify();
            }
            Command::ExtensionAction(id) => self.extension_action(&id),
            Command::InstallExtension => self.ask(
                "Install Extension",
                "A Firefox add-on's name or addons.mozilla.org link, or a .xpi file path.",
                String::new(),
                |this, answer, cx| {
                    this.settings_section = Section::Extensions;
                    this.install_extension(answer, cx);
                },
                cx,
            ),
            Command::ShowDownloads => {
                self.download_page_offset = 0;
                self.controls.scroll("downloads-page").set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
                self.open_page(Page::Downloads, window, cx);
            }
            Command::InspectMedia => {
                let tab = self.current();
                if tab.page == Page::Web {
                    let (id, url) = (tab.id, tab.url.clone());
                    let position = window.mouse_position();
                    let sender = self.sender.clone();
                    self.media_inspection_serial += 1;
                    let serial = self.media_inspection_serial;
                    self.media_inspecting = Some(serial);
                    cx.notify();
                    std::thread::spawn(move || {
                        let result = media::inspect(&url);
                        let _ = sender.try_send(crate::BrowserEvent::MediaInspected(serial, id, url, position, result));
                    });
                }
            }
            Command::DownloadMedia(url, info, choice, private) => {
                let destination = match media::destination(&self.settings.download_dir(), &info, &choice) {
                    Ok(path) => path,
                    Err(error) => {
                        vampir::dialog_window::alert(cx, self.controls.palette(), "Couldn't create download folder", error);
                        return;
                    }
                };
                let id = self.downloads().started(url.clone(), destination.clone(), private, self.serial);
                self.controls.scroll("download-shelf-items").set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
                self.refresh_other_windows(cx);
                cx.notify();
                let sender = self.sender.clone();
                std::thread::spawn(move || {
                    let result = media::download(&url, &destination, &choice);
                    let _ = sender.try_send(crate::BrowserEvent::MediaFinished(id, result));
                });
            }
            Command::ApplyRules => {
                self.apply_rules_everywhere(cx);
                self.release_waiting_loads();
                // Compiling the rules held tens of megabytes of text.
                crate::return_freed_memory();
            }
            command @ (Command::OpenDownload(_)
            | Command::RevealDownload(_)
            | Command::RemoveDownload(_)
            | Command::ClearDownloads) => self.download_command(command, cx),
            Command::ClosePalette => self.close_palette(cx),
            Command::SuggestMove(by) => self.move_suggestion(by, cx),
            Command::CloseBookmarkMenu => self.close_bookmark_menu(cx),
            Command::RemoveSuggestion => self.remove_suggestion(None, cx),
            Command::DismissSuggestions => {
                // Escape, as in Safari: the list goes, and the field shows
                // the page's address again.
                self.close_suggestions(cx);
                self.reset_address(cx);
                let focus = self.address.read(cx).focus_handle.clone();
                focus.dispatch_action(&vampir::text_input::SelectAll, window, cx);
            }
            Command::PaletteMove(by) => self.move_palette(by, cx),
            Command::PaletteChoose => self.choose_palette(window, cx),
        }
    }

    /// Shows a Vampir context menu at `position` and runs the chosen command.
    pub(crate) fn context_menu(
        &mut self,
        position: Point<Pixels>,
        items: Vec<MenuItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_context_menu(position, items, false, window, cx);
    }

    pub(crate) fn context_menu_above(
        &mut self,
        position: Point<Pixels>,
        items: Vec<MenuItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_context_menu(position, items, true, window, cx);
    }

    fn open_context_menu(
        &mut self,
        position: Point<Pixels>,
        items: Vec<MenuItem>,
        above: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let host = cx.entity().downgrade();
        let chosen = move |choice: Option<Option<Command>>, window: &mut Window, cx: &mut App| {
            let (Some(Some(command)), Some(host)) = (choice, host.upgrade()) else {
                return;
            };
            host.update(cx, |browser, cx| browser.run(command, window, cx));
        };
        let palette = self.controls.palette();
        if above {
            vampir::floating_menu::open_above_with(cx, window, position, items, palette, chosen);
        } else {
            vampir::floating_menu::open_with(cx, window, position, items, palette, chosen);
        }
    }

    /// Asks for a line of text in a Vampir dialog, then calls `answered`.
    pub(crate) fn ask(
        &mut self,
        title: &'static str,
        message: &'static str,
        initial: String,
        answered: impl FnOnce(&mut Browser, String, &mut Context<Browser>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let host = cx.entity().downgrade();
        vampir::dialog_window::prompt(
            cx,
            self.controls.palette(),
            title,
            message,
            initial,
            "OK",
            move |answer, cx| {
                if let (Some(answer), Some(host)) = (answer, host.upgrade()) {
                    host.update(cx, |browser, cx| answered(browser, answer, cx));
                }
            },
        );
    }

    pub(crate) fn tab_menu(&self, id: u64) -> Vec<MenuItem> {
        let Some(index) = self.index_of(id) else {
            return Vec::new();
        };
        let tab = &self.tabs[index];
        let web = tab.page == Page::Web;
        let bookmarked = web && self.bookmarks().find_url(&tab.url).is_some();
        vec![
            MenuItem::action(Some(Command::NewTab), "New Tab").icon(Icon::Plus),
            MenuItem::action(Some(Command::NewPrivateTab), "New Private Tab").icon(Icon::Private),
            MenuItem::separator(),
            MenuItem::action(
                Some(Command::ReloadTab(id)),
                "Reload Tab",
            )
            .enabled(web)
            .icon(Icon::Reload),
            if tab.muted {
                MenuItem::action(
                    Some(Command::ToggleTabMute(id)),
                    "Unmute Tab",
                )
                .enabled(web)
                .icon(Icon::Sound)
            } else {
                MenuItem::action(
                    Some(Command::ToggleTabMute(id)),
                    "Mute Tab",
                )
                .enabled(web)
                .icon(Icon::SoundMuted)
            },
            MenuItem::action(Some(Command::DuplicateTab(id)), "Duplicate Tab")
                .icon(Icon::Duplicate),
            if bookmarked {
                MenuItem::action(
                    Some(Command::BookmarkTab(id)),
                    "Bookmarked",
                )
                .enabled(false)
                .icon(Icon::StarFilled)
            } else {
                MenuItem::action(
                    Some(Command::BookmarkTab(id)),
                    "Bookmark Tab",
                )
                .enabled(web)
                .icon(Icon::Star)
            },
            MenuItem::action(
                Some(Command::Copy(tab.url.clone())),
                "Copy Link",
            )
            .enabled(web)
            .icon(Icon::Link),
            MenuItem::action(
                Some(Command::Copy(clean_link(&tab.url))),
                "Copy Link Without Tracking",
            )
            .enabled(web)
            .icon(Icon::Shield),
            MenuItem::separator(),
            MenuItem::action(Some(Command::CloseTabId(id)), "Close Tab").icon(Icon::Close),
            MenuItem::action(
                Some(Command::CloseOtherTabs(id)),
                "Close Other Tabs",
            )
            .enabled(self.tabs.len() > 1)
            .icon(Icon::Tabs),
            MenuItem::action(
                Some(Command::CloseTabsToRight(id)),
                "Close Tabs to the Right",
            )
            .enabled(index + 1 < self.tabs.len())
            .icon(Icon::CloseRight),
            MenuItem::separator(),
            MenuItem::action(
                Some(Command::ReopenClosedTab),
                "Reopen Closed Tab",
            )
            .enabled(!self.recently_closed.is_empty())
            .icon(Icon::Undo),
            MenuItem::action(
                Some(Command::ToggleVerticalTabs),
                "Vertical Tabs",
            )
            .checked(self.vertical_tabs)
            .icon(Icon::Sidebar),
        ]
    }

    pub(crate) fn tab_strip_menu(&self) -> Vec<MenuItem> {
        vec![
            MenuItem::action(Some(Command::NewTab), "New Tab").icon(Icon::Plus),
            MenuItem::action(Some(Command::NewPrivateTab), "New Private Tab").icon(Icon::Private),
            MenuItem::action(
                Some(Command::ReopenClosedTab),
                "Reopen Closed Tab",
            )
            .enabled(!self.recently_closed.is_empty())
            .icon(Icon::Undo),
            MenuItem::separator(),
            MenuItem::action(
                Some(Command::ToggleVerticalTabs),
                "Vertical Tabs",
            )
            .checked(self.vertical_tabs)
            .icon(Icon::Sidebar),
            MenuItem::action(
                Some(Command::ToggleBookmarksBar),
                "Bookmarks Bar",
            )
            .checked(self.bookmarks_bar)
            .icon(Icon::Bookmark),
        ]
    }

    pub(crate) fn bookmarks_bar_menu(&self) -> Vec<MenuItem> {
        let web = self.current().page == Page::Web;
        vec![
            MenuItem::action(
                Some(Command::BookmarkPage),
                "Bookmark This Page",
            )
            .enabled(web && !self.bookmarked())
            .icon(Icon::Star),
            MenuItem::action(
                Some(Command::NewBookmarkFolder(None)),
                "New Folder…",
            )
            .icon(Icon::Folder),
            MenuItem::separator(),
            MenuItem::action(Some(Command::ShowBookmarks), "Manage Bookmarks…")
                .icon(Icon::Bookmark),
            MenuItem::action(
                Some(Command::ToggleBookmarksBar),
                "Hide Bookmarks Bar",
            )
            .icon(Icon::Hide),
        ]
    }

    pub(crate) fn omnibox_menu(&self, clipboard: bool) -> Vec<MenuItem> {
        let tab = self.current();
        let web = tab.page == Page::Web;
        vec![
            MenuItem::action(
                Some(Command::PasteAndGo),
                "Paste and Go",
            )
            .enabled(clipboard)
            .icon(Icon::Paste),
            MenuItem::separator(),
            MenuItem::action(Some(Command::CopyLink), "Copy Link").enabled(web).icon(Icon::Link),
            MenuItem::action(
                Some(Command::CopyCleanLink),
                "Copy Link Without Tracking",
            )
            .enabled(web)
            .icon(Icon::Shield),
            MenuItem::action(
                Some(Command::CopyMarkdownLink),
                "Copy Link as Markdown",
            )
            .enabled(web)
            .icon(Icon::Markdown),
            MenuItem::action(
                Some(Command::Copy(tab.title.clone())),
                "Copy Page Title",
            )
            .enabled(web)
            .icon(Icon::Text),
            MenuItem::separator(),
            match self.bookmarks().find_url(&tab.url) {
                Some(id) => MenuItem::action(Some(Command::DeleteBookmark(id)), "Delete Bookmark")
                    .enabled(web)
                    .icon(Icon::Trash),
                None => MenuItem::action(Some(Command::BookmarkPage), "Bookmark This Page")
                    .enabled(web)
                    .icon(Icon::Star),
            },
            MenuItem::action(
                Some(Command::Settings(Section::Search)),
                "Settings: Search Engine…",
            )
            .icon(Icon::Search),
        ]
    }

    /// What yt-dlp found on the page. Each row names the formats it
    /// downloads; the first two are the ones macOS plays.
    pub(crate) fn media_menu(
        &self,
        url: &str,
        info: media::Info,
    ) -> Vec<MenuItem> {
        let info = std::sync::Arc::new(info);
        let private = self.current().private;
        let make = |label: String, icon: Icon, choice: media::Choice| {
            MenuItem::action(
                Some(Command::DownloadMedia(
                    url.to_owned(),
                    info.clone(),
                    choice,
                    private,
                )),
                label,
            )
            .icon(icon)
        };
        let sized = |label: String, size: Option<u64>| match size {
            Some(size) => format!("{label} · {}", crate::pages::file_size(size)),
            None => label,
        };
        // A video and the audio it's merged with, as a choice and its label.
        let video = |format: &media::Format, bundle: bool| {
            let audio = media::audio_for(&info, format);
            let size = match audio {
                Some(audio) => format.size().zip(audio.size()).map(|(a, b)| a + b),
                None => format.size(),
            };
            let mut label = format.quality();
            if let Some(audio) = audio {
                label = format!("{label} + {}", audio.quality());
            } else if !format.has_audio() {
                label = format!("{label}, no sound");
            }
            (
                sized(label, size),
                media::Choice::Video {
                    video: format.format_id.clone(),
                    audio: audio.map(|audio| audio.format_id.clone()),
                    bundle,
                },
            )
        };
        let mut items = Vec::new();
        let best_video = media::best_video(&info);
        if let Some(format) = best_video {
            let (label, choice) = video(format, false);
            items.push(make(format!("Video ({label})"), Icon::Play, choice));
        }
        if let Some(format) = media::best_audio(&info) {
            items.push(make(
                format!("Audio ({})", sized(format.quality(), format.size())),
                Icon::Sound,
                media::Choice::Audio(format.format_id.clone()),
            ));
        }
        if items.is_empty() {
            items.push(
                MenuItem::action(None, "No video or audio found")
                    .disabled()
                    .icon(Icon::Play),
            );
        }
        items.push(MenuItem::separator());
        let videos: Vec<_> = media::video_formats(&info)
            .into_iter()
            .map(|format| {
                let (label, choice) = video(format, false);
                make(label, Icon::Play, choice)
            })
            .collect();
        if !videos.is_empty() {
            items.push(MenuItem::submenu("Video Quality", videos).icon(Icon::Play));
        }
        let audios: Vec<_> = media::audio_formats(&info)
            .into_iter()
            .map(|format| {
                make(
                    sized(format.quality(), format.size()),
                    Icon::Sound,
                    media::Choice::Audio(format.format_id.clone()),
                )
            })
            .collect();
        if !audios.is_empty() {
            items.push(MenuItem::submenu("Audio Quality", audios).icon(Icon::Sound));
        }
        let subtitle = |lang: &String, automatic: bool| {
            make(lang.clone(), Icon::Captions, media::Choice::Subtitle(lang.clone(), automatic))
        };
        // YouTube's chat replay is listed among a stream's subtitles.
        let mut subtitles: Vec<_> = info
            .subtitles
            .keys()
            .filter(|lang| *lang != "live_chat")
            .map(|lang| subtitle(lang, false))
            .collect();
        // Machine translations into every language follow the original's
        // captions: those come first.
        let mut automatic: Vec<_> = info.automatic_captions.keys().collect();
        automatic.sort_by_key(|lang| !lang.ends_with("-orig"));
        let automatic: Vec<_> = automatic.into_iter().map(|lang| subtitle(lang, true)).collect();
        if !automatic.is_empty() {
            subtitles.push(MenuItem::submenu("Automatic", automatic).icon(Icon::Captions));
        }
        if !subtitles.is_empty() {
            items.push(MenuItem::submenu("Subtitles", subtitles).icon(Icon::Captions));
        }
        if info.description.as_deref().is_some_and(|text| !text.trim().is_empty()) {
            items.push(make("Description".into(), Icon::File, media::Choice::Description));
        }
        if let Some(format) = best_video {
            let (_, choice) = video(format, true);
            items.push(MenuItem::separator());
            items.push(make(
                "Video, Subtitles and Description in a Folder".into(),
                Icon::Folder,
                choice,
            ));
        }
        items
    }

    /// What a toolbar button's own menu offers: only what that button
    /// does, and for most, nothing.
    pub(crate) fn toolbar_item_menu(&self, item: ToolbarItem) -> Vec<MenuItem> {
        let web = self.current().page == Page::Web;
        match item {
            ToolbarItem::Reload => vec![
                MenuItem::action(
                    Some(Command::Reload),
                    "Reload Page",
                )
                .enabled(web)
                .icon(Icon::Reload),
                MenuItem::action(
                    Some(Command::EraseCacheAndReload),
                    "Erase Cache and Reload",
                )
                .enabled(web)
                .icon(Icon::Trash),
            ],
            _ => Vec::new(),
        }
    }

    /// For the toolbar itself, between its buttons.
    pub(crate) fn toolbar_menu(&self) -> Vec<MenuItem> {
        let mut items = Vec::new();
        items.push(
            MenuItem::action(Some(Command::Settings(Section::Toolbar)), "Customize Toolbar…")
                .icon(Icon::Sliders),
        );
        items.push(MenuItem::action(Some(Command::ResetToolbar), "Reset Toolbar").icon(Icon::Undo));
        items.push(MenuItem::separator());
        items.push(
            MenuItem::action(Some(Command::ToggleBookmarksBar), "Bookmarks Bar")
                .checked(self.bookmarks_bar)
                .icon(Icon::Bookmark),
        );
        items.push(
            MenuItem::action(Some(Command::ToggleVerticalTabs), "Vertical Tabs")
                .checked(self.vertical_tabs)
                .icon(Icon::Sidebar),
        );
        items
    }

    /// For start-page tiles and history rows.
    pub(crate) fn link_menu(
        &self,
        url: &str,
        in_history: bool,
    ) -> Vec<MenuItem> {
        let url = url.to_owned();
        let mut items = open_entries(&url);
        if in_history {
            items.push(
                MenuItem::action(Some(Command::ForgetPage(url)), "Remove Page from History")
                    .icon(Icon::Trash),
            );
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_ping_the_matching_controls() {
        assert_eq!(
            ping_for_command(&Command::Back, true),
            Some(PingTarget::Toolbar(ToolbarItem::Back))
        );
        assert_eq!(
            ping_for_command(&Command::Forward, true),
            Some(PingTarget::Toolbar(ToolbarItem::Forward))
        );
        assert_eq!(
            ping_for_command(&Command::BookmarkPage, true),
            Some(PingTarget::BookmarkPage)
        );
        assert_eq!(ping_for_command(&Command::BookmarkPage, false), None);
        assert_eq!(
            ping_for_command(&Command::CopyLink, true),
            Some(PingTarget::CopyLink)
        );
        assert!(PingTarget::CopyLink.omnibox());
        assert!(PingTarget::CopyLink.toolbar(ToolbarItem::CopyLink));
        assert_eq!(ping_for_command(&Command::CopyLink, false), None);
    }

    #[test]
    fn clean_links_drop_tracking_but_keep_the_rest() {
        assert_eq!(
            clean_link("https://a.org/p?utm_source=x&id=7&fbclid=abc#top"),
            "https://a.org/p?id=7#top"
        );
        assert_eq!(
            clean_link("https://a.org/p?utm_medium=y"),
            "https://a.org/p"
        );
        assert_eq!(clean_link("not a url"), "not a url");
    }
}

/// A row of one of the browser's menus: a command to run, or `None` for a
/// row that only says something.
pub(crate) type MenuItem = vampir::MenuItem<Option<Command>>;

/// Where a link or bookmark can open, and copying it: the start of both
/// their menus.
pub(crate) fn open_entries(url: &str) -> Vec<MenuItem> {
    let open = |place| Some(Command::Open(url.to_owned(), place));
    vec![
        MenuItem::action(open(Place::Here), "Open").icon(Icon::Open),
        MenuItem::action(open(Place::NewTab), "Open in New Tab").icon(Icon::Plus),
        MenuItem::action(open(Place::BackgroundTab), "Open in Background Tab").icon(Icon::Tabs),
        MenuItem::action(open(Place::PrivateTab), "Open in Private Tab").icon(Icon::Private),
        MenuItem::separator(),
        MenuItem::action(Some(Command::Copy(url.to_owned())), "Copy Link").icon(Icon::Link),
    ]
}

/// Asks for one file to open, with `prompt` on the button. GPUI's picker
/// is the platform's own, so this works wherever the browser does.
pub(crate) async fn choose_file(cx: &mut gpui::AsyncWindowContext, prompt: &str) -> Option<std::path::PathBuf> {
    let asked = cx
        .update(|_, cx| {
            cx.prompt_for_paths(gpui::PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some(prompt.to_string().into()),
            })
        })
        .ok()?;
    asked.await.ok()?.ok()?.and_then(|paths| paths.into_iter().next())
}
