//! Site icons. A loaded page reports the icons it links to; a bookmark
//! whose site hasn't been opened has its HTML read for them instead. The
//! best candidate is fetched off the main thread, rasterised if it is SVG,
//! scaled to at most [`ICON_PX`], and cached on disk as PNG per site — so
//! bookmarks and restored tabs show their icons before anything loads.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Cursor,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use gpui::{Image, ImageFormat};
use image::{RgbaImage, imageops::FilterType};
use serde::Deserialize;
use url::Url;

/// Cached icons are at most this many pixels square.
pub const ICON_PX: u32 = 64;
/// An icon older than this is shown but fetched again.
const FRESH: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// A site found to have no icon isn't asked again for this long.
const RETRY_MISSING: Duration = Duration::from_secs(3 * 24 * 60 * 60);
const MAX_IMAGE_BYTES: u64 = 1 << 20;
const MAX_HTML_BYTES: u64 = 768 << 10;
/// How many linked icons to try before falling back to `/favicon.ico`.
const MAX_ATTEMPTS: usize = 4;

/// Lists the page's icon links as JSON; `href` is already absolute.
pub const DISCOVER_SCRIPT: &str = r#"(() => JSON.stringify(
  [...document.querySelectorAll('link[rel][href]')]
    .filter((l) => /(^|\s)(icon|apple-touch-icon(-precomposed)?)(\s|$)/i.test(l.rel))
    .map((l) => ({ href: l.href, rel: l.rel, sizes: l.getAttribute('sizes') || '', kind: l.type || '' }))
))()"#;

/// One `<link rel="icon">` or similar.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Candidate {
    pub href: String,
    #[serde(default)]
    pub rel: String,
    #[serde(default)]
    pub sizes: String,
    #[serde(default)]
    pub kind: String,
}

impl Candidate {
    /// Higher is better: scalable, or a bitmap near [`ICON_PX`].
    fn score(&self) -> i32 {
        let path = self.href.split(['?', '#']).next().unwrap_or("");
        let path = path.to_ascii_lowercase();
        if self.kind.contains("svg") || path.ends_with(".svg") {
            return 950;
        }
        if self.sizes.eq_ignore_ascii_case("any") {
            return 900;
        }
        let size = self
            .sizes
            .split_whitespace()
            .filter_map(|s| s.split(['x', 'X']).next()?.parse::<i32>().ok())
            .max();
        match size {
            Some(size) if size >= 32 => 1000 - (size - ICON_PX as i32).abs().min(800),
            Some(size) => 100 + size,
            None if self.rel.to_ascii_lowercase().contains("apple-touch") => 880,
            None if path.ends_with(".ico") => 300,
            None => 400,
        }
    }
}

/// A decoded icon ready to draw.
#[derive(Clone)]
pub struct Favicon {
    pub image: Arc<Image>,
    /// Mostly dark ink, which needs a light plate behind it on a dark theme.
    pub dark: bool,
    /// The icon's own colour, for a page that has none.
    pub tint: Option<crate::tint::Tint>,
}

/// The site an icon belongs to: its host without `www.`.
pub fn site_key(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    // The URL parser has already lowercased (and punycoded) web hosts.
    let host = url.host_str()?;
    Some(host.strip_prefix("www.").unwrap_or(host).to_owned())
}

/// [`site_key`] without parsing or allocating, for the everyday address
/// whose host the parser would leave exactly as written: lowercase ASCII
/// letters, digits, dots and dashes, ending in a name rather than a number
/// (which the parser would read as an IPv4 address). `None` means "ask
/// `site_key`", not "no site".
fn plain_site_key(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#', '\\']).next()?;
    // Credentials and ports are left to the parser.
    let host = authority.strip_prefix("www.").unwrap_or(authority);
    let plain = host
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
    let named = host
        .trim_end_matches('.')
        .rsplit('.')
        .next()
        .is_some_and(|last| last.starts_with(|c: char| c.is_ascii_lowercase()) && !last.starts_with("0x"));
    (plain && named).then_some(host)
}

/// Icons in memory, and the bookkeeping that keeps each site to one fetch.
pub struct Favicons {
    dir: Option<PathBuf>,
    icons: HashMap<String, Favicon>,
    pending: HashSet<String>,
    /// Sites already fetched with a page's own links this session,
    /// successfully or not.
    tried: HashSet<String>,
    /// A page's own links that arrived while its site was being fetched
    /// without them, with the page's address: tried if that finds nothing.
    waiting: HashMap<String, (String, Vec<Candidate>)>,
    /// Sites looked for without a page's links, as lists do for sites not
    /// open: once a session is enough, and a page's own links still count.
    looked_up: HashSet<String>,
}

impl Favicons {
    pub fn new() -> Self {
        let dir = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Caches/Vamprowser/Favicons"));
        Self {
            dir,
            icons: HashMap::new(),
            pending: HashSet::new(),
            tried: HashSet::new(),
            waiting: HashMap::new(),
            looked_up: HashSet::new(),
        }
    }

    /// The icon for `url`'s site, if it is in memory. Drawn for every tab
    /// and row on every frame, so the usual address is looked up without
    /// allocating.
    pub fn get(&self, url: &str) -> Option<&Favicon> {
        match plain_site_key(url) {
            Some(key) => self.get_by_key(key),
            None => self.get_by_key(&site_key(url)?),
        }
    }

    /// The icon for a site already turned into its [`site_key`].
    pub fn get_by_key(&self, key: &str) -> Option<&Favicon> {
        self.icons.get(key)
    }

    /// Brings `url`'s site icon in from disk, if it is cached there.
    pub fn load(&mut self, url: &str) {
        let Some(key) = site_key(url) else {
            return;
        };
        if self.icons.contains_key(&key) {
            return;
        }
        if let Some(icon) = self
            .path(&key, "png")
            .and_then(|path| fs::read(path).ok())
            .and_then(decode)
        {
            self.icons.insert(key, icon);
        }
    }

    /// Makes sure `url`'s site has an icon: from memory, from disk, or else
    /// fetched in the background, calling `done` with the PNG (or `None`)
    /// on that thread. `candidates` are the page's own links when it has
    /// loaded; without them the site's HTML is read for links.
    pub fn want(
        &mut self,
        url: &str,
        candidates: Option<Vec<Candidate>>,
        done: impl FnOnce(String, Option<Vec<u8>>) + Send + 'static,
    ) {
        let Some(key) = site_key(url) else {
            return;
        };
        if self.pending.contains(&key) {
            if let Some(candidates) = candidates.filter(|c| !c.is_empty()) {
                self.waiting.insert(key, (url.to_owned(), candidates));
            }
            return;
        }
        if self.tried.contains(&key) {
            return;
        }
        if candidates.is_none() && !self.looked_up.insert(key.clone()) {
            return;
        }
        self.load(url);
        let fresh = |path: Option<PathBuf>, within: Duration| {
            path.and_then(|path| fs::metadata(path).ok())
                .and_then(|meta| meta.modified().ok())
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age < within)
        };
        if self.icons.contains_key(&key) && fresh(self.path(&key, "png"), FRESH) {
            return;
        }
        // A page's own links are worth trying even if its HTML had none:
        // they may have been added by script.
        let page_has_links = candidates.as_ref().is_some_and(|c| !c.is_empty());
        if !page_has_links && fresh(self.path(&key, "none"), RETRY_MISSING) {
            return;
        }
        self.pending.insert(key.clone());
        if candidates.is_some() {
            self.tried.insert(key.clone());
        }
        let png_path = self.path(&key, "png");
        let none_path = self.path(&key, "none");
        let url = url.to_owned();
        in_background(move || {
            let png = fetch(&url, candidates);
            if let (Some(dir), Some(png_path), Some(none_path)) = (
                png_path.as_ref().and_then(|p| p.parent()),
                &png_path,
                &none_path,
            ) {
                let _ = fs::create_dir_all(dir);
                match &png {
                    Some(png) => {
                        let _ = crate::state::write_atomic(png_path, png);
                        let _ = fs::remove_file(none_path);
                    }
                    // Keep a stale icon rather than forgetting it.
                    None if !png_path.exists() => {
                        let _ = fs::write(none_path, b"");
                    }
                    None => {}
                }
            }
            done(key, png);
        });
    }

    /// Takes in the result of a background fetch. Returns the page's own
    /// links to try next, with its address, if they came during a fetch
    /// without them that found nothing.
    pub fn fetched(&mut self, key: String, png: Option<Vec<u8>>) -> Option<(String, Vec<Candidate>)> {
        self.pending.remove(&key);
        let waiting = self.waiting.remove(&key);
        match png.and_then(decode) {
            Some(icon) => {
                self.icons.insert(key, icon);
                None
            }
            None => waiting,
        }
    }

    fn path(&self, key: &str, extension: &str) -> Option<PathBuf> {
        let name: String = key
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Some(self.dir.as_ref()?.join(format!("{name}.{extension}")))
    }
}

/// Runs `job` on one of a few threads kept for fetching icons, rather than
/// a thread of its own: importing thousands of bookmarks would otherwise
/// start thousands of threads, each making requests, at once.
fn in_background(job: impl FnOnce() + Send + 'static) {
    type Job = Box<dyn FnOnce() + Send>;
    static QUEUE: std::sync::OnceLock<async_channel::Sender<Job>> = std::sync::OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (sender, receiver) = async_channel::unbounded::<Job>();
        for _ in 0..6 {
            let receiver = receiver.clone();
            std::thread::spawn(move || {
                while let Ok(job) = receiver.recv_blocking() {
                    job();
                }
            });
        }
        sender
    });
    let _ = queue.try_send(Box::new(job));
}

/// Parses the page script's result, which WebKit hands back JSON-encoded.
pub fn parse_discovered(result: &str) -> Vec<Candidate> {
    let inner = serde_json::from_str::<String>(result).unwrap_or_else(|_| result.to_owned());
    serde_json::from_str(&inner).unwrap_or_default()
}

fn decode(png: Vec<u8>) -> Option<Favicon> {
    let pixels = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    Some(Favicon {
        dark: is_dark_ink(&pixels),
        tint: crate::tint::dominant_tint(pixels.pixels().map(|p| p.0)),
        image: Arc::new(Image::from_bytes(ImageFormat::Png, png)),
    })
}

/// Whether the icon's visible pixels are mostly near-black.
fn is_dark_ink(pixels: &RgbaImage) -> bool {
    let (mut total, mut count) = (0.0, 0u32);
    for p in pixels.pixels() {
        if p[3] > 128 {
            total += 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
            count += 1;
        }
    }
    count > 0 && total / f64::from(count) < 60.0
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
        )
        .build()
        .into()
}

/// Finds and fetches the best icon for `page_url`, as normalised PNG.
fn fetch(page_url: &str, candidates: Option<Vec<Candidate>>) -> Option<Vec<u8>> {
    let page = Url::parse(page_url).ok()?;
    let agent = agent();
    let candidates = match candidates {
        Some(candidates) if !candidates.is_empty() => candidates,
        // The page said it links none: its `/favicon.ico` is all there is.
        Some(_) => Vec::new(),
        // The site's home page for links, never the page itself: fetching
        // an address twice could use up a one-time link (a sign-in, say).
        None => page
            .join("/")
            .ok()
            .and_then(|home| html(&agent, &home).map(|html| links_in_html(&html, &home)))
            .unwrap_or_default(),
    };
    ranked(candidates, &page).into_iter().find_map(|href| {
        download(&agent, &href).and_then(|(bytes, mime)| normalise(&bytes, mime.as_deref()))
    })
}

/// The hrefs to try, best first, ending with the site's `/favicon.ico`.
fn ranked(mut candidates: Vec<Candidate>, page: &Url) -> Vec<String> {
    candidates.retain(|c| c.href.starts_with("http://") || c.href.starts_with("https://"));
    candidates.sort_by_key(|c| std::cmp::Reverse(c.score()));
    let mut hrefs: Vec<String> = Vec::new();
    for candidate in candidates {
        if !hrefs.contains(&candidate.href) && hrefs.len() < MAX_ATTEMPTS {
            hrefs.push(candidate.href);
        }
    }
    if let Ok(fallback) = page.join("/favicon.ico") {
        let fallback = fallback.to_string();
        if !hrefs.contains(&fallback) {
            hrefs.push(fallback);
        }
    }
    hrefs
}

fn html(agent: &ureq::Agent, page: &Url) -> Option<String> {
    let mut response = agent.get(page.as_str()).call().ok()?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_HTML_BYTES)
        .read_to_vec()
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn download(agent: &ureq::Agent, href: &str) -> Option<(Vec<u8>, Option<String>)> {
    let mut response = agent.get(href).call().ok()?;
    let mime = response.body().mime_type().map(str::to_owned);
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_IMAGE_BYTES)
        .read_to_vec()
        .ok()?;
    Some((bytes, mime))
}

/// Any image format a site might serve, as a square PNG of at most
/// [`ICON_PX`]. Blank or unreadable images are rejected so the next
/// candidate gets its turn.
fn normalise(bytes: &[u8], mime: Option<&str>) -> Option<Vec<u8>> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    let svg = mime.is_some_and(|m| m.contains("svg")) || head.contains("<svg");
    let pixels = if svg {
        rasterise_svg(bytes)?
    } else {
        if mime.is_some_and(|m| m.starts_with("text/")) {
            return None;
        }
        image::load_from_memory(bytes).ok()?.into_rgba8()
    };
    if pixels.width() < 8 || pixels.height() < 8 || pixels.pixels().all(|p| p[3] == 0) {
        return None;
    }
    let pixels = square(pixels);
    let pixels = if pixels.width() > ICON_PX {
        image::imageops::resize(&pixels, ICON_PX, ICON_PX, FilterType::Lanczos3)
    } else {
        pixels
    };
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// Centres a non-square image on a transparent square.
fn square(pixels: RgbaImage) -> RgbaImage {
    let (w, h) = pixels.dimensions();
    if w == h {
        return pixels;
    }
    let side = w.max(h);
    let mut canvas = RgbaImage::new(side, side);
    image::imageops::overlay(
        &mut canvas,
        &pixels,
        i64::from((side - w) / 2),
        i64::from((side - h) / 2),
    );
    canvas
}

fn rasterise_svg(bytes: &[u8]) -> Option<RgbaImage> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = ICON_PX as f32 / size.width().max(size.height());
    let (w, h) = (size.width() * scale, size.height() * scale);
    let mut pixmap = tiny_skia::Pixmap::new(ICON_PX, ICON_PX)?;
    let transform = tiny_skia::Transform::from_scale(scale, scale)
        .post_translate((ICON_PX as f32 - w) / 2.0, (ICON_PX as f32 - h) / 2.0);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // tiny-skia keeps premultiplied alpha; PNG wants it straight.
    let mut data = pixmap.take();
    for p in data.chunks_exact_mut(4) {
        let a = u32::from(p[3]);
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(ICON_PX, ICON_PX, data)
}

/// The icon links in an HTML document, resolved against `base`.
fn links_in_html(html: &str, base: &Url) -> Vec<Candidate> {
    // ASCII lowercasing keeps every byte offset, so positions found in
    // `lower` slice `html` at the same characters.
    let lower = html.to_ascii_lowercase();
    let head_end = lower.find("</head").unwrap_or(lower.len());
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = lower[from..head_end].find("<link") {
        let start = from + offset + "<link".len();
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        let attrs = attributes(&html[start..end]);
        from = end.min(head_end);
        let get = |name: &str| {
            attrs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        let rel = get("rel");
        let is_icon = rel.split_whitespace().any(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "icon" | "apple-touch-icon" | "apple-touch-icon-precomposed"
            )
        });
        if !is_icon {
            continue;
        }
        if let Ok(href) = base.join(get("href").trim()) {
            found.push(Candidate {
                href: href.to_string(),
                rel,
                sizes: get("sizes"),
                kind: get("type"),
            });
        }
    }
    found
}

/// `name=value` pairs in a tag, names lowercased, entities left alone
/// except `&amp;`, which appears in icon URLs with query strings.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = tag.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace() || *c == '/').is_some() {}
        let mut name = String::new();
        while let Some(c) = chars.next_if(|c| !c.is_whitespace() && *c != '=' && *c != '/') {
            name.push(c.to_ascii_lowercase());
        }
        if name.is_empty() {
            break;
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut value = String::new();
        if chars.next_if_eq(&'=').is_some() {
            while chars.next_if(|c| c.is_whitespace()).is_some() {}
            match chars.peek().copied() {
                Some(quote @ ('"' | '\'')) => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c == quote {
                            break;
                        }
                        value.push(c);
                    }
                }
                _ => {
                    while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                        value.push(c);
                    }
                }
            }
        }
        out.push((name, value.replace("&amp;", "&")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_icon_links_in_html() {
        let base = Url::parse("https://example.org/a/page").unwrap();
        let html = r#"<html><head>
            <link rel="stylesheet" href="/s.css">
            <LINK REL='shortcut icon' HREF='/favicon.ico'>
            <link rel=icon type="image/png" sizes="32x32" href="icons/32.png?v=1&amp;x=2" />
            <link rel="mask-icon" href="/mask.svg">
            <link rel="apple-touch-icon" sizes="180x180" href="https://cdn.example.org/t.png">
            </head><body><link rel="icon" href="/late.png"></body></html>"#;
        let links = links_in_html(html, &base);
        let hrefs: Vec<&str> = links.iter().map(|c| c.href.as_str()).collect();
        assert_eq!(
            hrefs,
            [
                "https://example.org/favicon.ico",
                "https://example.org/a/icons/32.png?v=1&x=2",
                "https://cdn.example.org/t.png",
            ]
        );
        assert_eq!(links[1].sizes, "32x32");
    }

    #[test]
    fn ranks_scalable_and_near_sized_icons_first_and_falls_back_to_favicon_ico() {
        let page = Url::parse("https://example.org/x").unwrap();
        let candidate = |href: &str, sizes: &str| Candidate {
            href: href.into(),
            rel: "icon".into(),
            sizes: sizes.into(),
            kind: String::new(),
        };
        let ranked = ranked(
            vec![
                candidate("https://example.org/16.png", "16x16"),
                candidate("https://example.org/512.png", "512x512"),
                candidate("https://example.org/64.png", "64x64"),
                candidate("https://example.org/i.svg", ""),
                candidate("data:image/png;base64,AAAA", ""),
            ],
            &page,
        );
        assert_eq!(
            ranked,
            [
                "https://example.org/64.png",
                "https://example.org/i.svg",
                "https://example.org/512.png",
                "https://example.org/16.png",
                "https://example.org/favicon.ico",
            ]
        );
    }

    #[test]
    fn site_keys_ignore_www_and_non_web_urls() {
        assert_eq!(
            site_key("https://www.GitHub.com/a").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            site_key("https://news.ycombinator.com/").as_deref(),
            Some("news.ycombinator.com")
        );
        assert_eq!(site_key("file:///etc/hosts"), None);
    }

    #[test]
    fn the_quick_site_key_agrees_with_the_parser_or_defers() {
        for url in [
            "https://www.github.com/a?b#c",
            "http://news.ycombinator.com",
            "https://example.org./x",
            "https://docs.rs\\gpui",
            "https://WWW.GitHub.com/",
            "https://user@example.org/",
            "https://example.org:8443/",
            "https://127.0.0.1/",
            "https://0x7f.1/",
            "https://bücher.de/",
            "file:///etc/hosts",
            "about:blank",
        ] {
            if let Some(quick) = plain_site_key(url) {
                assert_eq!(Some(quick.to_owned()), site_key(url), "{url}");
            }
        }
        assert_eq!(plain_site_key("https://www.github.com/a"), Some("github.com"));
        assert_eq!(plain_site_key("https://127.0.0.1/"), None);
        assert_eq!(plain_site_key("https://user@example.org/"), None);
    }

    #[test]
    fn normalises_bitmaps_and_svg_to_small_square_png() {
        let mut big = RgbaImage::new(128, 96);
        for p in big.pixels_mut() {
            *p = image::Rgba([200, 30, 30, 255]);
        }
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(big)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let png = normalise(&bytes, Some("image/png")).unwrap();
        let out = image::load_from_memory(&png).unwrap();
        assert_eq!((out.width(), out.height()), (ICON_PX, ICON_PX));

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="black"/></svg>"#;
        let png = normalise(svg, None).unwrap();
        let icon = decode(png).unwrap();
        assert!(icon.dark);

        assert_eq!(
            normalise(b"<html>not found</html>", Some("text/html")),
            None
        );
        let blank = RgbaImage::new(16, 16);
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(blank)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(normalise(&bytes, None), None);
    }

    #[test]
    fn parses_discovered_links_from_webkit() {
        let json = r#""[{\"href\":\"https://a.org/i.png\",\"rel\":\"icon\",\"sizes\":\"\",\"kind\":\"\"}]""#;
        assert_eq!(parse_discovered(json)[0].href, "https://a.org/i.png");
        assert!(parse_discovered("garbage").is_empty());
    }
}
