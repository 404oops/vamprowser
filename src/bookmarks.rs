//! Bookmarks: a tree of links and folders. The top level is the bookmarks
//! bar; folders nest to any depth. Links carry an address, folders don't,
//! so the flat lists of earlier versions read as a bar of links unchanged.
//!
//! Also here: bringing bookmarks in from other browsers (Safari, and those
//! built on Chromium), and the HTML bookmark file every browser can import
//! and export.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A link (with an address) or a folder (without one).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Node {
    /// Stable for a session, to name it in commands and drags; given out
    /// afresh on each launch.
    #[serde(skip)]
    pub id: u64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

impl Node {
    pub fn link(title: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            url: Some(url.into()),
            ..Self::default()
        }
    }

    pub fn folder(title: impl Into<String>, children: Vec<Node>) -> Self {
        Self {
            title: title.into(),
            children,
            ..Self::default()
        }
    }

    pub fn is_folder(&self) -> bool {
        self.url.is_none()
    }

    /// Whether `id` is this node or anything inside it.
    fn contains(&self, id: u64) -> bool {
        self.id == id || self.children.iter().any(|child| child.contains(id))
    }
}

/// Where something sits: in a folder, or at the top (the bookmarks bar).
pub type Folder = Option<u64>;

#[derive(Debug, Default)]
pub struct Bookmarks {
    root: Vec<Node>,
    next_id: u64,
}

impl Bookmarks {
    pub fn new(root: Vec<Node>) -> Self {
        let mut this = Self { root, next_id: 1 };
        let mut next = this.next_id;
        number(&mut this.root, &mut next);
        this.next_id = next;
        this
    }

    /// The tree as saved.
    pub fn root(&self) -> &[Node] {
        &self.root
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    /// Every link, depth first, in order.
    pub fn links(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        walk(&self.root, &mut |node, _| {
            if !node.is_folder() {
                out.push(node);
            }
        });
        out
    }

    /// Every link with the folders it's in, as "Work › Docs" (empty for the
    /// bar).
    pub fn links_with_paths(&self) -> Vec<(&Node, String)> {
        fn go<'a>(nodes: &'a [Node], path: &str, out: &mut Vec<(&'a Node, String)>) {
            for node in nodes {
                if node.is_folder() {
                    let inner = if path.is_empty() {
                        node.title.clone()
                    } else {
                        format!("{path} › {}", node.title)
                    };
                    go(&node.children, &inner, out);
                } else {
                    out.push((node, path.to_owned()));
                }
            }
        }
        let mut out = Vec::new();
        go(&self.root, "", &mut out);
        out
    }

    /// Every folder, depth first, with how deep it is (0 at the top).
    pub fn folders(&self) -> Vec<(u64, String, usize)> {
        let mut out = Vec::new();
        walk(&self.root, &mut |node, depth| {
            if node.is_folder() {
                out.push((node.id, node.title.clone(), depth));
            }
        });
        out
    }

    pub fn get(&self, id: u64) -> Option<&Node> {
        find(&self.root, id)
    }

    /// What's in a folder, or at the top.
    pub fn children(&self, folder: Folder) -> &[Node] {
        match folder {
            None => &self.root,
            Some(id) => self.get(id).map_or(&[], |node| &node.children),
        }
    }

    /// The folder `id` is in, if it exists.
    pub fn parent_of(&self, id: u64) -> Option<Folder> {
        fn go(nodes: &[Node], parent: Folder, id: u64) -> Option<Folder> {
            for node in nodes {
                if node.id == id {
                    return Some(parent);
                }
                if let Some(found) = go(&node.children, Some(node.id), id) {
                    return Some(found);
                }
            }
            None
        }
        go(&self.root, None, id)
    }

    /// The link to `url`, anywhere in the tree.
    pub fn find_url(&self, url: &str) -> Option<u64> {
        let url = url.trim_end_matches('/');
        self.links()
            .into_iter()
            .find(|node| node.url.as_deref().is_some_and(|u| u.trim_end_matches('/') == url))
            .map(|node| node.id)
    }

    /// Adds `node` (and anything in it) to a folder, at `index` or at the
    /// end. Returns its id.
    pub fn add(&mut self, folder: Folder, index: Option<usize>, mut node: Node) -> u64 {
        let mut next = self.next_id;
        number(std::slice::from_mut(&mut node), &mut next);
        self.next_id = next;
        let id = node.id;
        let Some(list) = self.children_mut(folder) else {
            // The folder went away: the top will do.
            self.root.push(node);
            return id;
        };
        let at = index.unwrap_or(list.len()).min(list.len());
        list.insert(at, node);
        id
    }

    pub fn remove(&mut self, id: u64) -> Option<Node> {
        fn go(nodes: &mut Vec<Node>, id: u64) -> Option<Node> {
            if let Some(at) = nodes.iter().position(|node| node.id == id) {
                return Some(nodes.remove(at));
            }
            nodes.iter_mut().find_map(|node| go(&mut node.children, id))
        }
        go(&mut self.root, id)
    }

    pub fn rename(&mut self, id: u64, title: &str) -> bool {
        self.get_mut(id).map(|node| node.title = title.to_owned()).is_some()
    }

    pub fn set_url(&mut self, id: u64, url: &str) -> bool {
        match self.get_mut(id) {
            Some(node) if !node.is_folder() => {
                node.url = Some(url.to_owned());
                true
            }
            _ => false,
        }
    }

    /// Moves `id` into a folder (or to the top), before whatever is at
    /// `index` there now, or to the end. A folder can't go inside itself.
    pub fn move_to(&mut self, id: u64, folder: Folder, index: Option<usize>) -> bool {
        if let Some(target) = folder
            && self.get(id).is_some_and(|node| node.contains(target))
        {
            return false;
        }
        let Some(parent) = self.parent_of(id) else {
            return false;
        };
        // Counted before the move, so a later place in the same folder
        // shifts down by the one taken out.
        let from = self.children(parent).iter().position(|node| node.id == id);
        let Some(node) = self.remove(id) else {
            return false;
        };
        let index = match (index, from) {
            (Some(index), Some(from)) if parent == folder && from < index => Some(index - 1),
            (index, _) => index,
        };
        let Some(list) = self.children_mut(folder) else {
            self.root.push(node);
            return true;
        };
        let at = index.unwrap_or(list.len()).min(list.len());
        list.insert(at, node);
        true
    }

    /// Moves several at once into `folder`, keeping their order, just
    /// before `before` there (or at the end): what a dragged selection
    /// does. Anything inside a folder also being moved goes with it rather
    /// than on its own. Whether anything moved.
    pub fn move_many(&mut self, ids: &[u64], folder: Folder, before: Option<u64>) -> bool {
        let moving: Vec<u64> = ids
            .iter()
            .copied()
            .filter(|&id| Some(id) != before)
            .filter(|&id| {
                !ids.iter().any(|&other| other != id && self.get(other).is_some_and(|node| node.contains(id)))
            })
            .collect();
        let mut moved = false;
        for id in moving {
            let index = before.and_then(|before| self.children(folder).iter().position(|node| node.id == before));
            moved |= self.move_to(id, folder, index);
        }
        moved
    }

    /// Moves `id` along its folder by `by` places.
    pub fn shift(&mut self, id: u64, by: isize) -> bool {
        let Some(parent) = self.parent_of(id) else {
            return false;
        };
        let Some(list) = self.children_mut(parent) else {
            return false;
        };
        let Some(from) = list.iter().position(|node| node.id == id) else {
            return false;
        };
        let to = from as isize + by;
        if to < 0 || to as usize >= list.len() {
            return false;
        }
        let node = list.remove(from);
        list.insert(to as usize, node);
        true
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Node> {
        fn go(nodes: &mut [Node], id: u64) -> Option<&mut Node> {
            for node in nodes {
                if node.id == id {
                    return Some(node);
                }
                if let Some(found) = go(&mut node.children, id) {
                    return Some(found);
                }
            }
            None
        }
        go(&mut self.root, id)
    }

    fn children_mut(&mut self, folder: Folder) -> Option<&mut Vec<Node>> {
        match folder {
            None => Some(&mut self.root),
            Some(id) => self.get_mut(id).filter(|node| node.is_folder()).map(|node| &mut node.children),
        }
    }
}

fn number(nodes: &mut [Node], next: &mut u64) {
    for node in nodes {
        node.id = *next;
        *next += 1;
        number(&mut node.children, next);
    }
}

fn walk<'a>(nodes: &'a [Node], visit: &mut impl FnMut(&'a Node, usize)) {
    fn go<'a>(nodes: &'a [Node], depth: usize, visit: &mut impl FnMut(&'a Node, usize)) {
        for node in nodes {
            visit(node, depth);
            go(&node.children, depth + 1, visit);
        }
    }
    go(nodes, 0, visit);
}

fn find(nodes: &[Node], id: u64) -> Option<&Node> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        if let Some(found) = find(&node.children, id) {
            return Some(found);
        }
    }
    None
}

// ---- Bringing bookmarks in and out -----------------------------------------

/// A browser to bring bookmarks in from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Safari,
    Chrome,
    Brave,
    Edge,
    Vivaldi,
    Arc,
}

impl Source {
    pub const ALL: [Source; 6] = [
        Source::Safari,
        Source::Chrome,
        Source::Brave,
        Source::Edge,
        Source::Vivaldi,
        Source::Arc,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Source::Safari => "Safari",
            Source::Chrome => "Google Chrome",
            Source::Brave => "Brave",
            Source::Edge => "Microsoft Edge",
            Source::Vivaldi => "Vivaldi",
            Source::Arc => "Arc",
        }
    }

    fn file(self) -> Option<PathBuf> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let support = home.join("Library/Application Support");
        Some(match self {
            Source::Safari => home.join("Library/Safari/Bookmarks.plist"),
            Source::Chrome => support.join("Google/Chrome/Default/Bookmarks"),
            Source::Brave => support.join("BraveSoftware/Brave-Browser/Default/Bookmarks"),
            Source::Edge => support.join("Microsoft Edge/Default/Bookmarks"),
            Source::Vivaldi => support.join("Vivaldi/Default/Bookmarks"),
            Source::Arc => support.join("Arc/User Data/Default/Bookmarks"),
        })
    }

    /// Whether this browser seems to be installed with bookmarks to bring in.
    pub fn available(self) -> bool {
        self.file().is_some_and(|file| file.exists())
    }

    /// Its bookmarks, as a folder named for it.
    pub fn import(self) -> Result<Node, String> {
        let file = self.file().ok_or("HOME is unset")?;
        let children = match self {
            Source::Safari => read_safari(&file)?,
            _ => read_chromium(&file)?,
        };
        Ok(Node::folder(format!("From {}", self.name()), children))
    }
}

/// Chromium's `Bookmarks` file: JSON, with the bar and "other bookmarks" as
/// roots.
fn read_chromium(file: &Path) -> Result<Vec<Node>, String> {
    let text = std::fs::read_to_string(file).map_err(|err| format!("Couldn't read {file:?}: {err}"))?;
    let json: Value = serde_json::from_str(&text).map_err(|err| format!("Not a bookmarks file: {err}"))?;
    fn convert(value: &Value) -> Option<Node> {
        let title = value["name"].as_str().unwrap_or_default();
        match value["type"].as_str()? {
            "url" => Some(Node::link(title, value["url"].as_str()?)),
            "folder" => Some(Node::folder(title, children(value))),
            _ => None,
        }
    }
    fn children(value: &Value) -> Vec<Node> {
        value["children"]
            .as_array()
            .map(|items| items.iter().filter_map(convert).collect())
            .unwrap_or_default()
    }
    let roots = &json["roots"];
    let mut out = children(&roots["bookmark_bar"]);
    for (key, title) in [("other", "Other Bookmarks"), ("synced", "Mobile Bookmarks")] {
        let inner = children(&roots[key]);
        if !inner.is_empty() {
            out.push(Node::folder(title, inner));
        }
    }
    Ok(out)
}

/// Safari's `Bookmarks.plist`. macOS guards it: reading needs Full Disk
/// Access for this app.
fn read_safari(file: &Path) -> Result<Vec<Node>, String> {
    let root = plist::Value::from_file(file).map_err(|err| {
        if err.to_string().contains("ermission") || err.to_string().contains("not permitted") {
            "macOS won't let Vamprowser read Safari's bookmarks. Allow it in System Settings → Privacy & Security → Full Disk Access, or export them from Safari (File → Export → Bookmarks) and import the file.".to_owned()
        } else {
            format!("Couldn't read Safari's bookmarks: {err}")
        }
    })?;
    fn convert(value: &plist::Value) -> Option<Node> {
        let dict = value.as_dictionary()?;
        let kind = dict.get("WebBookmarkType")?.as_string()?;
        match kind {
            "WebBookmarkTypeLeaf" => {
                let url = dict.get("URLString")?.as_string()?;
                let title = dict
                    .get("URIDictionary")
                    .and_then(|d| d.as_dictionary())
                    .and_then(|d| d.get("title"))
                    .and_then(|t| t.as_string())
                    .unwrap_or(url);
                Some(Node::link(title, url))
            }
            "WebBookmarkTypeList" => {
                let title = dict.get("Title").and_then(|t| t.as_string()).unwrap_or_default();
                // Reading List isn't bookmarks.
                if title == "com.apple.ReadingList" {
                    return None;
                }
                let title = match title {
                    "BookmarksBar" => "Favourites",
                    "BookmarksMenu" => "Bookmarks Menu",
                    other => other,
                };
                Some(Node::folder(title, children(dict)))
            }
            _ => None,
        }
    }
    fn children(dict: &plist::Dictionary) -> Vec<Node> {
        dict.get("Children")
            .and_then(|c| c.as_array())
            .map(|items| items.iter().filter_map(convert).collect())
            .unwrap_or_default()
    }
    let dict = root.as_dictionary().ok_or("Not a bookmarks file")?;
    Ok(children(dict))
}

/// The HTML bookmark file (the "Netscape" format) every browser exports:
/// `<DT><H3>` opens a folder, `<DT><A HREF>` is a link, `</DL>` closes a
/// folder.
pub fn read_html(html: &str) -> Vec<Node> {
    let mut stack: Vec<Node> = vec![Node::folder("", Vec::new())];
    // A folder's heading comes before its list opens.
    let mut pending: Option<String> = None;
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        rest = &rest[at..];
        let upper: String = rest.chars().take(4).collect::<String>().to_ascii_uppercase();
        if upper.starts_with("<H3") {
            let title = inner_text(rest, "</H3>");
            pending = Some(title);
        } else if upper.starts_with("<DL") {
            if let Some(title) = pending.take() {
                stack.push(Node::folder(title, Vec::new()));
            }
        } else if upper.starts_with("</DL") {
            if stack.len() > 1 {
                let folder = stack.pop().expect("more than the root");
                stack.last_mut().expect("root").children.push(folder);
            }
        } else if upper.starts_with("<A ") || upper.starts_with("<A\t") {
            let tag_end = rest.find('>').unwrap_or(rest.len());
            if let Some(url) = attribute(&rest[..tag_end], "HREF") {
                let title = inner_text(rest, "</A>");
                let title = if title.is_empty() { url.clone() } else { title };
                stack.last_mut().expect("root").children.push(Node::link(title, url));
            }
        }
        rest = &rest[1..];
    }
    // Folders left open by a sloppy file still count.
    while stack.len() > 1 {
        let folder = stack.pop().expect("more than the root");
        stack.last_mut().expect("root").children.push(folder);
    }
    let mut root = stack.pop().expect("root").children;
    // Most exports wrap everything in one top folder; unwrap it.
    if root.len() == 1 && root[0].is_folder() && root[0].title.is_empty() {
        root = root.remove(0).children;
    }
    root
}

/// The text between a tag's `>` and `close`, with entities decoded.
fn inner_text(from: &str, close: &str) -> String {
    let start = from.find('>').map_or(from.len(), |i| i + 1);
    let end = find_ignoring_case(&from[start..], close).unwrap_or(from.len() - start);
    decode(from[start..start + end].trim())
}

/// Where ASCII `needle` first appears in `haystack`, in any case. Without
/// copying the rest of the file in upper case at each tag, which made a big
/// export (icons inlined, megabytes long) take minutes to import.
fn find_ignoring_case(haystack: &str, needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let upper = tag.to_ascii_uppercase();
    let at = upper.find(&format!("{name}="))? + name.len() + 1;
    let value = &tag[at..];
    let (quote, value) = match value.chars().next()? {
        q @ ('"' | '\'') => (q, &value[1..]),
        _ => (' ', value),
    };
    let end = value.find(quote).unwrap_or(value.len());
    Some(decode(&value[..end]))
}

fn decode(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

fn encode(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The whole tree as an HTML bookmark file, which any browser can import.
pub fn write_html(root: &[Node]) -> String {
    fn go(nodes: &[Node], depth: usize, out: &mut String) {
        let indent = "    ".repeat(depth);
        for node in nodes {
            match &node.url {
                Some(url) => {
                    let _ = writeln!(
                        out,
                        "{indent}<DT><A HREF=\"{}\">{}</A>",
                        encode(url),
                        encode(&node.title)
                    );
                }
                None => {
                    let _ = writeln!(out, "{indent}<DT><H3>{}</H3>", encode(&node.title));
                    let _ = writeln!(out, "{indent}<DL><p>");
                    go(&node.children, depth + 1, out);
                    let _ = writeln!(out, "{indent}</DL><p>");
                }
            }
        }
    }
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n\
         <META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n\
         <TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n",
    );
    go(root, 1, &mut out);
    out.push_str("</DL><p>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Bookmarks {
        Bookmarks::new(vec![
            Node::link("A", "https://a.example/"),
            Node::folder(
                "Work",
                vec![
                    Node::link("B", "https://b.example/"),
                    Node::folder("Docs", vec![Node::link("C", "https://c.example/")]),
                ],
            ),
            Node::link("D", "https://d.example/"),
        ])
    }

    #[test]
    fn several_move_together_in_order() {
        let mut bookmarks = tree();
        let ids: Vec<u64> = bookmarks.root().iter().map(|n| n.id).collect();
        let (a, work, d) = (ids[0], ids[1], ids[2]);
        let b = bookmarks.children(Some(work))[0].id;
        // D and B go before A, in that order.
        assert!(bookmarks.move_many(&[d, b], None, Some(a)));
        assert_eq!(titles(bookmarks.root()), ["D", "B", "A", "Work"]);
        // A folder and something in it: the folder takes it along.
        let docs = bookmarks.children(Some(work))[0].id;
        let c = bookmarks.children(Some(docs))[0].id;
        assert!(bookmarks.move_many(&[docs, c], None, None));
        assert_eq!(titles(bookmarks.root()), ["D", "B", "A", "Work", "Docs"]);
        assert_eq!(bookmarks.children(Some(docs)).len(), 1);
    }

    fn titles(nodes: &[Node]) -> Vec<&str> {
        nodes.iter().map(|n| n.title.as_str()).collect()
    }

    #[test]
    fn old_flat_lists_read_as_links_on_the_bar() {
        let old: Vec<Node> =
            serde_json::from_str(r#"[{"title":"A","url":"https://a.example/"}]"#).unwrap();
        let bookmarks = Bookmarks::new(old);
        assert_eq!(bookmarks.links().len(), 1);
        assert!(!bookmarks.root()[0].is_folder());
        // Folders save without an address, and empty ones stay folders.
        let saved = serde_json::to_string(&Node::folder("Empty", vec![])).unwrap();
        assert_eq!(saved, r#"{"title":"Empty"}"#);
        let back: Node = serde_json::from_str(&saved).unwrap();
        assert!(back.is_folder());
    }

    #[test]
    fn walks_finds_and_paths() {
        let b = tree();
        assert_eq!(titles(&b.links().into_iter().cloned().collect::<Vec<_>>()), ["A", "B", "C", "D"]);
        let paths: Vec<String> = b.links_with_paths().into_iter().map(|(_, p)| p).collect();
        assert_eq!(paths, ["", "Work", "Work › Docs", ""]);
        let c = b.find_url("https://c.example").unwrap();
        let docs = b.folders().iter().find(|f| f.1 == "Docs").unwrap().0;
        assert_eq!(b.parent_of(c), Some(Some(docs)));
        assert_eq!(b.folders().iter().map(|f| f.2).collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn moving_within_and_between_folders() {
        let mut b = tree();
        let a = b.find_url("https://a.example/").unwrap();
        let work = b.folders()[0].0;
        let docs = b.folders()[1].0;
        // Along the bar: A goes before D (index 2 before the move).
        assert!(b.move_to(a, None, Some(2)));
        assert_eq!(titles(b.children(None)), ["Work", "A", "D"]);
        // Into a folder, at its start.
        assert!(b.move_to(a, Some(docs), Some(0)));
        assert_eq!(titles(b.children(Some(docs))), ["A", "C"]);
        // A folder can't go inside itself.
        assert!(!b.move_to(work, Some(docs), None));
        assert!(b.shift(a, 1));
        assert_eq!(titles(b.children(Some(docs))), ["C", "A"]);
        let removed = b.remove(work).unwrap();
        assert_eq!(removed.children.len(), 2);
        assert_eq!(titles(b.children(None)), ["D"]);
    }

    #[test]
    fn added_nodes_get_fresh_ids() {
        let mut b = tree();
        let id = b.add(None, Some(0), Node::folder("New", vec![Node::link("E", "https://e.example/")]));
        assert_eq!(b.children(None)[0].id, id);
        let e = b.find_url("https://e.example/").unwrap();
        assert_ne!(e, id);
        assert_eq!(b.parent_of(e), Some(Some(id)));
    }

    #[test]
    fn html_files_round_trip() {
        let b = tree();
        let html = write_html(b.root());
        let back = Bookmarks::new(read_html(&html));
        let paths: Vec<(String, String)> = back
            .links_with_paths()
            .into_iter()
            .map(|(n, p)| (n.title.clone(), p))
            .collect();
        assert_eq!(
            paths,
            [
                ("A".into(), "".into()),
                ("B".into(), "Work".into()),
                ("C".into(), "Work › Docs".into()),
                ("D".into(), "".into())
            ]
        );
        // What Firefox and Chrome write, give or take.
        let exported = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<DL><p>
    <DT><H3 ADD_DATE="1" PERSONAL_TOOLBAR_FOLDER="true">Bookmarks bar</H3>
    <DL><p>
        <DT><A HREF="https://x.example/?a=1&amp;b=2" ADD_DATE="2">X &amp; Y</A>
    </DL><p>
    <DT><A HREF='https://z.example/'>Z</A>
</DL><p>"#;
        let nodes = read_html(exported);
        assert_eq!(nodes[0].title, "Bookmarks bar");
        assert_eq!(nodes[0].children[0].title, "X & Y");
        assert_eq!(nodes[0].children[0].url.as_deref(), Some("https://x.example/?a=1&b=2"));
        assert_eq!(nodes[1].url.as_deref(), Some("https://z.example/"));
    }

    #[test]
    fn chromium_files_read() {
        let dir = std::env::temp_dir().join(format!("vamp-bm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Bookmarks");
        std::fs::write(
            &file,
            r#"{"roots":{"bookmark_bar":{"children":[{"type":"url","name":"A","url":"https://a/"},
               {"type":"folder","name":"F","children":[{"type":"url","name":"B","url":"https://b/"}]}]},
               "other":{"children":[{"type":"url","name":"O","url":"https://o/"}]},"synced":{"children":[]}}}"#,
        )
        .unwrap();
        let nodes = read_chromium(&file).unwrap();
        assert_eq!(titles(&nodes), ["A", "F", "Other Bookmarks"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
