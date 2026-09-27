//! Everything the browser can be asked to do, from any direction — the
//! keyboard, the menu bar, a context menu, the tab switcher, a toolbar
//! button — as one [`Command`], carried out in one place.

use gpui::{ClipboardItem, Context, Pixels, Point, Window};

use crate::{
    Browser, Page, PingTarget, TabTarget, media,
    native::MenuEntry,
    settings::{StartSection, ToolbarItem},
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
    /// A submenu's rows' commands, in order; see [`submenu`]. Never run.
    Submenu(Vec<Option<Command>>),
    ForgetPage(String),
    ClearHistory,
    ClearBrowsingData,
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
            Command::Submenu(_) => {}
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
                self.history().save();
                cx.notify();
            }
            Command::ClearHistory => self.confirm_then(
                "Clear all history?".into(),
                "Every page you've visited is forgotten. This can't be undone.".into(),
                "Clear History",
                cx,
                |this, cx| {
                    this.history().clear();
                    this.history().save();
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
                let sender = self.common.anywhere.clone();
                cx.spawn_in(window, async move |_, _| {
                    let Some(path) = crate::native::choose_save_path("Vamprowser Data.zip") else {
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
                cx.spawn_in(window, async move |_, _| {
                    let Some(path) = crate::native::choose_file(&["zip"]) else {
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
            Command::ShowDownloads => self.open_page(Page::Downloads, window, cx),
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
                        let _ = crate::app_dialog::open(cx, self.controls.palette(), "Couldn't create download folder", error, "OK", crate::app_dialog::Kind::Alert);
                        return;
                    }
                };
                let id = self.downloads().started(url.clone(), destination.clone(), private);
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
        items: Vec<(MenuEntry, Option<Command>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries: Vec<MenuEntry> = items.iter().map(|(entry, _)| entry.clone()).collect();
        let receiver =
            crate::app_menu::open(cx, window, position, entries, self.controls.palette());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Some(chosen)) = receiver.recv().await else {
                return;
            };
            let Some(command) = flatten(items).into_iter().nth(chosen).flatten() else {
                return;
            };
            let _ = cx.update(|window, app| {
                this.update(app, |browser, cx| browser.run(command, window, cx))
            });
        })
        .detach();
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
        let receiver = crate::app_dialog::open(
            cx,
            self.controls.palette(),
            title,
            message,
            "OK",
            crate::app_dialog::Kind::Prompt(initial),
        );
        cx.spawn(async move |this, cx| {
            if let Ok(Some(answer)) = receiver.recv().await {
                let _ = this.update(cx, |browser, cx| answered(browser, answer, cx));
            }
        })
        .detach();
    }

    pub(crate) fn tab_menu(&self, id: u64) -> Vec<(MenuEntry, Option<Command>)> {
        let Some(index) = self.index_of(id) else {
            return Vec::new();
        };
        let tab = &self.tabs[index];
        let web = tab.page == Page::Web;
        let bookmarked = web && self.bookmarks().find_url(&tab.url).is_some();
        vec![
            (MenuEntry::item("New Tab"), Some(Command::NewTab)),
            (
                MenuEntry::item("New Private Tab"),
                Some(Command::NewPrivateTab),
            ),
            (MenuEntry::Separator, None),
            (entry("Reload Tab", web), Some(Command::ReloadTab(id))),
            (
                entry(if tab.muted { "Unmute Tab" } else { "Mute Tab" }, web),
                Some(Command::ToggleTabMute(id)),
            ),
            (
                MenuEntry::item("Duplicate Tab"),
                Some(Command::DuplicateTab(id)),
            ),
            (
                entry(
                    if bookmarked {
                        "Bookmarked"
                    } else {
                        "Bookmark Tab"
                    },
                    web && !bookmarked,
                ),
                Some(Command::BookmarkTab(id)),
            ),
            (
                entry("Copy Link", web),
                Some(Command::Copy(tab.url.clone())),
            ),
            (
                entry("Copy Link Without Tracking", web),
                Some(Command::Copy(clean_link(&tab.url))),
            ),
            (MenuEntry::Separator, None),
            (MenuEntry::item("Close Tab"), Some(Command::CloseTabId(id))),
            (
                entry("Close Other Tabs", self.tabs.len() > 1),
                Some(Command::CloseOtherTabs(id)),
            ),
            (
                entry("Close Tabs to the Right", index + 1 < self.tabs.len()),
                Some(Command::CloseTabsToRight(id)),
            ),
            (MenuEntry::Separator, None),
            (
                entry("Reopen Closed Tab", !self.recently_closed.is_empty()),
                Some(Command::ReopenClosedTab),
            ),
            (
                MenuEntry::checked("Vertical Tabs", self.vertical_tabs),
                Some(Command::ToggleVerticalTabs),
            ),
        ]
    }

    pub(crate) fn tab_strip_menu(&self) -> Vec<(MenuEntry, Option<Command>)> {
        vec![
            (MenuEntry::item("New Tab"), Some(Command::NewTab)),
            (
                MenuEntry::item("New Private Tab"),
                Some(Command::NewPrivateTab),
            ),
            (
                entry("Reopen Closed Tab", !self.recently_closed.is_empty()),
                Some(Command::ReopenClosedTab),
            ),
            (MenuEntry::Separator, None),
            (
                MenuEntry::checked("Vertical Tabs", self.vertical_tabs),
                Some(Command::ToggleVerticalTabs),
            ),
            (
                MenuEntry::checked("Bookmarks Bar", self.bookmarks_bar),
                Some(Command::ToggleBookmarksBar),
            ),
        ]
    }

    pub(crate) fn bookmarks_bar_menu(&self) -> Vec<(MenuEntry, Option<Command>)> {
        let web = self.current().page == Page::Web;
        vec![
            (
                entry("Bookmark This Page", web && !self.bookmarked()),
                Some(Command::BookmarkPage),
            ),
            (
                MenuEntry::item("New Folder…"),
                Some(Command::NewBookmarkFolder(None)),
            ),
            (MenuEntry::Separator, None),
            (
                MenuEntry::item("Manage Bookmarks…"),
                Some(Command::ShowBookmarks),
            ),
            (
                MenuEntry::item("Hide Bookmarks Bar"),
                Some(Command::ToggleBookmarksBar),
            ),
        ]
    }

    pub(crate) fn omnibox_menu(&self, clipboard: bool) -> Vec<(MenuEntry, Option<Command>)> {
        let tab = self.current();
        let web = tab.page == Page::Web;
        vec![
            (entry("Paste and Go", clipboard), Some(Command::PasteAndGo)),
            (MenuEntry::Separator, None),
            (entry("Copy Link", web), Some(Command::CopyLink)),
            (
                entry("Copy Link Without Tracking", web),
                Some(Command::CopyCleanLink),
            ),
            (
                entry("Copy Link as Markdown", web),
                Some(Command::CopyMarkdownLink),
            ),
            (
                entry("Copy Page Title", web),
                Some(Command::Copy(tab.title.clone())),
            ),
            (MenuEntry::Separator, None),
            match self.bookmarks().find_url(&tab.url) {
                Some(id) => (
                    entry("Delete Bookmark", web),
                    Some(Command::DeleteBookmark(id)),
                ),
                None => (
                    entry("Bookmark This Page", web),
                    Some(Command::BookmarkPage),
                ),
            },
            (
                MenuEntry::item("Settings: Search Engine…"),
                Some(Command::Settings(Section::Search)),
            ),
        ]
    }

    /// What a toolbar button's own menu offers: only what that button
    /// does, and for most, nothing.
    pub(crate) fn media_menu(
        &self,
        url: &str,
        info: media::Info,
    ) -> Vec<(MenuEntry, Option<Command>)> {
        let info = std::sync::Arc::new(info);
        let private = self.current().private;
        let make = |label: String, choice: media::Choice| {
            (
                MenuEntry::item(label),
                Some(Command::DownloadMedia(url.to_owned(), info.clone(), choice, private)),
            )
        };
        let mut items = vec![make(
            "Download bundle (folder)".into(),
            media::Choice::Bundle,
        )];
        if info.description.is_some() {
            items.push(make("Description only".into(), media::Choice::Description));
        }
        let videos: Vec<_> = info
            .formats
            .iter()
            .filter(|format| format.is_video())
            .map(|format| {
                make(
                    format.label(),
                    media::Choice::Format(format.format_id.clone()),
                )
            })
            .collect();
        let audios: Vec<_> = info
            .formats
            .iter()
            .filter(|format| format.is_audio())
            .map(|format| {
                make(
                    format.label(),
                    media::Choice::Format(format.format_id.clone()),
                )
            })
            .collect();
        if videos.is_empty() {
            items.push((MenuEntry::disabled("No video formats"), None));
        } else {
            items.push(submenu("Video formats", videos));
        }
        if audios.is_empty() {
            items.push((MenuEntry::disabled("No separate audio formats"), None));
        } else {
            items.push(submenu("Audio formats", audios));
        }
        let mut subtitles = Vec::new();
        for lang in info.subtitles.keys() {
            subtitles.push(make(
                lang.clone(),
                media::Choice::Subtitle(lang.clone(), false),
            ));
        }
        for lang in info.automatic_captions.keys() {
            subtitles.push(make(
                format!("{lang} (automatic)"),
                media::Choice::Subtitle(lang.clone(), true),
            ));
        }
        if !subtitles.is_empty() {
            items.push(submenu("Subtitles", subtitles));
        }
        items
    }

    pub(crate) fn toolbar_item_menu(&self, item: ToolbarItem) -> Vec<(MenuEntry, Option<Command>)> {
        let web = self.current().page == Page::Web;
        match item {
            ToolbarItem::Reload => vec![
                (entry("Reload Page", web), Some(Command::Reload)),
                (
                    entry("Erase Cache and Reload", web),
                    Some(Command::EraseCacheAndReload),
                ),
            ],
            _ => Vec::new(),
        }
    }

    /// For the toolbar itself, between its buttons.
    pub(crate) fn toolbar_menu(&self) -> Vec<(MenuEntry, Option<Command>)> {
        let mut items = Vec::new();
        items.push((
            MenuEntry::item("Customize Toolbar…"),
            Some(Command::Settings(Section::Toolbar)),
        ));
        items.push((
            MenuEntry::item("Reset Toolbar"),
            Some(Command::ResetToolbar),
        ));
        items.push((MenuEntry::Separator, None));
        items.push((
            MenuEntry::checked("Bookmarks Bar", self.bookmarks_bar),
            Some(Command::ToggleBookmarksBar),
        ));
        items.push((
            MenuEntry::checked("Vertical Tabs", self.vertical_tabs),
            Some(Command::ToggleVerticalTabs),
        ));
        items
    }

    /// For start-page tiles and history rows.
    pub(crate) fn link_menu(
        &self,
        url: &str,
        in_history: bool,
    ) -> Vec<(MenuEntry, Option<Command>)> {
        let url = url.to_owned();
        let mut items = open_entries(&url);
        if in_history {
            items.push((
                MenuEntry::item("Remove Page from History"),
                Some(Command::ForgetPage(url)),
            ));
        }
        items
    }
}

pub(crate) fn entry(label: &str, enabled: bool) -> MenuEntry {
    if enabled {
        MenuEntry::item(label)
    } else {
        MenuEntry::disabled(label)
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

/// Where a link or bookmark can open, and copying it: the start of both
/// their menus.
pub(crate) fn open_entries(url: &str) -> Vec<(MenuEntry, Option<Command>)> {
    let open = |place| Some(Command::Open(url.to_owned(), place));
    vec![
        (MenuEntry::item("Open"), open(Place::Here)),
        (MenuEntry::item("Open in New Tab"), open(Place::NewTab)),
        (
            MenuEntry::item("Open in Background Tab"),
            open(Place::BackgroundTab),
        ),
        (
            MenuEntry::item("Open in Private Tab"),
            open(Place::PrivateTab),
        ),
        (MenuEntry::Separator, None),
        (
            MenuEntry::item("Copy Link"),
            Some(Command::Copy(url.to_owned())),
        ),
    ]
}

/// A row that opens another menu of `items`. Its entries' commands ride
/// along in [`Command::Submenu`], in the order the popup numbers them.
pub(crate) fn submenu(
    label: impl Into<String>,
    items: Vec<(MenuEntry, Option<Command>)>,
) -> (MenuEntry, Option<Command>) {
    let entries = items.iter().map(|(entry, _)| entry.clone()).collect();
    (
        MenuEntry::Submenu {
            label: label.into(),
            entries,
        },
        Some(Command::Submenu(flatten(items))),
    )
}

/// Every row's command in reading order, submenus' rows after their own:
/// the Vampir menu reports choices in.
fn flatten(items: Vec<(MenuEntry, Option<Command>)>) -> Vec<Option<Command>> {
    let mut out = Vec::new();
    for (entry, command) in items {
        match (entry, command) {
            (MenuEntry::Submenu { .. }, Some(Command::Submenu(inner))) => {
                out.push(None);
                out.extend(inner);
            }
            (_, command) => out.push(command),
        }
    }
    out
}
