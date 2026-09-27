//! Bookmarks in the interface: the bar (folders open as menus), menus for
//! each bookmark, saving a page (⌘D) with a choice of folder, and the
//! bookmark manager (`vamp://bookmarks`): a folder tree beside the chosen
//! folder's contents, search across everything, and dragging to reorder
//! and file.

use gpui::{
    AnyElement, Bounds, Context, DispatchPhase, FontWeight, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, SharedString, Stateful,
    Window, canvas, div, point, prelude::*, px,
};
use vampir::{ButtonVariant, Palette, color, lighting};

use crate::{
    BOOKMARKS_HEIGHT, Browser, Chrome, Page, ScrollAxis,
    bookmarks::{Folder, Node, Source},
    commands::{Command, Place, submenu},
    icons::{Icon, icon},
    marquee, native,
    native::MenuEntry,
    settings, site_icon, site_name,
};

type MenuItems = Vec<(MenuEntry, Option<Command>)>;

/// What a row shows of a bookmark or folder: copied out each frame, so
/// drawing doesn't copy whole folders.
#[derive(Clone)]
struct Item {
    id: u64,
    title: String,
    url: Option<String>,
    /// For a folder, how much is in it.
    count: usize,
}

impl Item {
    fn of(node: &Node) -> Self {
        Self {
            id: node.id,
            title: node.title.clone(),
            url: node.url.clone(),
            count: node.children.len(),
        }
    }
}

/// Bookmarks being dragged (the one pressed on, or every one selected with
/// it), and what they look like under the pointer.
#[derive(Clone)]
pub(crate) struct DraggedBookmark {
    ids: Vec<u64>,
    title: SharedString,
    palette: Palette,
}

impl DraggedBookmark {
    pub(crate) fn new(id: u64, title: String, palette: Palette) -> Self {
        Self::of(vec![id], title, palette)
    }

    fn of(ids: Vec<u64>, title: String, palette: Palette) -> Self {
        let title = match ids.len() {
            0 | 1 => title,
            count => format!("{count} items"),
        };
        Self {
            ids,
            title: title.into(),
            palette,
        }
    }

    /// Everything dragged, in the order it was listed.
    pub(crate) fn ids(&self) -> &[u64] {
        &self.ids
    }
}

/// A box being dragged out over the bookmark manager's list, selecting what
/// it touches: where it started and where the pointer is, in the window,
/// and the selection it adds to (with ⌘ held).
#[derive(Clone)]
pub(crate) struct SelectionBand {
    start: Point<Pixels>,
    current: Point<Pixels>,
    base: Vec<u64>,
}

impl SelectionBand {
    fn bounds(&self) -> Bounds<Pixels> {
        Bounds::from_corners(
            point(self.start.x.min(self.current.x), self.start.y.min(self.current.y)),
            point(self.start.x.max(self.current.x), self.start.y.max(self.current.y)),
        )
    }
}

/// The manager's selection: which rows, in order, and what ⇧-click
/// extends from.
#[derive(Default)]
pub(crate) struct BookmarkSelection {
    ids: Vec<u64>,
    anchor: Option<u64>,
    band: Option<SelectionBand>,
    /// Each row as last drawn, in list order, for the band to hit.
    rows: std::rc::Rc<std::cell::RefCell<Vec<(u64, Bounds<Pixels>)>>>,
}

impl Render for DraggedBookmark {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let chrome = Chrome::new(self.palette);
        div()
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(7.0))
            .bg(chrome.raised)
            .border_1()
            .border_color(self.palette.accent)
            .shadow(lighting::raised(self.palette.is_dark))
            .text_size(px(12.5))
            .text_color(self.palette.text_primary)
            .child(self.title.clone())
    }
}

impl Browser {
    /// Whether the page in front is bookmarked.
    pub(crate) fn bookmarked(&self) -> bool {
        let tab = self.current();
        tab.page == Page::Web && self.bookmarks().find_url(&tab.url).is_some()
    }

    /// Saves `url` in `folder` (the bar if `None`), named for its title or
    /// its site. Returns the bookmark's id.
    pub(crate) fn add_bookmark(&mut self, url: String, title: String, folder: Folder) -> u64 {
        // A page bookmarked before its title arrives is named for its site.
        let title = match title.trim() {
            "" | "New Tab" | "Loading" => site_name(&url),
            title => title.to_owned(),
        };
        self.want_favicon(&url, None);
        let id = self.bookmarks_mut().add(folder, None, Node::link(title, url));
        if folder.is_none() {
            // Show where it went.
            self.bookmarks_bar = true;
        }
        self.persist();
        id
    }

    /// ⌘D and the star: saves the page in the folder used last, or finds
    /// it if it's saved already, and offers to file it elsewhere, rename
    /// it or remove it. Never removes it unasked.
    pub(crate) fn toggle_bookmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.current();
        if tab.page != Page::Web {
            return;
        }
        let (url, title) = (tab.url.clone(), tab.title.clone());
        let existing = self.bookmarks().find_url(&url);
        let (id, heading) = match existing {
            Some(id) => {
                let folder = self.bookmarks().parent_of(id).flatten();
                (id, format!("Bookmarked in “{}”", self.folder_name(folder)))
            }
            None => {
                let folder = self
                    .last_bookmark_folder
                    .filter(|f| self.bookmarks().get(*f).is_some_and(Node::is_folder));
                let id = self.add_bookmark(url, title, folder);
                (id, format!("Added to “{}”", self.folder_name(folder)))
            }
        };
        cx.notify();
        // Where it is, and where else it could go, from the star.
        let mut items: MenuItems = vec![
            (MenuEntry::disabled(heading), None),
            (MenuEntry::Separator, None),
            self.move_to_menu(id),
            (MenuEntry::item("New Folder…"), Some(Command::FileInNewFolder(id))),
            (MenuEntry::item("Rename…"), Some(Command::RenameBookmark(id))),
            (MenuEntry::Separator, None),
            (MenuEntry::item("Delete Bookmark"), Some(Command::DeleteBookmark(id))),
        ];
        items.retain(|(entry, _)| !matches!(entry, MenuEntry::Submenu { entries, .. } if entries.is_empty()));
        let at = self
            .omnibox_bounds
            .get()
            .map(|b| point(b.origin.x + b.size.width - px(28.0), b.origin.y + b.size.height))
            .unwrap_or_else(|| point(px(400.0), px(52.0)));
        self.context_menu(at, items, window, cx);
    }

    fn folder_name(&self, folder: Folder) -> String {
        match folder {
            None => "Bookmarks Bar".to_owned(),
            Some(id) => self.bookmarks().get(id).map(|f| f.title.clone()).unwrap_or_default(),
        }
    }

    /// "Move to" with every folder `id` could go into, the one it's in
    /// ticked.
    fn move_to_menu(&self, id: u64) -> (MenuEntry, Option<Command>) {
        let bookmarks = self.bookmarks();
        let parent = bookmarks.parent_of(id).flatten();
        let inside = |folder: u64| {
            bookmarks
                .get(id)
                .is_some_and(|node| node.id == folder || find_in(node, folder))
        };
        let mut items: MenuItems = vec![(
            MenuEntry::checked("Bookmarks Bar", parent.is_none()),
            Some(Command::MoveBookmarkTo(id, None)),
        )];
        for (folder, title, depth) in bookmarks.folders() {
            if inside(folder) {
                continue;
            }
            let label = format!("{}{}", "    ".repeat(depth), title);
            items.push((
                MenuEntry::checked(label, parent == Some(folder)),
                Some(Command::MoveBookmarkTo(id, Some(folder))),
            ));
        }
        submenu("Move to", items)
    }

    /// The right-click menu of what's selected in the manager, when it's
    /// more than one.
    fn selection_menu(&self, ids: Vec<u64>) -> MenuItems {
        let bookmarks = self.bookmarks();
        let links = ids.iter().filter(|&&id| bookmarks.get(id).is_some_and(|n| !n.is_folder())).count();
        let inside = |folder: u64| {
            ids.iter().any(|&id| bookmarks.get(id).is_some_and(|node| node.id == folder || find_in(node, folder)))
        };
        let mut moves: MenuItems = vec![(
            MenuEntry::item("Bookmarks Bar"),
            Some(Command::MoveBookmarksTo(ids.clone(), None)),
        )];
        for (folder, title, depth) in bookmarks.folders() {
            if inside(folder) {
                continue;
            }
            moves.push((
                MenuEntry::item(format!("{}{}", "    ".repeat(depth), title)),
                Some(Command::MoveBookmarksTo(ids.clone(), Some(folder))),
            ));
        }
        drop(bookmarks);
        let count = ids.len();
        vec![
            (
                crate::commands::entry(
                    &format!("Open {links} in Tabs"),
                    links > 0,
                ),
                Some(Command::OpenBookmarks(ids.clone())),
            ),
            (MenuEntry::Separator, None),
            submenu(format!("Move {count} to"), moves),
            (MenuEntry::Separator, None),
            (MenuEntry::item(format!("Delete {count} Items")), Some(Command::DeleteBookmarks(ids))),
        ]
    }

    /// The right-click menu of a bookmark or folder; of every one selected,
    /// if it's one of several.
    pub(crate) fn bookmark_menu(&self, id: u64) -> MenuItems {
        if self.bookmark_selection.ids.len() > 1 && self.bookmark_selection.ids.contains(&id) {
            return self.selection_menu(self.bookmark_selection.ids.clone());
        }
        let (url, children, on_bar, index, siblings) = {
            let bookmarks = self.bookmarks();
            let Some(node) = bookmarks.get(id) else {
                return Vec::new();
            };
            let parent = bookmarks.parent_of(id).flatten();
            let siblings = bookmarks.children(parent);
            let index = siblings.iter().position(|n| n.id == id).unwrap_or(0);
            (node.url.clone(), node.children.len(), parent.is_none(), index, siblings.len())
        };
        let mut items = match &url {
            Some(url) => crate::commands::open_entries(url),
            None => vec![
                (
                    crate::commands::entry("Open All in Tabs", children > 0),
                    Some(Command::OpenBookmarkFolder(id)),
                ),
                (MenuEntry::item("New Folder Inside…"), Some(Command::NewBookmarkFolder(Some(id)))),
                (MenuEntry::Separator, None),
            ],
        };
        items.push((MenuEntry::item("Rename…"), Some(Command::RenameBookmark(id))));
        if url.is_some() {
            items.push((MenuEntry::item("Edit Address…"), Some(Command::EditBookmarkUrl(id))));
        }
        items.push(self.move_to_menu(id));
        // Reordering without dragging: along the bar, or up and down a folder.
        let (earlier, later) = if on_bar { ("Move Left", "Move Right") } else { ("Move Up", "Move Down") };
        items.push((crate::commands::entry(earlier, index > 0), Some(Command::MoveBookmark(id, -1))));
        items.push((
            crate::commands::entry(later, index + 1 < siblings),
            Some(Command::MoveBookmark(id, 1)),
        ));
        items.push((MenuEntry::Separator, None));
        items.push((
            MenuEntry::item(if url.is_some() { "Delete Bookmark" } else { "Delete Folder" }),
            Some(Command::DeleteBookmark(id)),
        ));
        items
    }

    /// Carries out a bookmark command.
    pub(crate) fn run_bookmark_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            Command::RenameBookmark(id) => {
                let Some(title) = self.bookmarks().get(id).map(|b| b.title.clone()) else {
                    return;
                };
                let what = if self.bookmarks().get(id).is_some_and(Node::is_folder) {
                    "Rename Folder"
                } else {
                    "Rename Bookmark"
                };
                self.ask(what, "", title, move |this, answer, cx| {
                    let answer = answer.trim();
                    if !answer.is_empty() && this.bookmarks_mut().rename(id, answer) {
                        this.persist();
                        cx.notify();
                    }
                }, cx);
            }
            Command::EditBookmarkUrl(id) => {
                let Some(url) = self.bookmarks().get(id).and_then(|b| b.url.clone()) else {
                    return;
                };
                self.ask("Edit Bookmark Address", "", url, move |this, answer, cx| {
                    // Emptied: left as it was, rather than the home page.
                    if answer.trim().is_empty() {
                        return;
                    }
                    let url = settings::destination(&answer, &this.settings);
                    if this.bookmarks_mut().set_url(id, &url) {
                        this.want_favicon(&url, None);
                        this.persist();
                        cx.notify();
                    }
                }, cx);
            }
            Command::DeleteBookmark(id) => {
                let (folder, count, title) = {
                    let bookmarks = self.bookmarks();
                    let Some(node) = bookmarks.get(id) else {
                        return;
                    };
                    (node.is_folder(), count_links(node), node.title.clone())
                };
                if folder && count > 0 {
                    let message = format!(
                        "It has {count} bookmark{} in it. This can't be undone.",
                        if count == 1 { "" } else { "s" }
                    );
                    self.confirm_then(
                        format!("Delete “{title}”?"), message, "Delete", cx,
                        move |browser, cx| browser.delete_bookmark(id, cx),
                    );
                } else {
                    self.delete_bookmark(id, cx);
                }
            }
            Command::MoveBookmarksTo(ids, folder) => {
                if self.bookmarks_mut().move_many(&ids, folder, None) {
                    self.last_bookmark_folder = folder;
                    self.persist();
                    cx.notify();
                }
            }
            Command::OpenBookmarks(ids) => {
                let urls: Vec<String> = {
                    let bookmarks = self.bookmarks();
                    ids.iter().filter_map(|&id| bookmarks.get(id)?.url.clone()).collect()
                };
                for (index, url) in urls.iter().enumerate() {
                    let place = if index == 0 { Place::NewTab } else { Place::BackgroundTab };
                    self.open_link(url, place, window, cx);
                }
            }
            Command::DeleteBookmarks(ids) => {
                let links: usize = {
                    let bookmarks = self.bookmarks();
                    ids.iter().filter_map(|&id| bookmarks.get(id)).map(count_links).sum()
                };
                let count = ids.len();
                self.confirm_then(
                    format!("Delete {count} items?"),
                    format!(
                        "{links} bookmark{} go{} with them. This can't be undone.",
                        if links == 1 { "" } else { "s" },
                        if links == 1 { "es" } else { "" },
                    ),
                    "Delete",
                    cx,
                    move |browser, cx| {
                        for id in &ids {
                            browser.delete_bookmark(*id, cx);
                        }
                        browser.bookmark_selection.ids.clear();
                    },
                );
            }
            Command::MoveBookmark(id, by) => {
                if self.bookmarks_mut().shift(id, by) {
                    self.persist();
                    cx.notify();
                }
            }
            Command::MoveBookmarkTo(id, folder) => {
                if self.bookmarks_mut().move_to(id, folder, None) {
                    self.last_bookmark_folder = folder;
                    self.persist();
                    cx.notify();
                }
            }
            Command::NewBookmarkFolder(parent) => {
                self.ask("New Folder", "", "Untitled Folder".into(), move |this, answer, cx| {
                    let name = answer.trim();
                    if name.is_empty() {
                        return;
                    }
                    let id = this.bookmarks_mut().add(parent, None, Node::folder(name, Vec::new()));
                    if this.current().page == Page::Bookmarks {
                        this.bookmark_folder = Some(id);
                    }
                    this.persist();
                    cx.notify();
                }, cx);
            }
            Command::FileInNewFolder(id) => {
                self.ask("New Folder", "", "Untitled Folder".into(), move |this, answer, cx| {
                    let name = answer.trim();
                    if name.is_empty() {
                        return;
                    }
                    let parent = this.bookmarks().parent_of(id).flatten();
                    let folder = this.bookmarks_mut().add(parent, None, Node::folder(name, Vec::new()));
                    this.bookmarks_mut().move_to(id, Some(folder), None);
                    this.last_bookmark_folder = Some(folder);
                    this.persist();
                    cx.notify();
                }, cx);
            }
            Command::OpenBookmarkFolder(id) => {
                let urls: Vec<String> = self
                    .bookmarks()
                    .children(Some(id))
                    .iter()
                    .filter_map(|n| n.url.clone())
                    .collect();
                for (index, url) in urls.iter().enumerate() {
                    let place = if index == 0 { Place::NewTab } else { Place::BackgroundTab };
                    self.open_link(url, place, window, cx);
                }
            }
            Command::ShowBookmarks => self.open_page(Page::Bookmarks, window, cx),
            Command::ImportBookmarks(source) => match source.import() {
                Ok(folder) => {
                    let count = count_links(&folder);
                    let urls: Vec<String> = links_in(&folder).into_iter().map(str::to_owned).collect();
                    let id = self.bookmarks_mut().add(None, None, folder);
                    for url in urls {
                        self.want_favicon(&url, None);
                    }
                    self.bookmark_folder = Some(id);
                    self.bookmark_notice = Some(format!("Imported {count} bookmarks from {}.", source.name()));
                    self.persist();
                    cx.notify();
                }
                Err(err) => {
                    self.bookmark_notice = Some(err);
                    cx.notify();
                }
            },
            Command::ImportBookmarksFile => {
                cx.spawn_in(window, async move |this, cx| {
                    let Some(path) = native::choose_file(&["html", "htm"]) else {
                        return;
                    };
                    let _ = this.update(cx, |browser, cx| browser.import_bookmark_file(&path, cx));
                })
                .detach();
            }
            Command::ExportBookmarks => {
                let html = crate::bookmarks::write_html(self.bookmarks().root());
                cx.spawn_in(window, async move |this, cx| {
                    let Some(path) = native::choose_save_path("Bookmarks.html") else {
                        return;
                    };
                    let notice = match std::fs::write(&path, html) {
                        Ok(()) => format!("Exported to {}.", path.display()),
                        Err(err) => format!("Couldn't export: {err}"),
                    };
                    let _ = this.update(cx, |browser, cx| {
                        browser.bookmark_notice = Some(notice);
                        cx.notify();
                    });
                })
                .detach();
            }
            _ => {}
        }
    }

    fn delete_bookmark(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.bookmarks_mut().remove(id).is_some() {
            // The manager was showing it: back to its parent, or the bar.
            if self.bookmark_folder == Some(id) || self.bookmark_folder.is_some_and(|f| self.bookmarks().get(f).is_none()) {
                self.bookmark_folder = None;
            }
            self.persist();
            cx.notify();
        }
    }

    fn import_bookmark_file(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        match std::fs::read_to_string(path) {
            Ok(html) => {
                let nodes = crate::bookmarks::read_html(&html);
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Imported".into());
                let folder = Node::folder(format!("From {name}"), nodes);
                let count = count_links(&folder);
                if count == 0 {
                    self.bookmark_notice = Some("No bookmarks in that file.".into());
                } else {
                    let urls: Vec<String> = links_in(&folder).into_iter().map(str::to_owned).collect();
                    let id = self.bookmarks_mut().add(None, None, folder);
                    for url in urls {
                        self.want_favicon(&url, None);
                    }
                    self.bookmark_folder = Some(id);
                    self.bookmark_notice = Some(format!("Imported {count} bookmarks."));
                    self.persist();
                }
            }
            Err(err) => self.bookmark_notice = Some(format!("Couldn't read that file: {err}")),
        }
        cx.notify();
    }

    /// Makes `row` a bookmark that can be dragged (with `with`, whatever's
    /// selected along with it), and dropped onto: a folder takes what's
    /// dropped in, anything else takes it before itself.
    fn bookmark_dnd(
        &self,
        row: Stateful<gpui::Div>,
        item: &Item,
        parent: Folder,
        with: Vec<u64>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let ids = if with.contains(&item.id) { with } else { vec![item.id] };
        let dragged = DraggedBookmark::of(ids, item.title.clone(), palette);
        let id = item.id;
        let folder = item.url.is_none();
        row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedBookmark>(move |style, _, _, _| {
                style.bg(color::with_alpha(palette.accent, 0.22))
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedBookmark, _, cx| {
                if dragged.ids == [id] {
                    return;
                }
                let moved = if folder && !dragged.ids.contains(&id) {
                    this.bookmarks_mut().move_many(&dragged.ids, Some(id), None)
                } else {
                    this.bookmarks_mut().move_many(&dragged.ids, parent, Some(id))
                };
                if moved {
                    this.persist();
                    cx.notify();
                }
            }))
    }

    // ---- The bar ------------------------------------------------------------

    /// A slim row of links and folders, divided from each other and, when
    /// the tab strip follows, from the tabs by a rule, so the two rows don't
    /// run together.
    pub(crate) fn bookmarks_bar(
        &mut self,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chrome = Chrome::new(palette);
        let scroll = self.controls.scroll("bookmarks-bar");
        let mut bar = div()
            .id("bookmarks-bar")
            .size_full()
            .overflow_x_scroll()
            .track_scroll(&scroll)
            .flex()
            .items_center()
            .gap(px(2.0))
            .px(px(12.0))
            .pb(px(2.0))
            .text_size(px(12.0));
        if self.bookmarks().is_empty() {
            bar = bar.child(
                div()
                    .px(px(8.0))
                    .text_color(palette.text_secondary)
                    .child("No bookmarks yet — press ⌘D or click ☆ in the address field."),
            );
        }
        let root: Vec<Item> = self.bookmarks().root().iter().map(Item::of).collect();
        for (index, node) in root.iter().enumerate() {
            if index > 0 {
                bar = bar.child(div().w(px(1.0)).h(px(14.0)).flex_none().bg(chrome.line));
            }
            let id = node.id;
            let key = marquee::bookmark_key(id);
            let leading: AnyElement = match &node.url {
                Some(url) => site_icon(self.shown_favicon(url), url, 16.0, false, palette),
                None => icon(Icon::Folder, 15.0, palette.text_secondary).into_any_element(),
            };
            let item = div()
                .id(("bookmark", id))
                .h(px(24.0))
                .flex_none()
                .max_w(px(190.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .pl(px(5.0))
                .pr(px(8.0))
                .rounded(px(6.0))
                .text_color(palette.text_primary)
                .cursor_pointer()
                .hover(move |style| style.bg(chrome.wash))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    this.hover_label(key, *hovered, cx)
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        let items = this.bookmark_menu(id);
                        this.context_menu(event.position, items, window, cx);
                    }),
                )
                .child(leading)
                .child(marquee::label(
                    node.title.clone(),
                    key,
                    self.hovered_label == Some(key),
                    &self.label_widths,
                ));
            let item = match node.url.clone() {
                Some(url) => {
                    let middle = url.clone();
                    item.on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        // ⌘-click opens behind, like a middle click.
                        let place = if event.modifiers().platform {
                            Place::BackgroundTab
                        } else {
                            Place::Here
                        };
                        this.open_link(&url, place, window, cx)
                    }))
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            this.open_link(&middle, Place::BackgroundTab, window, cx)
                        }),
                    )
                }
                // A folder opens as a menu under it, which bookmarks can be
                // dragged out of.
                None => item
                    .child(crate::bookmark_menu::anchor(&self.menu_anchors, id))
                    .on_click(cx.listener(move |this, _, window, cx| this.toggle_bookmark_menu(id, window, cx))),
            };
            bar = bar.child(self.bookmark_dnd(item, node, None, Vec::new(), palette, cx));
        }
        // Dropped past the last one: to the end of the bar.
        bar = bar.child(
            div()
                .id("bookmarks-bar-end")
                .flex_1()
                .min_w(px(24.0))
                .h_full()
                .drag_over::<DraggedBookmark>(move |style, _, _, _| {
                    style.bg(color::with_alpha(palette.accent, 0.12))
                })
                .on_drop(cx.listener(move |this, dragged: &DraggedBookmark, _, cx| {
                    if this.bookmarks_mut().move_many(&dragged.ids, None, None) {
                        this.persist();
                        cx.notify();
                    }
                })),
        );
        self.faded(
            "bookmarks-bar",
            bar,
            &scroll,
            ScrollAxis::Horizontal,
            chrome.ground,
        )
        .h(px(BOOKMARKS_HEIGHT))
        .w_full()
        .flex_none()
        .when(self.tab_strip_height() > 0.5, |el| {
            el.border_b_1().border_color(chrome.line)
        })
        .id("bookmarks-bar-area")
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                let items = this.bookmarks_bar_menu();
                this.context_menu(event.position, items, window, cx);
            }),
        )
    }

    // ---- The manager --------------------------------------------------------

    pub(crate) fn bookmarks_page(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chrome = Chrome::new(palette);
        // A folder deleted elsewhere: show the bar instead.
        if self
            .bookmark_folder
            .is_some_and(|f| !self.bookmarks().get(f).is_some_and(Node::is_folder))
        {
            self.bookmark_folder = None;
        }
        let query = self.inputs.bookmark_search.read(cx).content.to_string();
        let search = self.inputs.bookmark_search.clone();
        let header = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .pb(px(14.0))
            .child(
                div()
                    .flex_1()
                    .text_size(px(26.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Bookmarks"),
            )
            .child(
                div()
                    .w(px(240.0))
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .rounded(px(8.0))
                    .bg(palette.field_surface)
                    .border_1()
                    .border_color(color::with_alpha(palette.field_border_strong, 0.5))
                    .child(icon(Icon::Search, 12.0, palette.text_secondary))
                    .child(div().flex_1().min_w(px(0.0)).child(search)),
            )
            .child(self.bookmark_button("bm-new-folder", "New Folder", palette, cx, {
                let parent = self.bookmark_folder;
                move |this, window, cx| this.run(Command::NewBookmarkFolder(parent), window, cx)
            }))
            .child(self.bookmark_button("bm-import", "Import…", palette, cx, |this, window, cx| {
                let mut items: MenuItems = Source::ALL
                    .into_iter()
                    .filter(|s| s.available())
                    .map(|s| (MenuEntry::item(format!("From {}", s.name())), Some(Command::ImportBookmarks(s))))
                    .collect();
                if !items.is_empty() {
                    items.push((MenuEntry::Separator, None));
                }
                items.push((MenuEntry::item("From an HTML File…"), Some(Command::ImportBookmarksFile)));
                let at = this.pointer_position(window);
                this.context_menu(at, items, window, cx);
            }))
            .child(self.bookmark_button("bm-export", "Export…", palette, cx, |this, window, cx| {
                this.run(Command::ExportBookmarks, window, cx)
            }));
        let notice = self.bookmark_notice.clone().map(|notice| {
            div()
                .mb(px(12.0))
                .px(px(14.0))
                .py(px(9.0))
                .rounded(px(9.0))
                .bg(chrome.raised)
                .text_color(palette.text_primary)
                .child(notice)
        });
        let body = div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .gap(px(18.0))
            .child(self.folder_tree(palette, cx))
            .child(self.bookmark_list(&query, palette, cx));
        let band = self.selection_band(palette, cx);
        div()
            .id("bookmarks-page")
            .relative()
            .size_full()
            .bg(palette.backdrop)
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(980.0))
                    .h_full()
                    .flex()
                    .flex_col()
                    .px(px(32.0))
                    .pt(px(36.0))
                    .pb(px(20.0))
                    .child(header)
                    .children(notice)
                    .child(body),
            )
            // Over the list, so the box shows above the rows it crosses.
            .children(band)
            .into_any_element()
    }

    /// Where the pointer last was, for a menu from a button.
    fn pointer_position(&self, window: &Window) -> Point<gpui::Pixels> {
        window.mouse_position()
    }

    fn bookmark_button(
        &self,
        id: &'static str,
        label: &'static str,
        palette: Palette,
        cx: &mut Context<Self>,
        run: impl Fn(&mut Browser, &mut Window, &mut Context<Browser>) + 'static,
    ) -> AnyElement {
        div()
            .flex_none()
            .child(vampir::button(id, label, ButtonVariant::Soft, true, palette, cx, run))
            .into_any_element()
    }

    /// The bar and every folder, nested, each a place to drop onto.
    fn folder_tree(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chrome = Chrome::new(palette);
        let selected = self.bookmark_folder;
        let folders = self.bookmarks().folders();
        let mut tree = div()
            .id("bookmark-folders")
            .w(px(220.0))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0));
        let entries = std::iter::once((None, "Bookmarks Bar".to_owned(), 0))
            .chain(folders.into_iter().map(|(id, title, depth)| (Some(id), title, depth + 1)));
        for (folder, title, depth) in entries {
            let glyph = if folder.is_none() { Icon::Bookmark } else { Icon::Folder };
            let active = selected == folder;
            let key = folder.map_or(0, |f| f as usize + 1);
            let row = div()
                .id(("bookmark-folder", key))
                .h(px(30.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .pl(px(10.0 + depth as f32 * 14.0))
                .pr(px(10.0))
                .rounded(px(7.0))
                .cursor_pointer()
                .text_color(palette.text_primary)
                .when(active, |el| el.bg(palette.soft_fill))
                .when(!active, |el| el.hover(move |s| s.bg(chrome.wash)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.bookmark_folder = folder;
                    this.bookmark_selection = Default::default();
                    this.bookmark_notice = None;
                    this.inputs.bookmark_search.update(cx, |input, cx| input.set_text("", cx));
                    cx.notify();
                }))
                .drag_over::<DraggedBookmark>(move |style, _, _, _| {
                    style.bg(color::with_alpha(palette.accent, 0.22))
                })
                .on_drop(cx.listener(move |this, dragged: &DraggedBookmark, _, cx| {
                    if this.bookmarks_mut().move_many(&dragged.ids, folder, None) {
                        this.persist();
                        cx.notify();
                    }
                }))
                .when_some(folder, |el, id| {
                    el.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let items = this.bookmark_menu(id);
                            this.context_menu(event.position, items, window, cx);
                        }),
                    )
                })
                .child(icon(glyph, 14.0, palette.text_secondary))
                .child(div().min_w(px(0.0)).truncate().child(title));
            tree = tree.child(row);
        }
        tree.into_any_element()
    }

    /// The chosen folder's contents, or with a search, every bookmark that
    /// matches, with the folder it's in.
    fn bookmark_list(
        &mut self,
        query: &str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = Chrome::new(palette);
        let folder = self.bookmark_folder;
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        // (item, where it is, its folder, its place there)
        let rows: Vec<(Item, String, Folder, usize)> = {
            let bookmarks = self.bookmarks();
            if words.is_empty() {
                bookmarks
                    .children(folder)
                    .iter()
                    .enumerate()
                    .map(|(i, node)| (Item::of(node), String::new(), folder, i))
                    .collect()
            } else {
                bookmarks
                    .links_with_paths()
                    .into_iter()
                    .filter(|(node, _)| {
                        let haystack = format!("{} {}", node.title, node.url.as_deref().unwrap_or_default()).to_lowercase();
                        words.iter().all(|w| haystack.contains(w.as_str()))
                    })
                    .map(|(node, path)| {
                        let parent = bookmarks.parent_of(node.id).flatten();
                        let index = bookmarks.children(parent).iter().position(|n| n.id == node.id).unwrap_or(0);
                        (Item::of(node), path, parent, index)
                    })
                    .collect()
            }
        };
        // What's selected, kept to what's listed, in the order it's listed.
        let order: Vec<u64> = rows.iter().map(|(node, ..)| node.id).collect();
        if self.bookmark_selection.band.is_none() {
            let selection = &mut self.bookmark_selection;
            selection.ids = order.iter().copied().filter(|id| selection.ids.contains(id)).collect();
        }
        let selected = self.bookmark_selection.ids.clone();
        self.bookmark_selection.rows.borrow_mut().clear();
        let focus = self.controls.focus("bookmark-list", cx);
        let keys_order = order.clone();
        let mut list = div()
            .id("bookmark-list")
            .track_focus(&focus)
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0))
            // ⌘A selects everything listed, ⌫ deletes what's selected, and
            // Escape lets it go.
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let platform = keystroke.modifiers.platform;
                match keystroke.key.as_str() {
                    "a" if platform => {
                        this.bookmark_selection.ids = keys_order.clone();
                        cx.stop_propagation();
                        cx.notify();
                    }
                    "backspace" | "delete" if !this.bookmark_selection.ids.is_empty() => {
                        let ids = this.bookmark_selection.ids.clone();
                        cx.stop_propagation();
                        match ids.as_slice() {
                            [id] => this.run(Command::DeleteBookmark(*id), window, cx),
                            _ => this.run(Command::DeleteBookmarks(ids), window, cx),
                        }
                    }
                    "escape" if !this.bookmark_selection.ids.is_empty() => {
                        this.bookmark_selection.ids.clear();
                        cx.stop_propagation();
                        cx.notify();
                    }
                    _ => {}
                }
            }));
        if words.is_empty() {
            list = list.child(self.breadcrumbs(folder, palette, cx));
        }
        if rows.is_empty() {
            let message = if words.is_empty() {
                "Nothing here yet. Drag bookmarks in, or save a page with ⌘D."
            } else {
                "No bookmarks match."
            };
            list = list.child(
                div()
                    .pt(px(30.0))
                    .text_center()
                    .text_color(palette.text_secondary)
                    .child(message),
            );
        }
        for (node, path, parent, index) in &rows {
            let id = node.id;
            let (leading, detail): (AnyElement, String) = match &node.url {
                Some(url) => (
                    site_icon(self.shown_favicon(url), url, 18.0, false, palette),
                    {
                        let address = url.trim_start_matches("https://").trim_start_matches("http://");
                        let address = address.strip_prefix("www.").unwrap_or(address).trim_end_matches('/');
                        if path.is_empty() {
                            address.to_owned()
                        } else {
                            format!("{path} — {address}")
                        }
                    },
                ),
                None => (
                    div()
                        .size(px(18.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(Icon::Folder, 16.0, palette.text_secondary))
                        .into_any_element(),
                    {
                        let count = node.count;
                        format!("{count} item{}", if count == 1 { "" } else { "s" })
                    },
                ),
            };
            let key = marquee::bookmark_key(id) | (1 << 41);
            let is_selected = selected.contains(&id);
            let record = self.bookmark_selection.rows.clone();
            let row = div()
                .id(("bookmark-row", id))
                .relative()
                .h(px(44.0))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(12.0))
                .px(px(12.0))
                .rounded(px(9.0))
                .cursor_pointer()
                .border_1()
                .when(is_selected, |el| {
                    el.bg(color::with_alpha(palette.accent, 0.16))
                        .border_color(color::with_alpha(palette.accent, 0.55))
                })
                .when(!is_selected, |el| {
                    el.bg(chrome.raised)
                        .border_color(Palette::transparent())
                        .hover(move |s| s.bg(chrome.wash))
                })
                .child(
                    canvas(move |bounds, _, _| record.borrow_mut().push((id, bounds)), |_, _, _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                )
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    this.hover_label(key, *hovered, cx)
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        let items = this.bookmark_menu(id);
                        this.context_menu(event.position, items, window, cx);
                    }),
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
                                .flex()
                                .text_color(palette.text_primary)
                                .child(marquee::label(
                                    node.title.clone(),
                                    key,
                                    self.hovered_label == Some(key),
                                    &self.label_widths,
                                )),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.5))
                                .text_color(palette.text_secondary)
                                .child(detail),
                        ),
                );
            // ⌘-click adds to the selection or takes out, ⇧-click selects
            // everything from the last one clicked; a plain click opens: a
            // link in a tab of its own, so the manager stays (the middle
            // button, behind), a folder here.
            let url = node.url.clone();
            let click_order = order.clone();
            let click_focus = focus.clone();
            let row = row.on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                let modifiers = event.modifiers();
                window.focus(&click_focus, cx);
                if modifiers.platform || modifiers.shift {
                    this.select_bookmark_row(id, modifiers.shift, modifiers.platform, &click_order);
                    cx.notify();
                    return;
                }
                this.bookmark_selection.ids.clear();
                this.bookmark_selection.anchor = Some(id);
                match &url {
                    Some(url) => this.open_link(url, Place::NewTab, window, cx),
                    None => {
                        this.bookmark_folder = Some(id);
                        this.bookmark_selection = Default::default();
                        cx.notify();
                    }
                }
            }));
            let row = match node.url.clone() {
                Some(url) => row.on_mouse_up(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| this.open_link(&url, Place::BackgroundTab, window, cx)),
                ),
                None => row,
            };
            let _ = index;
            list = list.child(self.bookmark_dnd(row, node, *parent, selected.clone(), palette, cx));
        }
        // Below the last: dropped on, to the end of this folder; pressed
        // on, where a box is dragged out to select what it touches.
        let band_focus = focus.clone();
        let end = div()
            .id("bookmark-list-end")
            .flex_1()
            .min_h(px(40.0))
            .rounded(px(9.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    window.focus(&band_focus, cx);
                    let base = if event.modifiers.platform || event.modifiers.shift {
                        this.bookmark_selection.ids.clone()
                    } else {
                        Vec::new()
                    };
                    this.bookmark_selection.ids = base.clone();
                    this.bookmark_selection.band = Some(SelectionBand {
                        start: event.position,
                        current: event.position,
                        base,
                    });
                    cx.notify();
                }),
            );
        let end = if words.is_empty() {
            end.drag_over::<DraggedBookmark>(move |style, _, _, _| {
                style.bg(color::with_alpha(palette.accent, 0.12))
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedBookmark, _, cx| {
                if this.bookmarks_mut().move_many(&dragged.ids, folder, None) {
                    this.persist();
                    cx.notify();
                }
            }))
        } else {
            end
        };
        list = list.child(end);
        list.into_any_element()
    }

    /// ⌘-click (`toggle`) and ⇧-click (`extend`) on row `id` of `order`.
    fn select_bookmark_row(&mut self, id: u64, extend: bool, toggle: bool, order: &[u64]) {
        let selection = &mut self.bookmark_selection;
        let anchor = selection.anchor.filter(|a| order.contains(a));
        match (extend, anchor) {
            (true, Some(anchor)) => {
                let (from, to) = (
                    order.iter().position(|&x| x == anchor).unwrap_or(0),
                    order.iter().position(|&x| x == id).unwrap_or(0),
                );
                let range = &order[from.min(to)..=from.max(to)];
                if !toggle {
                    selection.ids.clear();
                }
                for &row in range {
                    if !selection.ids.contains(&row) {
                        selection.ids.push(row);
                    }
                }
            }
            _ => {
                if let Some(at) = selection.ids.iter().position(|&x| x == id) {
                    selection.ids.remove(at);
                } else {
                    selection.ids.push(id);
                }
                selection.anchor = Some(id);
            }
        }
        selection.ids = order.iter().copied().filter(|x| selection.ids.contains(x)).collect();
    }

    /// While a selection box is out: follows the pointer anywhere in the
    /// window until the button comes up, selecting the rows it touches, and
    /// draws it.
    fn selection_band(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let band = self.bookmark_selection.band.clone()?;
        let me = cx.weak_entity();
        let fill_color = color::with_alpha(palette.accent, 0.12);
        let edge = color::with_alpha(palette.accent, 0.7);
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    window.paint_quad(gpui::quad(
                        band.bounds(),
                        px(3.0),
                        fill_color,
                        px(1.0),
                        edge,
                        gpui::BorderStyle::Solid,
                    ));
                    let moved = me.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            let _ = moved.update(cx, |browser, cx| {
                                browser.move_selection_band(event.position);
                                cx.notify();
                            });
                        }
                    });
                    let ended = me.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            let _ = ended.update(cx, |browser, cx| {
                                browser.bookmark_selection.band = None;
                                cx.notify();
                            });
                        }
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }

    fn move_selection_band(&mut self, to: Point<Pixels>) {
        let selection = &mut self.bookmark_selection;
        let Some(band) = &mut selection.band else {
            return;
        };
        band.current = to;
        let area = band.bounds();
        let mut ids = band.base.clone();
        for (id, bounds) in selection.rows.borrow().iter() {
            if bounds.intersects(&area) && !ids.contains(id) {
                ids.push(*id);
            }
        }
        let order: Vec<u64> = selection.rows.borrow().iter().map(|(id, _)| *id).collect();
        selection.ids = order.into_iter().filter(|id| ids.contains(id)).collect();
    }

    /// Where the chosen folder is: the bar, then each folder down to it,
    /// each a way back up and a place to drop onto.
    fn breadcrumbs(&self, folder: Folder, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let mut trail: Vec<(Folder, String)> = Vec::new();
        {
            let bookmarks = self.bookmarks();
            let mut at = folder;
            while let Some(id) = at {
                trail.push((Some(id), bookmarks.get(id).map(|f| f.title.clone()).unwrap_or_default()));
                at = bookmarks.parent_of(id).flatten();
            }
        }
        trail.push((None, "Bookmarks Bar".to_owned()));
        trail.reverse();
        let last = trail.len() - 1;
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .pb(px(8.0))
            .text_size(px(13.0));
        for (index, (target, title)) in trail.into_iter().enumerate() {
            if index > 0 {
                row = row.child(div().text_color(palette.text_secondary).child("›"));
            }
            let current = index == last;
            row = row.child(
                div()
                    .id(("bookmark-crumb", index))
                    .px(px(4.0))
                    .rounded(px(5.0))
                    .text_color(if current { palette.text_primary } else { palette.text_secondary })
                    .when(current, |el| el.font_weight(FontWeight::SEMIBOLD))
                    .when(!current, |el| {
                        el.cursor_pointer().hover(|s| s.underline()).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.bookmark_folder = target;
                                this.bookmark_selection = Default::default();
                                cx.notify();
                            },
                        ))
                    })
                    .drag_over::<DraggedBookmark>(move |style, _, _, _| {
                        style.bg(color::with_alpha(palette.accent, 0.22))
                    })
                    .on_drop(cx.listener(move |this, dragged: &DraggedBookmark, _, cx| {
                        if this.bookmarks_mut().move_many(&dragged.ids, target, None) {
                            this.persist();
                            cx.notify();
                        }
                    }))
                    .child(title),
            );
        }
        row.into_any_element()
    }
}

/// Whether `id` is somewhere inside `node`.
fn find_in(node: &Node, id: u64) -> bool {
    node.children.iter().any(|child| child.id == id || find_in(child, id))
}

fn count_links(node: &Node) -> usize {
    if node.is_folder() {
        node.children.iter().map(count_links).sum()
    } else {
        1
    }
}

fn links_in(node: &Node) -> Vec<&str> {
    match &node.url {
        Some(url) => vec![url.as_str()],
        None => node.children.iter().flat_map(links_in).collect(),
    }
}
