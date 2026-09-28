//! The browser's own pages, drawn natively rather than as web pages: the
//! start page, settings, and downloads — and the download shelf along the
//! bottom of the window, as old Chrome had it.

use std::{
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex, PoisonError},
    time::Duration,
};

use gpui::{
    Animation, AnimationExt, AnyElement, Context, Div, Entity, ExternalDragPayload, FileDragPaths,
    FontWeight, MouseButton, MouseDownEvent, Render, SharedString, Stateful, Window, div, img,
    prelude::*, px,
};
use vampir::{ButtonVariant, Palette, TextInput, WidgetContext, color, lighting};

use crate::{
    Browser, BrowserEvent, Chrome, Page, SettingsInputs, WithHint,
    commands::{Command, Place, Section},
    downloads::DownloadState,
    history::Visit,
    icons::{Icon, icon},
    interop,
    native::{self, MenuEntry},
    settings::{
        self, NewTabPage, PopupPolicy, Protection, SchemeChoice, Settings, SitePermission,
        StartSection, Startup, TabPlacement, TintMode, ToolbarItem, UserAgent,
    },
    site_icon, site_name, slowed,
    state::now_secs,
};

/// A text field on the settings page, saved when submitted or on "Set".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    HomePage,
    CustomSearch,
    Instance,
    DownloadDir,
    UserAgent,
    ExtensionSource,
}

const ZOOMS: [f64; 13] = [
    0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
];
const INTENSITIES: [(f64, &str); 3] = [(0.6, "Subtle"), (1.0, "Normal"), (1.6, "Vivid")];
const SWATCH_HUES: [f64; 12] = [
    0.0, 30.0, 60.0, 95.0, 130.0, 165.0, 200.0, 235.0, 268.0, 295.0, 325.0, 350.0,
];

struct DraggedDownload {
    name: SharedString,
    palette: Palette,
}

impl Render for DraggedDownload {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(7.0))
            .bg(Chrome::new(self.palette).raised)
            .border_1()
            .border_color(self.palette.accent)
            .shadow(lighting::raised(self.palette.is_dark))
            .text_size(px(12.5))
            .text_color(self.palette.text_primary)
            .child(self.name.clone())
    }
}

impl SettingsInputs {
    /// The text field behind a settings field.
    fn get(&self, field: Field) -> &Entity<TextInput> {
        match field {
            Field::HomePage => &self.home_page,
            Field::CustomSearch => &self.custom_search,
            Field::Instance => &self.instance,
            Field::DownloadDir => &self.download_dir,
            Field::UserAgent => &self.user_agent,
            Field::ExtensionSource => &self.extension_source,
        }
    }
}

/// "3 minutes ago", "yesterday"…
fn ago(then: u64) -> String {
    let seconds = now_secs().saturating_sub(then);
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

fn day_label(then: u64) -> String {
    let days = now_secs().saturating_sub(then) / 86_400;
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..7 => format!("{days} days ago"),
        _ => format!("{} weeks ago", days / 7),
    }
}

fn heading(text: &str, palette: Palette) -> Div {
    div()
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(palette.text_secondary)
        .child(text.to_uppercase())
}

/// A heading over what it names, `gap` apart.
fn section(title: &str, gap: f32, palette: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(gap))
        .child(heading(title, palette))
}

/// Today's date, spelled out. The start page draws it on every frame while
/// anything moves, so the system is asked once a quarter hour; every time
/// zone is a whole number of quarter hours from UTC, so the date can't
/// change within one.
fn long_date() -> SharedString {
    static CACHE: Mutex<Option<(u64, SharedString)>> = Mutex::new(None);
    let quarter = now_secs() / 900;
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    match &*cache {
        Some((at, date)) if *at == quarter => date.clone(),
        _ => {
            let date = SharedString::from(native::long_date());
            *cache = Some((quarter, date.clone()));
            date
        }
    }
}

/// Opens `url` on a click, in a background tab on ⌘-click or a middle
/// click, as links on a page do.
fn opens_link(el: Stateful<Div>, url: SharedString, cx: &mut Context<Browser>) -> Stateful<Div> {
    let middle = url.clone();
    el.on_click(
        cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
            let place = if event.modifiers().platform {
                Place::BackgroundTab
            } else {
                Place::Here
            };
            this.open_link(&url, place, window, cx)
        }),
    )
    .on_mouse_up(
        MouseButton::Middle,
        cx.listener(move |this, _, window, cx| {
            this.open_link(&middle, Place::BackgroundTab, window, cx)
        }),
    )
}

/// A white (or lifted) panel holding a group of rows.
fn card(palette: Palette) -> Div {
    let chrome = Chrome::new(palette);
    div()
        .flex()
        .flex_col()
        .rounded(px(12.0))
        .bg(chrome.raised)
        .border_1()
        .border_color(color::with_alpha(chrome.line, 0.8))
        .shadow(lighting::raised(palette.is_dark))
        .px(px(16.0))
}

/// A settings row: a title and an optional explanation on the left, the
/// control on the right.
fn row(title: &str, detail: Option<&str>, control: AnyElement, palette: Palette) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(20.0))
        .py(px(12.0))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_color(palette.text_primary)
                        .child(title.to_owned()),
                )
                .when_some(detail, |el, detail| {
                    el.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(palette.text_secondary)
                            .child(detail.to_owned()),
                    )
                }),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// Rows with hairlines between them.
fn rows(items: Vec<AnyElement>, palette: Palette) -> Div {
    let line = Chrome::new(palette).line;
    let count = items.len();
    let mut out = card(palette);
    for (index, item) in items.into_iter().enumerate() {
        out = out.child(item);
        if index + 1 < count {
            out = out.child(div().h(px(1.0)).bg(color::with_alpha(line, 0.7)));
        }
    }
    out
}

impl Browser {
    // ---- Controls bound to settings ----------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn toggle(
        &mut self,
        id: &'static str,
        title: &str,
        detail: Option<&str>,
        value: bool,
        palette: Palette,
        cx: &mut Context<Self>,
        set: impl Fn(&mut Settings, bool) + 'static,
    ) -> AnyElement {
        let control = vampir::switch(
            id,
            value,
            None,
            true,
            WidgetContext::new(palette, self, cx),
            move |this, on, _, cx| {
                set(&mut this.settings, on);
                this.save_settings(cx);
            },
        )
        .into_any_element();
        row(title, detail, control, palette)
    }

    #[allow(clippy::too_many_arguments)]
    fn choose<T: Copy + PartialEq + 'static>(
        &mut self,
        id: &'static str,
        title: &str,
        detail: Option<&str>,
        options: &[(T, &str)],
        current: T,
        palette: Palette,
        cx: &mut Context<Self>,
        set: impl Fn(&mut Browser, T, &mut Context<Browser>) + 'static,
    ) -> AnyElement {
        let labels: Vec<String> = options.iter().map(|(_, l)| (*l).to_owned()).collect();
        let values: Vec<T> = options.iter().map(|(v, _)| *v).collect();
        let selected = values.iter().position(|v| *v == current).unwrap_or(0);
        let control = if labels.len() <= 4 {
            vampir::segmented(
                id,
                &labels,
                selected,
                true,
                WidgetContext::new(palette, self, cx),
                move |this, index, _, cx| {
                    if let Some(value) = values.get(index) {
                        set(this, *value, cx);
                    }
                },
            )
            .into_any_element()
        } else {
            vampir::combo(
                id,
                selected,
                &labels,
                Some(220.0),
                WidgetContext::new(palette, self, cx),
                move |this, index, _, cx| {
                    if let Some(value) = values.get(index) {
                        set(this, *value, cx);
                    }
                },
            )
            .into_any_element()
        };
        row(title, detail, control, palette)
    }

    /// [`Self::choose`] for a setting: `set` puts the choice in the
    /// settings, which are then saved.
    #[allow(clippy::too_many_arguments)]
    fn choose_setting<T: Copy + PartialEq + 'static>(
        &mut self,
        id: &'static str,
        title: &str,
        detail: Option<&str>,
        options: &[(T, &str)],
        current: T,
        palette: Palette,
        cx: &mut Context<Self>,
        set: impl Fn(&mut Settings, T) + 'static,
    ) -> AnyElement {
        self.choose(
            id,
            title,
            detail,
            options,
            current,
            palette,
            cx,
            move |this, value, cx| {
                set(&mut this.settings, value);
                this.save_settings(cx);
            },
        )
    }

    fn field(
        &mut self,
        field: Field,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = self.inputs.get(field).clone();
        div()
            .w(px(420.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(vampir::text_field(&input, palette, window, cx)),
            )
            // Buttons fill their container; this one sizes to its label.
            .child(div().flex_none().w(px(64.0)).child(vampir::button(
                ("set-field", field as usize),
                "Set",
                ButtonVariant::Soft,
                true,
                palette,
                cx,
                move |this, window, cx| this.save_field(field, window, cx),
            )))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn action(
        &mut self,
        id: &'static str,
        label: &str,
        variant: ButtonVariant,
        enabled: bool,
        palette: Palette,
        cx: &mut Context<Self>,
        command: Command,
    ) -> AnyElement {
        vampir::button(
            id,
            label,
            variant,
            enabled,
            palette,
            cx,
            move |this, window, cx| this.run(command.clone(), window, cx),
        )
        .into_any_element()
    }

    pub(crate) fn save_field(
        &mut self,
        field: Field,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.inputs.get(field).read(cx).content.trim().to_owned();
        match field {
            Field::HomePage => {
                self.settings.home_page = settings::destination(&text, &Settings::default());
                let home = self.settings.home_page.clone();
                self.inputs
                    .home_page
                    .update(cx, |input, cx| input.set_text(&home, cx));
            }
            Field::CustomSearch => self.settings.custom_search = text,
            Field::Instance => {
                let engine = self.settings.search_engine.clone();
                self.settings.instances.insert(engine, text);
            }
            Field::DownloadDir => self.settings.download_dir = text,
            Field::UserAgent => self.settings.custom_user_agent = text,
            Field::ExtensionSource => {
                self.install_extension(text, cx);
                return;
            }
        }
        self.notice = Some("Saved.".into());
        self.save_settings(cx);
    }

    /// Installs from a file path, an addons.mozilla.org link or an add-on
    /// name. Downloading, unpacking and patching happen off the main
    /// thread and come back as an `ExtensionPrepared` event.
    pub(crate) fn install_extension(&mut self, source: String, cx: &mut Context<Self>) {
        self.common.ensure_extensions();
        if self.common.extensions.borrow().is_none() {
            self.notice = Some("Extensions need macOS 15.4 or later.".into());
            cx.notify();
            return;
        }
        let source = source.trim().to_owned();
        if source.is_empty() {
            return;
        }
        use crate::extensions::Extensions;
        let expanded = settings::expand_home(&source);
        let sender = self.common.anywhere.clone();
        if expanded.exists() {
            self.notice = Some("Installing…".into());
            std::thread::spawn(move || {
                let result = Extensions::prepare(&expanded);
                let _ = sender.try_send(crate::BrowserEvent::ExtensionPrepared(result));
            });
        } else {
            self.notice = Some(format!("Downloading “{source}” from addons.mozilla.org…"));
            std::thread::spawn(move || {
                let result = Extensions::download_from_amo(&source).and_then(|path| {
                    let prepared = Extensions::prepare(&path);
                    let _ = std::fs::remove_file(path);
                    prepared
                });
                let _ = sender.try_send(crate::BrowserEvent::ExtensionPrepared(result));
            });
        }
        cx.notify();
    }

    /// Finishes an install begun by [`Browser::install_extension`].
    pub(crate) fn install_prepared_extension(
        &mut self,
        prepared: Result<crate::extensions::Prepared, String>,
        cx: &mut Context<Self>,
    ) {
        let mut extensions_guard = self.common.extensions.borrow_mut();
        let Some(extensions) = extensions_guard.as_mut() else {
            return;
        };
        match prepared.and_then(|prepared| extensions.install_prepared(prepared)) {
            Ok(Some(info)) => {
                self.notice = Some(format!("Installed {} {}.", info.name, info.version));
                self.inputs
                    .extension_source
                    .update(cx, |input, cx| input.set_text("", cx));
            }
            Ok(None) => {}
            Err(err) => self.notice = Some(format!("Couldn't install: {err}")),
        }
        cx.notify();
    }

    // ---- Start page ---------------------------------------------------------

    pub(crate) fn start_page(
        &mut self,
        palette: Palette,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = Chrome::new(palette);
        let hour = native::local_hour();
        let greeting = match hour {
            5..12 => "Good morning",
            12..18 => "Good afternoon",
            18..23 => "Good evening",
            _ => "Hello, night owl",
        };
        let private = self.current().private;
        let sections = self.settings.start_page_sections.clone();
        let mut column = div()
            .w_full()
            .max_w(px(880.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(34.0))
            .px(px(32.0))
            .pt(px(56.0))
            .pb(px(48.0))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(16.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(30.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if private { "Private browsing" } else { greeting }),
                            )
                            .child(
                                div()
                                    .text_color(palette.text_secondary)
                                    .child(if private {
                                        "Nothing from this tab is saved: no history, cookies or cache, and it won't come back after quitting.".into()
                                    } else {
                                        long_date()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("start-customize")
                            .size(px(32.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.0))
                            .cursor_pointer()
                            .text_size(px(20.0))
                            .text_color(palette.text_secondary)
                            .hover(move |s| s.bg(chrome.wash))
                            .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                                let items = StartSection::ALL
                                    .into_iter()
                                    .map(|section| (
                                        MenuEntry::checked(
                                            section.label(),
                                            this.settings.start_page_sections.shown(section),
                                        ),
                                        Some(Command::ToggleStartSection(section)),
                                    ))
                                    .collect();
                                this.context_menu(event.position(), items, window, cx);
                            }))
                            .child("•••")
                            .with_hint("Customize start page".into(), crate::hint::Side::Below, cx),
                    ),
            );
        let tile = |id: (&'static str, usize),
                    url: &str,
                    title: &str,
                    this: &Browser,
                    cx: &mut Context<Browser>| {
            let tile = div()
                .id(id)
                .w(px(104.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .child(
                    div()
                        .size(px(72.0))
                        .rounded(px(18.0))
                        .bg(chrome.raised)
                        .border_1()
                        .border_color(color::with_alpha(chrome.line, 0.8))
                        .shadow(lighting::raised(palette.is_dark))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(move |s| s.bg(palette.soft_fill))
                        .child(site_icon(
                            this.shown_favicon(url),
                            url,
                            34.0,
                            false,
                            palette,
                        )),
                )
                .child(
                    div()
                        .w_full()
                        .text_size(px(12.0))
                        .text_center()
                        .truncate()
                        .child(SharedString::from(title.to_owned())),
                );
            opens_link(tile, url.to_owned().into(), cx)
        };
        // Borrowed, not copied, for the length of the page: nothing drawing
        // it changes the bookmarks or history.
        let has_bookmarks = {
            let bookmarks = self.bookmarks();
            // The links on the bookmarks bar; folders have the bar and the
            // bookmark manager.
            let favorites: Vec<&crate::bookmarks::Node> = bookmarks
                .root()
                .iter()
                .filter(|node| !node.is_folder())
                .take(16)
                .collect();
            if sections.favorites && !favorites.is_empty() {
                let mut grid = div().flex().flex_wrap().gap(px(18.0));
                for bookmark in &favorites {
                    let id = bookmark.id;
                    let url = bookmark.url.as_deref().unwrap_or_default();
                    grid = grid.child(
                        tile(
                            ("start-favorite", id as usize),
                            url,
                            &bookmark.title,
                            self,
                            cx,
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                let items = this.bookmark_menu(id);
                                this.context_menu(event.position, items, window, cx);
                            }),
                        ),
                    );
                }
                column = column.child(section("Favorites", 14.0, palette).child(grid));
            }
            sections.favorites && !favorites.is_empty()
        };
        let mut has_frequent = false;
        if !private {
            let history = self.history();
            let frequent = if sections.frequent {
                history.frequent(8)
            } else {
                Vec::new()
            };
            has_frequent = !frequent.is_empty();
            if has_frequent {
                let mut grid = div().flex().flex_wrap().gap(px(18.0));
                for (index, visit) in frequent.into_iter().enumerate() {
                    let title = if visit.title.is_empty() {
                        site_name(&visit.url)
                    } else {
                        visit.title.clone()
                    };
                    let menu_url = visit.url.clone();
                    grid = grid.child(
                        tile(("start-frequent", index), &visit.url, &title, self, cx)
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    let items = this.link_menu(&menu_url, true);
                                    this.context_menu(event.position, items, window, cx);
                                }),
                            ),
                    );
                }
                column = column.child(section("Frequently visited", 14.0, palette).child(grid));
            }
            let recent = history.recent(8);
            if sections.recent && !recent.is_empty() {
                let mut list = card(palette).py(px(4.0));
                for (index, visit) in recent.iter().enumerate() {
                    list = list.child(self.history_row(
                        ("start-recent", index),
                        &visit.url,
                        &visit.title,
                        visit.last_visit,
                        palette,
                        cx,
                    ));
                }
                column = column.child(section("Recently visited", 14.0, palette).child(list));
            }
        }
        let closed: Vec<String> = self
            .recently_closed
            .iter()
            .rev()
            .filter(|c| c.page == Page::Web)
            .map(|c| c.url.clone())
            .take(5)
            .collect();
        if sections.closed && !closed.is_empty() {
            let mut list = card(palette).py(px(4.0));
            for (index, url) in closed.iter().enumerate() {
                list = list.child(self.history_row(
                    ("start-closed", index),
                    url,
                    &site_name(url),
                    0,
                    palette,
                    cx,
                ));
            }
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(heading("Recently closed", palette))
                            .child(div().flex_1())
                            .child(div().w(px(142.0)).flex_none().child(self.action(
                                "start-reopen",
                                "Reopen Last Tab",
                                ButtonVariant::Soft,
                                true,
                                palette,
                                cx,
                                Command::ReopenClosedTab,
                            ))),
                    )
                    .child(list),
            );
        }
        if !has_bookmarks && !has_frequent && !private && sections.favorites && sections.frequent {
            column = column.child(
                div()
                    .text_color(palette.text_secondary)
                    .child("Bookmark pages with ⌘D and they'll appear here, along with the sites you visit most. ⌘K finds any tab, bookmark or page."),
            );
        }
        div()
            .id("start-page")
            .size_full()
            .overflow_y_scroll()
            .bg(palette.backdrop)
            .flex()
            .justify_center()
            .child(column)
            .into_any_element()
    }

    /// A row linking to a page: its icon, title, address and when.
    fn history_row(
        &self,
        id: (&'static str, usize),
        url: &str,
        title: &str,
        when: u64,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = Chrome::new(palette);
        let in_history = when > 0;
        let icon_image = self.shown_favicon(url);
        let site = site_name(url);
        // One copy of the address, shared by every handler.
        let url = SharedString::from(url.to_owned());
        let menu = url.clone();
        let forget = url.clone();
        let row = div()
            .id(id)
            .group("history-row")
            .h(px(40.0))
            .mx(px(-8.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .hover(move |s| s.bg(chrome.wash));
        opens_link(row, url.clone(), cx)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let items = this.link_menu(&menu, in_history);
                    this.context_menu(event.position, items, window, cx);
                }),
            )
            .child(site_icon(icon_image, &url, 18.0, false, palette))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .child(if title.is_empty() {
                        url
                    } else {
                        SharedString::from(title.to_owned())
                    }),
            )
            .child(
                div()
                    .w(px(200.0))
                    .flex_none()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(palette.text_secondary)
                    .child(site),
            )
            .when(in_history, |el| {
                el.child(
                    div()
                        .w(px(84.0))
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(palette.text_secondary)
                        .child(ago(when)),
                )
                .child(
                    div()
                        .id((id.0, id.1 + 100_000))
                        .size(px(22.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .opacity(0.0)
                        .group_hover("history-row", |s| s.opacity(1.0))
                        .hover(move |s| s.bg(palette.row_hover))
                        .child(icon(Icon::Close, 10.0, palette.text_secondary))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.run(Command::ForgetPage(forget.to_string()), window, cx)
                        })),
                )
            })
            .into_any_element()
    }

    // ---- Settings -----------------------------------------------------------

    pub(crate) fn settings_page(
        &mut self,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = Chrome::new(palette);
        let mut nav = div()
            .w(px(210.0))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(12.0))
            .pt(px(28.0))
            .child(
                div()
                    .px(px(10.0))
                    .pb(px(14.0))
                    .text_size(px(20.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Settings"),
            );
        for section in Section::ALL {
            let active = section == self.settings_section;
            nav = nav.child(
                div()
                    .id(("settings-nav", section as usize))
                    .h(px(32.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .text_color(if active {
                        palette.text_primary
                    } else {
                        palette.text_secondary
                    })
                    .when(active, |el| {
                        el.bg(chrome.raised)
                            .shadow(lighting::raised(palette.is_dark))
                            .font_weight(FontWeight::MEDIUM)
                    })
                    .when(!active, |el| el.hover(move |s| s.bg(chrome.wash)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_section = section;
                        this.notice = None;
                        let text = this.address_text(this.selected);
                        this.address
                            .update(cx, |input, cx| input.set_text(&text, cx));
                        cx.notify();
                    }))
                    .child(section.label()),
            );
        }
        let body = match self.settings_section {
            Section::General => self.general_settings(palette, window, cx),
            Section::Appearance => self.appearance_settings(palette, cx),
            Section::Tabs => self.tab_settings(palette, cx),
            Section::Search => self.search_settings(palette, window, cx),
            Section::Privacy => self.privacy_settings(palette, cx),
            Section::History => self.history_settings(palette, window, cx),
            Section::Downloads => self.download_settings(palette, window, cx),
            Section::Toolbar => self.toolbar_settings(palette, cx),
            Section::Extensions => self.extension_settings(palette, window, cx),
            Section::Advanced => self.advanced_settings(palette, window, cx),
            Section::About => self.about(palette, cx),
        };
        let notice = self.notice.clone();
        div()
            .size_full()
            .flex()
            .bg(palette.backdrop)
            .child(nav)
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .max_w(px(760.0))
                            .flex()
                            .flex_col()
                            .gap(px(18.0))
                            .px(px(28.0))
                            .pt(px(30.0))
                            .pb(px(48.0))
                            .child(
                                div()
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.settings_section.label()),
                            )
                            .when_some(notice, |el, notice| {
                                el.child(
                                    div()
                                        .px(px(12.0))
                                        .py(px(8.0))
                                        .rounded(px(8.0))
                                        .bg(palette.soft_fill)
                                        .text_color(palette.soft_label)
                                        .child(notice),
                                )
                            })
                            .child(body),
                    ),
            )
            .into_any_element()
    }

    fn general_settings(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let default = self.is_default_browser;
        let make_default = self.action(
            "make-default",
            if default {
                "Vamprowser is your default browser"
            } else {
                "Make Default"
            },
            ButtonVariant::Primary,
            !default,
            palette,
            cx,
            Command::MakeDefaultBrowser,
        );
        let startup = self.choose_setting(
            "startup",
            "When Vamprowser opens",
            Some("⌘⇧T brings back the windows that were open when it quit."),
            &[
                (Startup::Restore, "Restore tabs"),
                (Startup::Home, "Home page"),
                (Startup::StartPage, "Start page"),
            ],
            self.settings.startup,
            palette,
            cx,
            |s, value| s.startup = value,
        );
        let home = self.field(Field::HomePage, palette, window, cx);
        // The web page seen last: while settings show, the current tab is
        // this one.
        let current_url = self
            .tabs
            .iter()
            .filter(|tab| tab.page == Page::Web && !tab.private)
            .max_by_key(|tab| tab.last_active)
            .map(|tab| tab.url.clone());
        let use_current = vampir::button(
            "home-use-current",
            "Use Current Page",
            ButtonVariant::Soft,
            current_url.is_some(),
            palette,
            cx,
            move |this, _, cx| {
                if let Some(url) = &current_url {
                    this.settings.home_page = url.clone();
                    let url = url.clone();
                    this.inputs
                        .home_page
                        .update(cx, |input, cx| input.set_text(&url, cx));
                    this.save_settings(cx);
                }
            },
        )
        .into_any_element();
        let new_tabs = self.choose_setting(
            "new-tab-page",
            "New tabs show",
            None,
            &[
                (NewTabPage::StartPage, "Start page"),
                (NewTabPage::Home, "Home page"),
                (NewTabPage::Blank, "Blank page"),
            ],
            self.settings.new_tab_page,
            palette,
            cx,
            |s, value| s.new_tab_page = value,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(rows(
                vec![row(
                    "Default browser",
                    Some("Open links from other apps, Mail and Messages here."),
                    make_default,
                    palette,
                )],
                palette,
            ))
            .child(rows(
                vec![
                    startup,
                    row("Home page", None, home, palette),
                    row("Set to the page you're on", None, use_current, palette),
                    new_tabs,
                ],
                palette,
            ))
            .into_any_element()
    }

    fn appearance_settings(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.choose_setting(
            "scheme",
            "Theme",
            None,
            &[
                (SchemeChoice::System, "System"),
                (SchemeChoice::Light, "Light"),
                (SchemeChoice::Dark, "Dark"),
            ],
            self.settings.scheme,
            palette,
            cx,
            |s, value| s.scheme = value,
        );
        let tint = self.choose_setting(
            "tint",
            "Window color",
            Some("From page takes the active site's colour, or its icon's."),
            &[
                (TintMode::Page, "From page"),
                (TintMode::Fixed, "Fixed"),
                (TintMode::Neutral, "Neutral"),
            ],
            self.settings.tint,
            palette,
            cx,
            |s, value| s.tint = value,
        );
        let intensity = {
            let current = INTENSITIES
                .iter()
                .min_by(|a, b| {
                    (a.0 - self.settings.intensity)
                        .abs()
                        .total_cmp(&(b.0 - self.settings.intensity).abs())
                })
                .map_or(1.0, |i| i.0);
            let options: Vec<(u64, &str)> =
                INTENSITIES.iter().map(|(v, l)| (v.to_bits(), *l)).collect();
            self.choose_setting(
                "intensity",
                "Color intensity",
                None,
                &options,
                current.to_bits(),
                palette,
                cx,
                |s, bits| s.intensity = f64::from_bits(bits),
            )
        };
        let mut items = vec![theme, tint];
        if self.settings.tint == TintMode::Fixed {
            let mut swatches = div().flex().gap(px(8.0));
            for (index, hue) in SWATCH_HUES.into_iter().enumerate() {
                let chosen = (self.settings.fixed_hue - hue).abs() < 1.0;
                swatches = swatches.child(
                    div()
                        .id(("swatch", index))
                        .size(px(22.0))
                        .rounded_full()
                        .bg(color::oklch_to_color(0.66, 0.14, hue))
                        .border_2()
                        .border_color(if chosen {
                            palette.text_primary
                        } else {
                            Palette::transparent()
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.fixed_hue = hue;
                            this.save_settings(cx);
                        })),
                );
            }
            items.push(row("Color", None, swatches.into_any_element(), palette));
        }
        items.push(intensity);
        let show_icons = self.settings.show_favicons;
        items.push(self.toggle(
            "show-favicons",
            "Show site icons",
            Some("Otherwise tabs and bookmarks show a letter."),
            show_icons,
            palette,
            cx,
            |s, on| s.show_favicons = on,
        ));
        let zoom_options: Vec<(u64, String)> = ZOOMS
            .iter()
            .map(|z| (z.to_bits(), format!("{:.0}%", z * 100.0)))
            .collect();
        let zoom_refs: Vec<(u64, &str)> =
            zoom_options.iter().map(|(v, l)| (*v, l.as_str())).collect();
        let current_zoom = ZOOMS
            .iter()
            .min_by(|a, b| {
                (*a - self.settings.page_zoom)
                    .abs()
                    .total_cmp(&(*b - self.settings.page_zoom).abs())
            })
            .copied()
            .unwrap_or(1.0);
        items.push(self.choose(
            "page-zoom",
            "Default page zoom",
            Some("Per-tab zoom: ⌘+ and ⌘−; ⌘0 returns here."),
            &zoom_refs,
            current_zoom.to_bits(),
            palette,
            cx,
            |this, bits, cx| {
                let zoom = f64::from_bits(bits);
                this.settings.page_zoom = zoom;
                for tab in &mut this.tabs {
                    tab.zoom = zoom;
                    if let Some(view) = &tab.view {
                        let _ = view.zoom(zoom);
                    }
                }
                this.save_settings(cx);
            },
        ));
        let bar = self.bookmarks_bar;
        let bar_switch = vampir::switch(
            "bookmarks-bar-setting",
            bar,
            None,
            true,
            WidgetContext::new(palette, self, cx),
            |this, _, _, cx| this.toggle_bookmarks_bar(cx),
        )
        .into_any_element();
        items.push(row("Show bookmarks bar", Some("⌘⇧B"), bar_switch, palette));
        rows(items, palette).into_any_element()
    }

    fn tab_settings(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        #[derive(Clone, Copy, PartialEq)]
        enum Layout {
            Horizontal,
            Vertical,
            Rail,
        }
        let layout = match (self.vertical_tabs, self.compact_vertical_tabs) {
            (false, _) => Layout::Horizontal,
            (true, false) => Layout::Vertical,
            (true, true) => Layout::Rail,
        };
        let layout = self.choose(
            "tab-layout",
            "Tab layout",
            Some("⌘⇧L switches between tabs along the top and down the side."),
            &[
                (Layout::Horizontal, "Top"),
                (Layout::Vertical, "Sidebar"),
                (Layout::Rail, "Icon rail"),
            ],
            layout,
            palette,
            cx,
            |this, value, cx| {
                this.vertical_tabs = value != Layout::Horizontal;
                this.compact_vertical_tabs = value == Layout::Rail;
                this.reveal_selected = true;
                this.persist();
                cx.notify();
            },
        );
        let placement = self.choose_setting(
            "tab-placement",
            "Open new tabs",
            None,
            &[
                (TabPlacement::End, "At the end"),
                (TabPlacement::AfterCurrent, "Next to current"),
            ],
            self.settings.tab_placement,
            palette,
            cx,
            |s, value| s.tab_placement = value,
        );
        let popups = self.choose_setting(
            "popups",
            "Links that open new windows",
            Some("Pop-ups become tabs; blocking stops them entirely."),
            &[
                (PopupPolicy::NewTab, "New tab"),
                (PopupPolicy::BackgroundTab, "Background tab"),
                (PopupPolicy::Block, "Block"),
            ],
            self.settings.popups,
            palette,
            cx,
            |s, value| s.popups = value,
        );
        let sleep = self.choose_setting(
            "sleep-tabs",
            "Unload tabs you haven't used in",
            Some("Frees their memory; a tab reloads when you go back to it. Tabs playing sound or video stay."),
            &[
                (15, "15 minutes"),
                (60, "An hour"),
                (240, "4 hours"),
                (0, "Never"),
            ],
            self.settings.sleep_tabs_after,
            palette,
            cx,
            |s, value| s.sleep_tabs_after = value,
        );
        let swipe = self.settings.swipe_navigation;
        let swipe = self.toggle(
            "swipe",
            "Swipe between pages",
            Some("Two-finger swipes go back and forward. Applies to new tabs."),
            swipe,
            palette,
            cx,
            |s, on| s.swipe_navigation = on,
        );
        rows(vec![layout, placement, popups, sleep, swipe], palette).into_any_element()
    }

    fn search_settings(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let engines: Vec<(usize, &str)> = settings::ENGINES
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.name))
            .collect();
        let current = settings::ENGINES
            .iter()
            .position(|e| e.id == self.settings.search_engine)
            .unwrap_or(0);
        let engine_row = self.choose(
            "search-engine",
            "Search engine",
            Some("Used for anything typed in the address field that isn't an address."),
            &engines,
            current,
            palette,
            cx,
            |this, index, cx| {
                let engine = &settings::ENGINES[index];
                this.settings.search_engine = engine.id.to_owned();
                let instance = this
                    .settings
                    .instances
                    .get(engine.id)
                    .cloned()
                    .unwrap_or_default();
                this.inputs
                    .instance
                    .update(cx, |input, cx| input.set_text(&instance, cx));
                this.save_settings(cx);
            },
        );
        let mut items = vec![engine_row];
        let engine = settings::engine(&self.settings.search_engine);
        if engine.is_some_and(|e| e.self_hosted) {
            let name = engine.map_or("", |e| e.name);
            let field = self.field(Field::Instance, palette, window, cx);
            items.push(row(
                &format!("{name} instance"),
                Some("The address of the server to search, like https://searx.example.org."),
                field,
                palette,
            ));
        }
        if engine.is_some_and(|e| e.id == "custom") {
            let field = self.field(Field::CustomSearch, palette, window, cx);
            items.push(row(
                "Search address",
                Some("Put %s where the search words go."),
                field,
                palette,
            ));
        }
        let mut keywords = div().flex().flex_wrap().gap(px(6.0)).py(px(12.0));
        for engine in settings::ENGINES.iter().filter(|e| e.id != "custom") {
            keywords = keywords.child(
                div()
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .bg(palette.soft_fill)
                    .text_size(px(11.5))
                    .text_color(palette.soft_label)
                    .child(format!("@{} {}", engine.keyword, engine.name)),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(rows(items, palette))
            .child(
                section("Search once with another engine", 8.0, palette)
                    .child(
                        div()
                            .text_color(palette.text_secondary)
                            .child("Type a keyword and your search in the address field, like “@k rust lifetimes” for Kagi. With DuckDuckGo, !bangs work too."),
                    )
                    .child(card(palette).child(keywords)),
            )
            .into_any_element()
    }

    fn privacy_settings(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let protection_now = self.settings.protection;
        let protection = self.choose_setting(
            "protection",
            "Tracking protection",
            Some(match protection_now {
                Protection::Off => "Nothing is blocked.",
                Protection::Standard => "Analytics and cross-site trackers are blocked on other sites.",
                Protection::Strict => "Trackers, ad networks and every third-party cookie are blocked. Some sites may break.",
            }),
            &[
                (Protection::Off, "Off"),
                (Protection::Standard, "Standard"),
                (Protection::Strict, "Strict"),
            ],
            protection_now,
            palette,
            cx,
            |s, value| s.protection = value,
        );
        let cookies = self.toggle(
            "third-party-cookies",
            "Block third-party cookies",
            Some("Stops sites embedded in others from recognising you across the web."),
            self.settings.block_third_party_cookies,
            palette,
            cx,
            |s, on| s.block_third_party_cookies = on,
        );
        let https = self.toggle(
            "https-only",
            "HTTPS-only mode",
            Some("Upgrades every address to a secure connection, except on your local network."),
            self.settings.https_only,
            palette,
            cx,
            |s, on| s.https_only = on,
        );
        let gpc = self.toggle(
            "gpc",
            "Tell sites not to sell or share my data",
            Some("Sends Global Privacy Control. Applies to new tabs."),
            self.settings.global_privacy_control,
            palette,
            cx,
            |s, on| s.global_privacy_control = on,
        );
        let history = self.toggle(
            "remember-history",
            "Remember browsing history",
            None,
            self.settings.remember_history,
            palette,
            cx,
            |s, on| s.remember_history = on,
        );
        let suggestions = self.toggle(
            "search-suggestions",
            "Search engine suggestions",
            Some("Sends what you type in the address field to your search engine, or to DuckDuckGo if it has no suggestions. Never from private tabs."),
            self.settings.search_suggestions,
            palette,
            cx,
            |s, on| s.search_suggestions = on,
        );
        let javascript = self.toggle(
            "javascript",
            "JavaScript",
            Some("Most sites need it. Applies to new tabs."),
            self.settings.javascript,
            palette,
            cx,
            |s, on| s.javascript = on,
        );
        let autoplay = self.toggle(
            "block-autoplay",
            "Block autoplaying media",
            Some("Applies to new tabs."),
            self.settings.block_autoplay,
            palette,
            cx,
            |s, on| s.block_autoplay = on,
        );
        let permission_options: Vec<(SitePermission, &str)> = SitePermission::ALL
            .iter()
            .map(|p| (*p, p.label()))
            .collect();
        type Permission = fn(&mut Settings) -> &mut SitePermission;
        let pickers: [(&'static str, &str, SitePermission, Permission); 3] = [
            ("camera", "Camera", self.settings.camera, |s| &mut s.camera),
            ("microphone", "Microphone", self.settings.microphone, |s| {
                &mut s.microphone
            }),
            (
                "screen-capture",
                "Screen sharing",
                self.settings.screen_capture,
                |s| &mut s.screen_capture,
            ),
        ];
        let permissions: Vec<AnyElement> = pickers
            .into_iter()
            .map(|(id, title, current, setting)| {
                self.choose_setting(
                    id,
                    title,
                    None,
                    &permission_options,
                    current,
                    palette,
                    cx,
                    move |s, value| {
                        *setting(s) = value;
                    },
                )
            })
            .collect();
        let clear_data = self.action(
            "clear-data",
            "Clear…",
            ButtonVariant::Danger,
            true,
            palette,
            cx,
            Command::ClearBrowsingData,
        );
        let export = self.action(
            "export-data",
            "Export…",
            ButtonVariant::Soft,
            true,
            palette,
            cx,
            Command::ExportBrowserData,
        );
        let import = self.action(
            "import-data",
            "Import…",
            ButtonVariant::Soft,
            true,
            palette,
            cx,
            Command::ImportBrowserData,
        );
        let has_history = !self.history().is_empty();
        let clear_history = self.action(
            "clear-history-privacy",
            "Clear History",
            ButtonVariant::Danger,
            has_history,
            palette,
            cx,
            Command::ClearHistory,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(rows(vec![protection, cookies, https, gpc], palette))
            .child(rows(vec![javascript, autoplay], palette))
            .child(section("When a site asks for", 8.0, palette).child(rows(permissions, palette)))
            .child(rows(
                vec![
                    history,
                    suggestions,
                    row(
                        "Browsing history",
                        Some("Every page you've visited, kept on this Mac only."),
                        clear_history,
                        palette,
                    ),
                    row(
                        "Cookies and website data",
                        Some("Signs you out of sites and empties caches."),
                        clear_data,
                        palette,
                    ),
                ],
                palette,
            ))
            .child(
                section("Your data", 8.0, palette).child(rows(
                    vec![
                        row(
                            "Export browsing data",
                            Some("Logins and site data, bookmarks, history, settings and extensions, in one archive to bring back after reinstalling or on another Mac. All of it lives in ~/Library/Application Support/Vamprowser."),
                            export,
                            palette,
                        ),
                        row(
                            "Import browsing data",
                            Some("Replaces everything here with an exported archive, then relaunches."),
                            import,
                            palette,
                        ),
                    ],
                    palette,
                )),
            )
            .into_any_element()
    }

    fn history_settings(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.inputs.history_search.read(cx).content.clone();
        let search = vampir::search_field(
            "history-search",
            &self.inputs.history_search,
            palette,
            window,
            cx,
        )
        .into_any_element();
        let has_history = !self.history().is_empty();
        let clear = self.action(
            "clear-history",
            "Clear History",
            ButtonVariant::Danger,
            has_history,
            palette,
            cx,
            Command::ClearHistory,
        );
        // Borrowed for the length of the page rather than copied: drawing
        // it changes nothing in the history.
        let history = self.history();
        let visits: Vec<&Visit> = if query.trim().is_empty() {
            history.recent(300).iter().collect()
        } else {
            history.search(&query, 300)
        };
        let mut list = div().flex().flex_col().gap(px(10.0));
        if visits.is_empty() {
            list = list.child(div().text_color(palette.text_secondary).child(
                if query.trim().is_empty() {
                    "No history yet."
                } else {
                    "No pages match."
                },
            ));
        }
        let mut group = String::new();
        let mut current: Option<Div> = None;
        for (index, visit) in visits.into_iter().enumerate() {
            let label = day_label(visit.last_visit);
            if label != group {
                if let Some(done) = current.take() {
                    list = list.child(done);
                }
                list = list.child(heading(&label, palette).pt(px(6.0)));
                group = label;
                current = Some(card(palette).py(px(4.0)));
            }
            let entry = self.history_row(
                ("history", index),
                &visit.url,
                &visit.title,
                visit.last_visit,
                palette,
                cx,
            );
            current = current.map(|c| c.child(entry));
        }
        if let Some(done) = current {
            list = list.child(done);
        }
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(div().flex_1().child(search))
                    .child(clear),
            )
            .child(list)
            .into_any_element()
    }

    fn download_settings(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let field = self.field(Field::DownloadDir, palette, window, cx);
        let open = self.action(
            "open-downloads",
            "Open Folder",
            ButtonVariant::Soft,
            true,
            palette,
            cx,
            Command::OpenDownloads,
        );
        let page = self.action(
            "show-downloads",
            "Show Downloads",
            ButtonVariant::Soft,
            true,
            palette,
            cx,
            Command::ShowDownloads,
        );
        rows(
            vec![
                row(
                    "Save downloads to",
                    Some("A folder path; ~ is your home folder."),
                    field,
                    palette,
                ),
                row(
                    "Downloaded files",
                    None,
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(open)
                        .child(page)
                        .into_any_element(),
                    palette,
                ),
            ],
            palette,
        )
        .into_any_element()
    }

    fn toolbar_settings(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let entries = self.settings.toolbar.clone();
        let count = entries.len();
        let mut items = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            let item = entry.item;
            let arrows = div()
                .flex()
                .gap(px(4.0))
                .child(
                    crate::tool_button(
                        ("toolbar-up", index),
                        Icon::ChevronLeft,
                        "Move left",
                        26.0,
                        false,
                        palette,
                        cx,
                        move |this, _, cx| {
                            this.settings.move_toolbar_item(item, false);
                            this.save_settings(cx);
                        },
                    )
                    .when(index == 0, |el| el.opacity(0.3)),
                )
                .child(
                    crate::tool_button(
                        ("toolbar-down", index),
                        Icon::ChevronRight,
                        "Move right",
                        26.0,
                        false,
                        palette,
                        cx,
                        move |this, _, cx| {
                            this.settings.move_toolbar_item(item, true);
                            this.save_settings(cx);
                        },
                    )
                    .when(index + 1 == count, |el| el.opacity(0.3)),
                );
            let control: AnyElement = if item == ToolbarItem::Address {
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(arrows)
                    .child(
                        div()
                            .w(px(52.0))
                            .flex()
                            .justify_end()
                            .text_size(px(11.5))
                            .text_color(palette.text_secondary)
                            .child("Always"),
                    )
                    .into_any_element()
            } else {
                let switch = vampir::switch(
                    ("toolbar-shown", index),
                    entry.shown,
                    None,
                    true,
                    WidgetContext::new(palette, self, cx),
                    move |this, on, _, cx| {
                        if let Some(e) = this.settings.toolbar.iter_mut().find(|e| e.item == item) {
                            e.shown = on;
                        }
                        this.save_settings(cx);
                    },
                )
                .into_any_element();
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(arrows)
                    .child(div().w(px(52.0)).flex().justify_end().child(switch))
                    .into_any_element()
            };
            items.push(row(item.label(), None, control, palette));
        }
        let reset = self.action(
            "reset-toolbar",
            "Reset Toolbar",
            ButtonVariant::Soft,
            true,
            palette,
            cx,
            Command::ResetToolbar,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(
                div()
                    .text_color(palette.text_secondary)
                    .child("Choose which buttons the toolbar shows and in what order; items before the address field sit on its left. Right-click the toolbar for these options too."),
            )
            .child(rows(items, palette))
            .child(div().child(reset))
            .into_any_element()
    }

    fn extension_settings(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.common.ensure_extensions();
        let field = self.field(Field::ExtensionSource, palette, window, cx);
        let auto_update = self.toggle(
            "auto-update-extensions",
            "Keep extensions up to date",
            Some("Checks addons.mozilla.org once a day and installs new versions, keeping their settings."),
            self.settings.auto_update_extensions,
            palette,
            cx,
            |s, on| s.auto_update_extensions = on,
        );
        let check = self.action(
            "check-extension-updates",
            "Check Now",
            ButtonVariant::Soft,
            true,
            palette,
            cx,
            Command::CheckExtensionUpdates,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(rows(
                vec![
                    row(
                        "Install an extension",
                        Some("Firefox add-ons from addons.mozilla.org, or a .xpi/.zip file or unpacked folder."),
                        field,
                        palette,
                    ),
                    auto_update,
                    row("Check for updates", None, check, palette),
                ],
                palette,
            ))
            .child(self.installed_extensions(palette, cx))
            .into_any_element()
    }

    /// The installed extensions: each with its icon, what it is, anything
    /// that went wrong, and switches to turn it off or remove it.
    fn installed_extensions(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let extensions_guard = self.common.extensions.borrow();
        let Some(extensions) = extensions_guard.as_ref() else {
            return div()
                .text_color(palette.text_secondary)
                .child("This Mac's WebKit can't run extensions; they need macOS 15.4 or later.")
                .into_any_element();
        };
        let list = extensions.list();
        if list.is_empty() {
            return div()
                .text_color(palette.text_secondary)
                .child("No extensions yet. Try “darkreader” above for Dark Reader.")
                .into_any_element();
        }
        let mut items = Vec::new();
        for (index, info) in list.into_iter().enumerate() {
            let id = info.id.clone();
            let remove_id = info.id.clone();
            let remove_name = info.name.clone();
            // Moved out of the list, which is the page's own copy.
            let icon_image = info
                .icon_png
                .map(|png| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png)));
            let switch = vampir::switch(
                ("extension-enabled", index),
                info.enabled,
                None,
                true,
                WidgetContext::new(palette, self, cx),
                move |this, on, _, cx| {
                    if let Some(extensions) = this.common.extensions.borrow_mut().as_mut() {
                        extensions.set_enabled(&id, on);
                    }
                    cx.notify();
                },
            )
            .into_any_element();
            let remove = div()
                .flex_none()
                .child(vampir::button(
                    ("extension-remove", index),
                    "Remove",
                    ButtonVariant::Danger,
                    true,
                    palette,
                    cx,
                    move |this, _, cx| {
                        let id = remove_id.clone();
                        this.confirm_then(
                            format!("Remove “{remove_name}”?"),
                            "It's uninstalled, with everything it has stored.".into(),
                            "Remove",
                            cx,
                            move |this, cx| {
                                if let Some(extensions) =
                                    this.common.extensions.borrow_mut().as_mut()
                                {
                                    extensions.remove(&id);
                                }
                                this.refresh_other_windows(cx);
                                cx.notify();
                            },
                        );
                    },
                ))
                .into_any_element();
            let homepage = info.homepage;
            items.push(
                div()
                    .flex()
                    .gap(px(14.0))
                    .py(px(14.0))
                    .child(
                        div()
                            .size(px(36.0))
                            .flex_none()
                            .rounded(px(8.0))
                            .bg(palette.soft_fill)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(match icon_image {
                                Some(image) => img(image).size(px(28.0)).into_any_element(),
                                None => {
                                    icon(Icon::Puzzle, 20.0, palette.soft_label).into_any_element()
                                }
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.0))
                                    .items_baseline()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(info.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(palette.text_secondary)
                                            .child(if info.has_action {
                                                format!("{} · toolbar button", info.version)
                                            } else {
                                                info.version.clone()
                                            }),
                                    ),
                            )
                            .when(!info.description.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(palette.text_secondary)
                                        .line_clamp(2)
                                        .child(info.description.clone()),
                                )
                            })
                            .children(info.errors.iter().take(3).map(|error| {
                                div()
                                    .text_size(px(11.5))
                                    .text_color(palette.danger_label)
                                    .truncate()
                                    .child(SharedString::from(error.clone()))
                            }))
                            .when_some(homepage, |el, homepage| {
                                el.child(link(
                                    ("extension-home", index),
                                    "Website",
                                    palette,
                                    cx,
                                    Command::Open(homepage, Place::NewTab),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(switch)
                            .child(remove),
                    )
                    .into_any_element(),
            );
        }
        section("Installed", 8.0, palette)
            .child(rows(items, palette))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(palette.text_secondary)
                    .child("Extensions ask before getting access to sites or browser features, and don't run in private tabs. Changes apply to pages as they load."),
            )
            .into_any_element()
    }

    fn advanced_settings(
        &mut self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let agents: Vec<(UserAgent, &str)> =
            UserAgent::ALL.iter().map(|a| (*a, a.label())).collect();
        let agent = self.choose_setting(
            "user-agent",
            "Identify as",
            Some("The browser sites think you use. Applies to new tabs."),
            &agents,
            self.settings.user_agent,
            palette,
            cx,
            |s, value| s.user_agent = value,
        );
        let mut items = vec![agent];
        if self.settings.user_agent == UserAgent::Custom {
            let field = self.field(Field::UserAgent, palette, window, cx);
            items.push(row("Custom user agent", None, field, palette));
        }
        let inspector = self.settings.web_inspector;
        items.push(self.toggle(
            "web-inspector",
            "Web Inspector",
            Some("Adds Inspect Element to the page's right-click menu. Applies to new tabs."),
            inspector,
            palette,
            cx,
            |s, on| s.web_inspector = on,
        ));
        let reset = vampir::button(
            "reset-settings",
            "Reset All Settings",
            ButtonVariant::Danger,
            true,
            palette,
            cx,
            |this, _, cx| {
                this.confirm_then(
                    "Reset all settings?".into(),
                    "Every setting goes back to how it came. Bookmarks, history, logins and extensions stay.".into(),
                    "Reset",
                    cx,
                    |this, cx| {
                        this.settings = Settings::default();
                        this.notice = Some("Settings are back to their defaults.".into());
                        this.save_settings(cx);
                    },
                );
            },
        )
        .into_any_element();
        let data = vampir::button(
            "show-data",
            "Show in Finder",
            ButtonVariant::Soft,
            true,
            palette,
            cx,
            |_, _, _| {
                if let Some(dir) = crate::state::data_path("") {
                    interop::open_file(&dir);
                }
            },
        )
        .into_any_element();
        div()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(rows(items, palette))
            .child(rows(
                vec![
                    row(
                        "Browser data",
                        Some("Settings, bookmarks, history and session live in ~/Library/Application Support/Vamprowser."),
                        data,
                        palette,
                    ),
                    row("Reset", Some("Returns every setting to its default. Bookmarks and history stay."), reset, palette),
                ],
                palette,
            ))
            .into_any_element()
    }

    fn about(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let default = self.is_default_browser;
        let make_default = self.action(
            "about-default",
            if default {
                "Default Browser ✓"
            } else {
                "Make Default Browser"
            },
            ButtonVariant::Soft,
            !default,
            palette,
            cx,
            Command::MakeDefaultBrowser,
        );
        card(palette)
            .py(px(24.0))
            .items_center()
            .gap(px(10.0))
            .child(
                img(APP_ICON.clone())
                .size(px(96.0))
                .rounded(px(22.0)),
            )
            .child(
                div()
                    .text_size(px(22.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Vamprowser"),
            )
            .child(
                div()
                    .text_color(palette.text_secondary)
                    .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
            )
            .child(
                div()
                    .max_w(px(420.0))
                    .text_center()
                    .text_color(palette.text_secondary)
                    .child("A macOS browser with WebKit underneath and a GPUI interface on top, tinted by whatever you're looking at."),
            )
            .child(make_default)
            .into_any_element()
    }

    // ---- Downloads ----------------------------------------------------------

    /// A download's right-click menu, built when it's asked for, so the
    /// rows drawing it needn't keep a copy of the download.
    fn download_menu(&self, id: u64) -> Vec<(MenuEntry, Option<Command>)> {
        let downloads = self.downloads();
        let Some(item) = downloads.get(id) else {
            return Vec::new();
        };
        let exists = item.exists();
        let done = item.state == DownloadState::Done && exists;
        vec![
            (entry("Open", done), Some(Command::OpenDownload(item.id))),
            (
                entry("Show in Finder", exists),
                Some(Command::RevealDownload(item.id)),
            ),
            (
                MenuEntry::item("Copy Download Link"),
                Some(Command::Copy(item.url.clone())),
            ),
            (MenuEntry::Separator, None),
            (
                MenuEntry::item("Remove from List"),
                Some(Command::RemoveDownload(item.id)),
            ),
        ]
    }

    /// The indeterminate bar under a download in progress: WebKit doesn't
    /// report how far along it is.
    fn busy_bar(id: (&'static str, u64), width: f32, palette: Palette) -> AnyElement {
        div()
            .relative()
            .w(px(width))
            .h(px(3.0))
            .rounded(px(2.0))
            .overflow_hidden()
            .bg(palette.field_border)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .w(px(width * 0.35))
                    .rounded(px(2.0))
                    .bg(palette.accent)
                    .with_animation(
                        id,
                        Animation::new(slowed(Duration::from_millis(1100))).repeat(),
                        move |bar, t| bar.left(px(-width * 0.35 + t * width * 1.35)),
                    ),
            )
            .into_any_element()
    }

    /// Offer a finished download to macOS as a file when its drag leaves
    /// the window. Check the path then, since a file may have moved since
    /// the download was drawn.
    fn draggable_download(
        row: Stateful<Div>,
        path: PathBuf,
        palette: Palette,
        sender: async_channel::Sender<BrowserEvent>,
    ) -> Stateful<Div> {
        let name: SharedString = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
            .into();
        row.on_drag(path, move |_, _, _, cx| {
            let _ = sender.try_send(BrowserEvent::DownloadDragStarted);
            cx.new(|_| DraggedDownload {
                name: name.clone(),
                palette,
            })
        })
        .external_drag_payload(|path: &PathBuf, _, _| {
            let metadata = std::fs::metadata(path).ok()?;
            Some(ExternalDragPayload::Files(FileDragPaths::new([(
                path.clone(),
                metadata.is_dir(),
            )])))
        })
    }

    /// The shelf along the bottom of the window, as old Chrome had it:
    /// recent downloads, newest first, with "Show all" and a close button.
    pub(crate) fn download_shelf(
        &mut self,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = Chrome::new(palette);
        let scroll = self.controls.scroll("download-shelf-items");
        let downloads = self.downloads();
        let mut strip = div()
            .id("download-shelf-items")
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_x_scroll()
            .track_scroll(&scroll)
            .flex()
            .items_center()
            .gap(px(6.0));
        for item in downloads.shelf().take(8) {
            let id = item.id;
            let status = match item.state {
                DownloadState::InProgress => "Downloading…".to_owned(),
                DownloadState::Done => match item.file_size() {
                    Some(Some(size)) => file_size(size),
                    Some(None) => "Moved or deleted".to_owned(),
                    None => String::new(),
                },
                DownloadState::Failed => "Failed".to_owned(),
            };
            let chip = div()
                .id(("shelf-item", id))
                .w(px(240.0))
                .h(px(SHELF_ITEM_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(10.0))
                .rounded(px(8.0))
                .bg(chrome.raised)
                .border_1()
                .border_color(color::with_alpha(chrome.line, 0.8))
                .cursor_pointer()
                .hover(move |s| s.bg(palette.soft_fill))
                .when(item.state == DownloadState::Done, |row| {
                    Self::draggable_download(row, item.path.clone(), palette, self.sender.clone())
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run(Command::OpenDownload(id), window, cx)
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        let items = this.download_menu(id);
                        this.context_menu(event.position, items, window, cx);
                    }),
                )
                .child(icon(
                    Icon::File,
                    18.0,
                    if item.state == DownloadState::Failed {
                        palette.danger_label
                    } else {
                        palette.text_secondary
                    },
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .justify_center()
                        .gap(px(2.0))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.5))
                                .line_height(px(16.0))
                                .text_color(palette.text_primary)
                                .child(SharedString::from(item.name())),
                        )
                        .child(if item.state == DownloadState::InProgress {
                            div()
                                .h(px(14.0))
                                .flex()
                                .items_center()
                                .child(Self::busy_bar(("shelf-busy", id), 150.0, palette))
                                .into_any_element()
                        } else {
                            div()
                                .h(px(14.0))
                                .text_size(px(11.0))
                                .line_height(px(14.0))
                                .text_color(palette.text_secondary)
                                .child(status)
                                .into_any_element()
                        }),
                )
                .child(
                    div()
                        .id(("shelf-item-menu", id))
                        .w(px(20.0))
                        .h(px(28.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.0))
                        .cursor_pointer()
                        .hover(move |button| button.bg(chrome.wash))
                        .on_click(
                            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                                cx.stop_propagation();
                                let items = this.download_menu(id);
                                this.context_menu(event.position(), items, window, cx);
                            }),
                        )
                        .child(icon(Icon::ChevronDown, 12.0, palette.text_secondary)),
                );
            strip = strip.child(
                div()
                    .w(px(240.0))
                    .h(px(SHELF_ITEM_HEIGHT))
                    .flex_none()
                    .overflow_hidden()
                    .child(chip)
                    .with_animation(
                        ("shelf-item-enter", id),
                        Animation::new(slowed(Duration::from_millis(220)))
                            .with_easing(|t: f32| 1.0 - (1.0 - t).powi(3)),
                        |wrapper, t| wrapper.w(px(240.0 * t)).opacity(t),
                    ),
            );
        }
        drop(downloads);
        div()
            .w_full()
            .h(px(crate::SHELF_HEIGHT))
            .flex()
            .items_center()
            .gap(px(10.0))
            // Clear of the window's rounded corners.
            .pl(px(14.0))
            .pr(px(10.0))
            .border_t_1()
            .border_color(chrome.line)
            .bg(chrome.ground)
            .child(
                self.faded(
                    "download-shelf",
                    strip,
                    &scroll,
                    crate::ScrollAxis::Horizontal,
                    chrome.ground,
                )
                .flex_1()
                .min_w(px(0.0))
                .h_full(),
            )
            .child(
                div()
                    .id("shelf-show-all")
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .h(px(28.0))
                    .rounded(px(7.0))
                    .text_size(px(12.0))
                    .text_color(palette.accent)
                    .cursor_pointer()
                    .hover(move |s| s.bg(chrome.wash))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.run(Command::ShowDownloads, window, cx)
                    }))
                    .child(icon(Icon::Download, 13.0, palette.accent))
                    .child("Show all"),
            )
            .child(crate::tool_button(
                "shelf-close",
                Icon::Close,
                "Close",
                26.0,
                false,
                palette,
                cx,
                |this, _, cx| {
                    this.downloads().clear_shelf();
                    cx.notify();
                },
            ))
            .into_any_element()
    }

    /// The Downloads page, after old Chrome's: one column, grouped by day,
    /// each file with its source and plain-text actions.
    pub(crate) fn downloads_page(
        &mut self,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let downloads = self.downloads();
        let items = downloads.all();
        let mut column = div()
            .w_full()
            .max_w(px(720.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .px(px(32.0))
            .pt(px(40.0))
            .pb(px(48.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.0))
                    .pb(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(26.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Downloads"),
                    )
                    .child(link(
                        "dl-open-folder",
                        "Open downloads folder",
                        palette,
                        cx,
                        Command::OpenDownloads,
                    ))
                    .child(link(
                        "dl-clear",
                        "Clear all",
                        palette,
                        cx,
                        Command::ClearDownloads,
                    )),
            );
        if items.is_empty() {
            column = column.child(
                div()
                    .pt(px(40.0))
                    .text_center()
                    .text_color(palette.text_secondary)
                    .child("Files you download appear here."),
            );
        }
        let mut group = String::new();
        for item in items {
            let label = day_label(item.started);
            if label != group {
                column = column.child(heading(&label, palette).pt(px(10.0)));
                group = label;
            }
            let id = item.id;
            // The size, if the file is still there; while that's being
            // looked up, it counts as there.
            let looked = item.file_size();
            let size = looked.flatten();
            let exists = looked.is_none_or(|size| size.is_some());
            let (status, strike) = match item.state {
                DownloadState::InProgress => (Some("Downloading…"), false),
                DownloadState::Done if exists => (None, false),
                DownloadState::Done => (Some("Removed"), true),
                DownloadState::Failed => (Some("Failed"), true),
            };
            // One line under the name: where it came from, how big it is,
            // and what went wrong, if anything did.
            let mut details = vec![site_name(&item.url)];
            if let Some(size) = size.filter(|_| item.state == DownloadState::Done) {
                details.push(file_size(size));
            }
            details.extend(
                status
                    .filter(|_| item.state != DownloadState::InProgress)
                    .map(str::to_owned),
            );
            let details = details
                .into_iter()
                .filter(|d| !d.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            let ink = if strike {
                palette.text_secondary
            } else {
                palette.text_primary
            };
            column = column.child(
                card(palette)
                    .id(("download", id))
                    .py(px(12.0))
                    .flex_row()
                    .items_center()
                    .gap(px(14.0))
                    .when(item.state == DownloadState::Done, |row| {
                        Self::draggable_download(
                            row,
                            item.path.clone(),
                            palette,
                            self.sender.clone(),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let items = this.download_menu(id);
                            this.context_menu(event.position, items, window, cx);
                        }),
                    )
                    .child(
                        div()
                            .size(px(38.0))
                            .flex_none()
                            .rounded(px(9.0))
                            .bg(palette.soft_fill)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(
                                Icon::File,
                                20.0,
                                if item.state == DownloadState::Failed {
                                    palette.danger_label
                                } else {
                                    palette.soft_label
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .id(("download-name", id))
                                    .truncate()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(ink)
                                    .when(strike, |el| el.line_through())
                                    .when(!strike, |el| {
                                        el.cursor_pointer().hover(|s| s.underline()).on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.run(Command::OpenDownload(id), window, cx)
                                            }),
                                        )
                                    })
                                    .child(SharedString::from(item.name())),
                            )
                            .child(if item.state == DownloadState::InProgress {
                                div()
                                    .pt(px(4.0))
                                    .child(Self::busy_bar(("page-busy", id), 240.0, palette))
                                    .into_any_element()
                            } else {
                                div()
                                    .truncate()
                                    .text_size(px(11.5))
                                    .text_color(palette.text_secondary)
                                    .child(details)
                                    .into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(14.0))
                            .when(exists, |el| {
                                el.child(link(
                                    ("dl-reveal", id),
                                    "Show in Finder",
                                    palette,
                                    cx,
                                    Command::RevealDownload(id),
                                ))
                            })
                            .child(crate::tool_button(
                                ("dl-remove", id),
                                Icon::Close,
                                "Remove from list",
                                26.0,
                                false,
                                palette,
                                cx,
                                move |this, window, cx| {
                                    this.run(Command::RemoveDownload(id), window, cx)
                                },
                            )),
                    ),
            );
        }
        drop(downloads);
        div()
            .id("downloads-page")
            .size_full()
            .overflow_y_scroll()
            .bg(palette.backdrop)
            .flex()
            .justify_center()
            .child(column)
            .into_any_element()
    }

    pub(crate) fn download_command(&mut self, command: Command, cx: &mut Context<Self>) {
        match command {
            Command::OpenDownload(id) => {
                if let Some(item) = self.downloads().get(id)
                    && item.state == DownloadState::Done
                {
                    interop::open_file(&item.path);
                }
            }
            Command::RevealDownload(id) => {
                if let Some(item) = self.downloads().get(id) {
                    interop::show_in_finder(&item.path);
                }
            }
            Command::RemoveDownload(id) => self.downloads().remove(id),
            Command::ClearDownloads => self.downloads().clear(),
            _ => {}
        }
        cx.notify();
    }
}

/// The app's icon for the About section, decoded once rather than copied
/// on every frame.
static APP_ICON: LazyLock<Arc<gpui::Image>> = LazyLock::new(|| {
    Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        crate::ICON.bytes().to_vec(),
    ))
});

fn entry(label: &str, enabled: bool) -> MenuEntry {
    if enabled {
        MenuEntry::item(label)
    } else {
        MenuEntry::disabled(label)
    }
}

/// Each download's chip on the shelf, centred in its height.
const SHELF_ITEM_HEIGHT: f32 = 40.0;

/// A file's size as Finder shows it: decimal units, one decimal place.
fn file_size(bytes: u64) -> String {
    let bytes = bytes as f64;
    match bytes {
        b if b < 1e3 => format!("{b} bytes"),
        b if b < 1e6 => format!("{:.0} KB", b / 1e3),
        b if b < 1e9 => format!("{:.1} MB", b / 1e6),
        b => format!("{:.2} GB", b / 1e9),
    }
}

/// A text link that runs a command, as old Chrome's downloads page had.
fn link(
    id: impl Into<gpui::ElementId>,
    label: &str,
    palette: Palette,
    cx: &mut Context<Browser>,
    command: Command,
) -> AnyElement {
    div()
        .id(id)
        .text_size(px(12.0))
        .text_color(palette.accent)
        .cursor_pointer()
        .hover(|s| s.underline())
        .on_click(cx.listener(move |this, _, window, cx| this.run(command.clone(), window, cx)))
        .child(label.to_owned())
        .into_any_element()
}
