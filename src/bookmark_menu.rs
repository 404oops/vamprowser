//! A folder on the bookmarks bar opens over browser pages. On web pages it
//! uses a separate popup window so the live WebView can keep rendering.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui::{
    AnyElement, Bounds, Context, MouseButton, Pixels, SharedString, Window, canvas,
    div, prelude::*, px,
};
use vampir::{Palette, color, lighting};

use crate::{
    Browser, Chrome, Page,
    bookmarks::Node,
    bookmarks_view::DraggedBookmark,
    commands::Place,
    icons::{Icon, icon},
    native::MenuEntry, site_icon,
};

const WIDTH: f32 = 270.0;
const ROW: f32 = 28.0;

enum PopupChoice {
    Link(String),
    All(u64),
}

fn popup_entries(nodes: &[Node], folder: u64) -> (Vec<MenuEntry>, Vec<Option<PopupChoice>>) {
    let mut entries = Vec::new();
    let mut choices = Vec::new();
    let mut has_links = false;
    for node in nodes {
        if node.is_folder() {
            let (nested, nested_choices) = popup_entries(&node.children, node.id);
            entries.push(MenuEntry::Submenu { label: node.title.clone(), entries: nested });
            choices.push(None);
            choices.extend(nested_choices);
        } else if let Some(url) = &node.url {
            has_links = true;
            entries.push(MenuEntry::item(&node.title));
            choices.push(Some(PopupChoice::Link(url.clone())));
        }
    }
    if has_links {
        entries.push(MenuEntry::Separator);
        choices.push(None);
        entries.push(MenuEntry::item("Open All in Tabs"));
        choices.push(Some(PopupChoice::All(folder)));
    }
    if entries.is_empty() {
        entries.push(MenuEntry::disabled("Empty"));
        choices.push(None);
    }
    (entries, choices)
}

pub(crate) struct BookmarkMenu {
    /// The folders open, from the one on the bar to the innermost.
    path: Vec<u64>,
}

/// Where each folder's row (or button on the bar) was drawn, by id, for
/// menus to open beside.
pub(crate) type Anchors = Rc<RefCell<HashMap<u64, Bounds<Pixels>>>>;

/// Records where a folder's row or button is drawn.
pub(crate) fn anchor(anchors: &Anchors, id: u64) -> impl IntoElement {
    let anchors = anchors.clone();
    canvas(
        move |bounds, _, _| {
            anchors.borrow_mut().insert(id, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

impl Browser {
    /// Opens folder `id` from the bookmarks bar; open already, closes it.
    pub(crate) fn toggle_bookmark_menu(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.current().page == Page::Web {
            let bookmarks = self.bookmarks();
            let Some(folder) = bookmarks.get(id) else { return };
            let (entries, choices) = popup_entries(&folder.children, id);
            let Some(anchor) = self.menu_anchors.borrow().get(&id).copied() else { return };
            let position = gpui::point(anchor.origin.x, anchor.origin.y + anchor.size.height + px(4.0));
            let receiver = crate::app_menu::open(cx, window, position, entries, self.controls.palette());
            cx.spawn_in(window, async move |this, cx| {
                let Ok(Some(index)) = receiver.recv().await else { return };
                let Some(Some(choice)) = choices.into_iter().nth(index) else { return };
                let _ = cx.update(|window, app| {
                    this.update(app, |browser, cx| match choice {
                        PopupChoice::Link(url) => browser.open_link(&url, Place::Here, window, cx),
                        PopupChoice::All(folder) => browser.run(crate::commands::Command::OpenBookmarkFolder(folder), window, cx),
                    })
                });
            }).detach();
            return;
        }
        if self.bookmark_menu.as_ref().is_some_and(|m| m.path.first() == Some(&id)) {
            self.close_bookmark_menu(cx);
            return;
        }
        self.bookmark_menu = Some(BookmarkMenu { path: vec![id] });
        self.bookmark_menu_open.set(true);
        cx.notify();
    }

    pub(crate) fn close_bookmark_menu(&mut self, cx: &mut Context<Self>) {
        if self.bookmark_menu.take().is_none() {
            return;
        }
        self.bookmark_menu_open.set(false);
        self.show_page_if_uncovered();
        cx.notify();
    }

    /// The menu and its open submenus, over everything, with a click
    /// anywhere else closing them.
    pub(crate) fn bookmark_menu_overlay(&mut self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.bookmark_menu.as_ref()?;
        let path = menu.path.clone();
        let anchors = self.menu_anchors.borrow().clone();
        let mut overlay = div()
            .id("bookmark-menu-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            // Keep the scrim through mouse-up. Removing it on mouse-down
            // lets the same click land on the folder button underneath and
            // immediately reopen the menu.
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.close_bookmark_menu(cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.close_bookmark_menu(cx);
                }),
            );
        for (level, folder) in path.iter().enumerate() {
            // The first hangs from its button on the bar; the rest open to
            // the side of the row that opened them.
            let Some(anchor) = anchors.get(folder).copied() else {
                break;
            };
            let (left, top) = if level == 0 {
                (anchor.origin.x, anchor.origin.y + anchor.size.height + px(4.0))
            } else {
                (anchor.origin.x + anchor.size.width + px(4.0), anchor.origin.y - px(6.0))
            };
            overlay = overlay.child(self.menu_panel(*folder, level, left, top, window, palette, cx));
        }
        Some(overlay.into_any_element())
    }

    #[allow(clippy::too_many_arguments)]
    fn menu_panel(
        &self,
        folder: u64,
        level: usize,
        left: Pixels,
        top: Pixels,
        window: &Window,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = Chrome::new(palette);
        let open_child = self.bookmark_menu.as_ref().and_then(|m| m.path.get(level + 1).copied());
        let children: Vec<Node> = self
            .bookmarks()
            .children(Some(folder))
            .iter()
            .map(|node| Node {
                id: node.id,
                title: node.title.clone(),
                url: node.url.clone(),
                children: Vec::new(),
            })
            .collect();
        let viewport = window.viewport_size();
        let rows = children.len().max(1) as f32 +
            if children.iter().any(|node| !node.is_folder()) { 1.5 } else { 0.0 };
        let height = (rows * ROW + 10.0).min(520.0);
        let top = top.min((viewport.height - px(height + 8.0)).max(px(8.0))).max(px(8.0));
        let left = left.min((viewport.width - px(WIDTH + 8.0)).max(px(8.0))).max(px(8.0));
        let max_height = (viewport.height - top - px(8.0)).min(px(520.0));
        let mut list = div()
            .id(("bookmark-menu", folder))
            .max_h(max_height)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p(px(5.0));
        if children.is_empty() {
            list = list.child(
                div()
                    .h(px(ROW))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .text_color(palette.text_secondary)
                    .child("Empty"),
            );
        }
        let links = children.iter().filter(|n| !n.is_folder()).count();
        for node in children {
            let id = node.id;
            let is_folder = node.is_folder();
            let open = open_child == Some(id);
            let leading: AnyElement = match &node.url {
                Some(url) => site_icon(self.shown_favicon(url), url, 16.0, false, palette),
                None => icon(Icon::Folder, 15.0, palette.text_secondary).into_any_element(),
            };
            let dragged = DraggedBookmark::new(id, node.title.clone(), palette);
            let weak = cx.weak_entity();
            let row = div()
                .id(("bookmark-menu-row", id))
                .relative()
                .h(px(ROW))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(9.0))
                .px(px(8.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .text_color(palette.text_primary)
                .when(open, |el| el.bg(palette.soft_fill))
                .when(!open, |el| el.hover(move |s| s.bg(chrome.wash)))
                .child(anchor(&self.menu_anchors, id))
                .child(leading)
                .child(div().flex_1().min_w(px(0.0)).truncate().child(SharedString::from(node.title.clone())))
                .when(is_folder, |el| el.child(icon(Icon::ChevronRight, 10.0, palette.text_secondary)))
                // Resting on a folder opens it beside; on a link, closes
                // whatever was open further in.
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if !*hovered {
                        return;
                    }
                    if let Some(menu) = &mut this.bookmark_menu {
                        menu.path.truncate(level + 1);
                        if is_folder {
                            menu.path.push(id);
                        }
                        cx.notify();
                    }
                }))
                // Dragged out, the menu gets out of the way of where it's
                // going.
                .on_drag(dragged, move |dragged, _, _, cx| {
                    let weak = weak.clone();
                    cx.defer(move |cx| {
                        let _ = weak.update(cx, |browser, cx| browser.close_bookmark_menu(cx));
                    });
                    cx.new(|_| dragged.clone())
                });
            let row = match node.url.clone() {
                Some(url) => {
                    let middle = url.clone();
                    row.on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        let place = if event.modifiers().platform {
                            Place::BackgroundTab
                        } else {
                            Place::Here
                        };
                        this.close_bookmark_menu(cx);
                        this.open_link(&url, place, window, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            this.close_bookmark_menu(cx);
                            this.open_link(&middle, Place::BackgroundTab, window, cx);
                        }),
                    )
                }
                None => row,
            };
            list = list.child(row);
        }
        if links > 0 {
            list = list
                .child(div().mx(px(6.0)).my(px(4.0)).h(px(1.0)).bg(chrome.line))
                .child(
                    div()
                        .id(("bookmark-menu-all", folder))
                        .h(px(ROW))
                        .flex()
                        .items_center()
                        .px(px(10.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .text_color(palette.text_primary)
                        .hover(move |s| s.bg(chrome.wash))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.close_bookmark_menu(cx);
                            this.run(crate::commands::Command::OpenBookmarkFolder(folder), window, cx);
                        }))
                        .child("Open All in Tabs"),
                );
        }
        div()
            .absolute()
            .left(left)
            .top(top)
            .w(px(WIDTH))
            .child(
                div()
                    .id(("bookmark-menu-panel", folder))
                    .occlude()
                    // Clicks inside stay inside.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .rounded(px(10.0))
                    .bg(chrome.raised)
                    .border_1()
                    .border_color(color::with_alpha(palette.field_border_strong, 0.5))
                    .shadow(lighting::panel(palette.is_dark))
                    .text_size(px(13.0))
                    .child(list),
            )
            .into_any_element()
    }

    /// A bookmark (or folder) dropped on the tab bar: onto the tab in front,
    /// it opens there; anywhere else, in a new tab, after tab `on_tab` if it
    /// was dropped on another tab. A folder opens all it holds.
    pub(crate) fn drop_bookmark_on_tabs(
        &mut self,
        id: u64,
        on_tab: Option<u64>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let (url, links) = {
            let bookmarks = self.bookmarks();
            let Some(node) = bookmarks.get(id) else {
                return;
            };
            let links: Vec<String> = node.children.iter().filter_map(|n| n.url.clone()).collect();
            (node.url.clone(), links)
        };
        let Some(url) = url else {
            for (index, link) in links.iter().enumerate() {
                let place = if index == 0 { Place::NewTab } else { Place::BackgroundTab };
                self.open_link(link, place, window, cx);
            }
            return;
        };
        if on_tab.is_some() && on_tab == self.tabs.get(self.selected).map(|tab| tab.id) {
            self.open_link(&url, Place::Here, window, cx);
            return;
        }
        // Where it was dropped: after the tab it landed on, found again by
        // id, as opening a tab can shift the others along.
        let landed = on_tab.filter(|&id| self.index_of(id).is_some());
        self.open_link(&url, Place::NewTab, window, cx);
        if let Some(landed) = landed {
            let tab = self.tabs.remove(self.selected);
            let to = self.index_of(landed).map_or(self.tabs.len(), |index| index + 1);
            self.tabs.insert(to, tab);
            self.selected = to;
            self.persist();
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_choices_follow_submenu_indices() {
        let nodes = vec![
            Node::folder("Folder", vec![Node::link("Nested", "https://nested.example")]),
            Node::link("After", "https://after.example"),
        ];
        let (_, choices) = popup_entries(&nodes, 7);
        assert!(choices[0].is_none());
        assert!(matches!(&choices[1], Some(PopupChoice::Link(url)) if url == "https://nested.example"));
        assert!(matches!(&choices[3], Some(PopupChoice::All(0))));
        assert!(matches!(&choices[4], Some(PopupChoice::Link(url)) if url == "https://after.example"));
        assert!(matches!(&choices[6], Some(PopupChoice::All(7))));
    }
}
