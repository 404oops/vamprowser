//! uBlock Origin's network filtering, carried out by WebKit.
//!
//! WebKit ignores the `webRequest` replies uBlock Origin blocks with, so its
//! popup can count a request as blocked while the request goes ahead. This
//! makes the blocking real: the filter lists uBlock Origin has selected
//! (reported by a small bridge script inside it) are fetched from the
//! addresses in its own asset registry, converted to WebKit content rules,
//! and enforced in WebKit's network stack. uBlock Origin keeps doing
//! everything else — its popup, cosmetic filtering, scriptlets.
//!
//! Your own filters ("My filters"), lists imported by address, your dynamic
//! filtering rules ("My rules") and the per-site switches (no pop-ups, no
//! scripting, no remote fonts) are carried over too, and `$badfilter`
//! switches off what it names, as in uBlock Origin.
//!
//! Only what WebKit can express is converted; a filter with an option it
//! can't honour (`$removeparam`, `$csp`, a script `$redirect`, …) is left out
//! rather than applied loosely, since a looser rule could block what the
//! filter author meant to allow. Rules WebKit is most likely to reject —
//! translated regular expressions, and the dynamic rules — go in lists of
//! their own, so a rejection can't take the rest down with it.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, SystemTime},
};

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// uBlock Origin's extension id.
pub const UBLOCK_ID: &str = "uBlock0@raymondhill.net";
/// The application name its bridge script sends native messages to.
pub const BRIDGE_APP: &str = "dev.oops404.vamprowser.ublock";
/// uBlock Origin refreshes its lists about this often; so do we.
const REFRESH: Duration = Duration::from_secs(4 * 24 * 60 * 60);
/// Rules per compiled list. WebKit takes up to 150,000; smaller lists
/// compile faster and one bad rule costs less.
const CHUNK: usize = 50_000;

/// What the bridge script inside uBlock Origin reports.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct UblockState {
    /// Asset keys of the enabled filter lists, like `easylist`.
    pub selected_filter_lists: Vec<String>,
    /// Sites uBlock Origin is switched off for.
    pub net_whitelist: Vec<String>,
    /// "My filters".
    pub user_filters: String,
    /// "My rules": dynamic filtering, one `source destination type action`
    /// rule a line.
    pub dynamic_filtering_string: String,
    /// Per-site switches, one `switch: site true|false` a line.
    pub hostname_switches_string: String,
}

/// uBlock Origin's asset registry, `assets/assets.json`: what each list is
/// and where it can be downloaded. Empty if it can't be read.
fn read_assets(extension_dir: &Path) -> Option<Map<String, Value>> {
    fs::read_to_string(extension_dir.join("assets/assets.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

/// Where each selected list can be downloaded, from uBlock Origin's own
/// registry — so lists it adds or moves in an update follow.
pub fn list_urls(assets: &Map<String, Value>, selected: &[String]) -> Vec<(String, String)> {
    let web = |v: &Value| {
        v.as_str()
            .filter(|s| s.starts_with("https://"))
            .map(str::to_owned)
    };
    let mut out = Vec::new();
    for key in selected {
        let Some(asset) = assets.get(key) else {
            continue;
        };
        if asset["content"] != "filters" {
            continue;
        }
        let first_of = |field: &str| match &asset[field] {
            Value::Array(items) => items.iter().find_map(web),
            other => web(other),
        };
        if let Some(url) = first_of("cdnURLs").or_else(|| first_of("contentURL")) {
            out.push((key.clone(), url));
        }
    }
    // Lists imported by address are keyed by it.
    for key in selected {
        if key.starts_with("https://") && !assets.contains_key(key) {
            out.push((key.clone(), key.clone()));
        }
    }
    out
}

/// The lists uBlock Origin enables by default, for when it hasn't saved a
/// selection of its own yet.
pub fn default_lists(assets: &Map<String, Value>) -> Vec<String> {
    assets
        .iter()
        .filter(|(_, a)| a["content"] == "filters" && a["off"] != true)
        .map(|(key, _)| key.clone())
        .collect()
}

/// Everything needed to turn a report from uBlock Origin into rule lists:
/// fetches (or reads cached) lists and compiles them. Blocking.
/// `None` if uBlock Origin's registry can't be read, as while an update
/// replaces its folder: better to keep the rules in force than clear them.
pub fn build(extension_dir: &Path, state: &UblockState) -> Option<Vec<String>> {
    let assets = read_assets(extension_dir)?;
    let selected = if state.selected_filter_lists.is_empty() {
        default_lists(&assets)
    } else {
        state.selected_filter_lists.clone()
    };
    let mut lists: Vec<String> = list_urls(&assets, &selected)
        .iter()
        .filter_map(|(key, url)| fetch_list(key, url))
        .collect();
    if selected.iter().any(|key| key == "user-filters") {
        lists.push(state.user_filters.clone());
    }
    Some(compile(
        &lists,
        &state.net_whitelist,
        &state.dynamic_filtering_string,
        &state.hostname_switches_string,
    ))
}

/// A list's text: from the cache if fresh, else downloaded (blocking; run
/// off the main thread). A failed download falls back to a stale copy.
pub fn fetch_list(key: &str, url: &str) -> Option<String> {
    let path = cache_dir()?.join(format!("{}.txt", safe_name(key)));
    let fresh = fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < REFRESH);
    if fresh && let Ok(text) = fs::read_to_string(&path) {
        return Some(text);
    }
    let downloaded = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .new_agent()
        .get(url)
        .call()
        .ok()
        .and_then(|mut r| {
            r.body_mut()
                .with_config()
                .limit(32 << 20)
                .read_to_string()
                .ok()
        });
    match downloaded {
        Some(text) if text.len() > 16 => {
            let _ = fs::create_dir_all(path.parent()?);
            // Written aside and moved into place, so another build reading
            // the cache meanwhile never sees half a list.
            let aside = path.with_extension(format!("{}.{:?}.tmp", std::process::id(), std::thread::current().id()));
            if fs::write(&aside, &text).is_ok() && fs::rename(&aside, &path).is_err() {
                let _ = fs::remove_file(&aside);
            }
            Some(text)
        }
        _ => fs::read_to_string(&path).ok(),
    }
}

/// Where the fingerprints of the rule lists last enforced are kept, to
/// bring those lists back from WebKit's store at the next launch.
fn enforced_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("enforced.json"))
}

/// Remembers which compiled lists are being enforced; none, to forget.
pub fn save_enforced(fingerprints: &[String]) {
    let Some(path) = enforced_path() else {
        return;
    };
    if fingerprints.is_empty() {
        let _ = fs::remove_file(path);
    } else if let Ok(bytes) = serde_json::to_vec(fingerprints) {
        let _ = crate::state::write_atomic(&path, &bytes);
    }
}

/// The lists enforced when the browser last ran.
pub fn enforced() -> Vec<String> {
    enforced_path()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Caches/Vamprowser/Filters"))
}

fn safe_name(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Converts filter lists to WebKit content rule lists, as JSON. Every list
/// ends with all the exceptions and whitelisted sites, since WebKit only
/// lets a rule override earlier rules in its own list.
pub fn compile(
    lists: &[String],
    whitelist: &[String],
    dynamic: &str,
    switches: &str,
) -> Vec<String> {
    // `$badfilter` switches off the filter it repeats, wherever it is.
    let disabled: HashSet<String> = lists
        .iter()
        .flat_map(|list| list.lines())
        .filter_map(badfilter_target)
        .collect();
    // Rules are kept as the JSON they'll be written as: each is serialised
    // once, deduplicated on that text, and lists are joined from it. Shared,
    // as the set that deduplicates holds them too.
    let mut blocks = Vec::new();
    let mut regex_blocks = Vec::new();
    let mut exceptions = Vec::new();
    let mut seen = HashSet::new();
    for list in lists {
        for line in list.lines() {
            if !disabled.is_empty() && disabled.contains(line.trim()) {
                continue;
            }
            for converted in convert(line) {
                let rule: Rc<str> = converted.rule.to_string().into();
                if !seen.insert(rule.clone()) {
                    continue;
                }
                match (converted.exception, converted.from_regex) {
                    (true, _) => exceptions.push(rule),
                    (false, false) => blocks.push(rule),
                    (false, true) => regex_blocks.push(rule),
                }
            }
        }
    }
    let mut ignored_sites = Vec::new();
    for site in whitelist {
        let site = site.trim().trim_start_matches("*.");
        if is_hostname(site) {
            ignored_sites.push(
                json!({
                    "trigger": { "url-filter": ".*", "if-domain": [format!("*{}", site.to_ascii_lowercase())] },
                    "action": { "type": "ignore-previous-rules" },
                })
                .to_string(),
            );
        }
    }
    let dynamic = dynamic_rules(dynamic, switches);
    let dynamic_allows: Vec<String> = dynamic.allows.iter().map(Value::to_string).collect();
    // Every list ends with the exceptions, dynamic allow rules and sites
    // uBlock Origin is off for, since a rule only overrides earlier rules in
    // its own list.
    let tail: Vec<&str> = exceptions
        .iter()
        .map(|rule| &**rule)
        .chain(dynamic_allows.iter().chain(&ignored_sites).map(String::as_str))
        .collect();
    let mut out: Vec<String> = blocks
        .chunks(CHUNK)
        .chain(regex_blocks.chunks(CHUNK))
        .map(|chunk| json_array(chunk.iter().map(|rule| &**rule).chain(tail.iter().copied())))
        .collect();
    // Dynamic rules in a list of their own, general before specific so the
    // specific wins, as in uBlock Origin; allows and no-ops override them.
    if !dynamic.blocks.is_empty() {
        let blocks: Vec<String> = dynamic.blocks.iter().map(Value::to_string).collect();
        out.push(json_array(
            blocks
                .iter()
                .chain(&dynamic_allows)
                .chain(&ignored_sites)
                .map(String::as_str),
        ));
    }
    out
}

/// A JSON array of rules already serialised, written as serde_json would
/// write the array itself.
fn json_array<'a>(rules: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::from("[");
    for (index, rule) in rules.enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(rule);
    }
    out.push(']');
    out
}

fn is_hostname(site: &str) -> bool {
    !site.is_empty()
        && site.contains('.')
        && site.is_ascii()
        && site
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// The filter a `$badfilter` line switches off: the same line without it.
fn badfilter_target(line: &str) -> Option<String> {
    // Nearly every line of every list: skip it before any splitting.
    if !line.contains("badfilter") {
        return None;
    }
    let line = line.trim();
    let at = line.rfind('$')?;
    let options: Vec<&str> = line[at + 1..].split(',').collect();
    if !options.contains(&"badfilter") {
        return None;
    }
    let kept: Vec<&str> = options.into_iter().filter(|o| *o != "badfilter").collect();
    Some(if kept.is_empty() {
        line[..at].to_owned()
    } else {
        format!("{}${}", &line[..at], kept.join(","))
    })
}

/// Dynamic rules and switches as WebKit rules: blocks, and the allows and
/// no-ops that override them.
struct Dynamic {
    blocks: Vec<Value>,
    allows: Vec<Value>,
}

fn dynamic_rules(rules: &str, switches: &str) -> Dynamic {
    // (specificity, rule), sorted so later (more specific) rules win.
    let mut blocks: Vec<(u8, Value)> = Vec::new();
    let mut allows: Vec<(u8, Value)> = Vec::new();
    for line in rules.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let [source, destination, kind, action] = parts[..] else {
            continue;
        };
        if source == "behind-the-scene" || !matches!(action, "block" | "allow" | "noop") {
            continue;
        }
        let mut trigger = serde_json::Map::new();
        let url_filter = if destination == "*" {
            ".*".to_owned()
        } else if is_hostname(destination) {
            format!("^[^:]+://([^/]+\\.)?{}[:/]", destination.replace('.', "\\."))
        } else {
            continue;
        };
        trigger.insert("url-filter".into(), json!(url_filter));
        if source != "*" {
            if !is_hostname(source) {
                continue;
            }
            trigger.insert("if-domain".into(), json!([format!("*{source}")]));
        }
        match kind {
            "*" => {}
            "image" => {
                trigger.insert("resource-type".into(), json!(["image"]));
            }
            "3p" => {
                trigger.insert("load-type".into(), json!(["third-party"]));
            }
            "1p-script" => {
                trigger.insert("resource-type".into(), json!(["script"]));
                trigger.insert("load-type".into(), json!(["first-party"]));
            }
            "3p-script" => {
                trigger.insert("resource-type".into(), json!(["script"]));
                trigger.insert("load-type".into(), json!(["third-party"]));
            }
            "3p-frame" => {
                trigger.insert("resource-type".into(), json!(["document"]));
                trigger.insert("load-type".into(), json!(["third-party"]));
                trigger.insert("load-context".into(), json!(["child-frame"]));
            }
            // Inline scripts never reach the network.
            _ => continue,
        }
        let specificity = u8::from(source != "*") * 2 + u8::from(destination != "*");
        if action == "block" {
            blocks.push((specificity, json!({ "trigger": trigger, "action": { "type": "block" } })));
        } else {
            allows.push((
                specificity,
                json!({ "trigger": trigger, "action": { "type": "ignore-previous-rules" } }),
            ));
        }
    }
    for line in switches.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let [site, state] = parts[..] else {
            continue;
        };
        let resource = match name.trim() {
            "no-popups" => "popup",
            "no-scripting" => "script",
            "no-remote-fonts" => "font",
            _ => continue,
        };
        let mut trigger = serde_json::Map::new();
        trigger.insert("url-filter".into(), json!(".*"));
        trigger.insert("resource-type".into(), json!([resource]));
        if site != "*" {
            if !is_hostname(site) {
                continue;
            }
            trigger.insert("if-domain".into(), json!([format!("*{site}")]));
        }
        let specificity = u8::from(site != "*") * 2;
        match state {
            "true" => blocks.push((specificity, json!({ "trigger": trigger, "action": { "type": "block" } }))),
            // Switched off for this site against a global switch.
            "false" if site != "*" => allows.push((
                specificity,
                json!({ "trigger": trigger, "action": { "type": "ignore-previous-rules" } }),
            )),
            _ => {}
        }
    }
    blocks.sort_by_key(|(s, _)| *s);
    allows.sort_by_key(|(s, _)| *s);
    Dynamic {
        blocks: blocks.into_iter().map(|(_, r)| r).collect(),
        allows: allows.into_iter().map(|(_, r)| r).collect(),
    }
}

/// A converted filter.
struct Converted {
    exception: bool,
    /// Translated from a regular-expression filter: kept apart, as WebKit
    /// may still reject it.
    from_regex: bool,
    rule: Value,
}

/// Every resource type WebKit knows, for a filter that excludes some.
const ALL_TYPES: [&str; 12] = [
    "document",
    "image",
    "style-sheet",
    "script",
    "font",
    "raw",
    "svg-document",
    "media",
    "popup",
    "ping",
    "websocket",
    "other",
];

/// Top-level domains an entity domain like `google.*` stands for.
const ENTITY_TLDS: [&str; 24] = [
    "com", "net", "org", "de", "fr", "it", "es", "nl", "pl", "ru", "co.uk", "ca", "com.au", "co.jp",
    "jp", "com.br", "in", "se", "ch", "at", "be", "cz", "rs", "com.tr",
];

/// One filter line as WebKit rules (a regular expression can become
/// several), or none if it can't be expressed faithfully.
fn convert(line: &str) -> Vec<Converted> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(['!', '[', '#']) || !line.is_ascii() {
        return Vec::new();
    }
    // Cosmetic filters and scriptlets are uBlock Origin's own business.
    if line.contains('#')
        && ["##", "#@#", "#?#", "#$#", "#%#", "#+js", "#@$#", "#@?#"]
            .iter()
            .any(|marker| line.contains(marker))
    {
        return Vec::new();
    }
    let (exception, rest) = match line.strip_prefix("@@") {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let regex_end = rest.starts_with('/').then(|| regex_end(rest)).flatten();
    let regex = regex_end.is_some();
    let (pattern, options) = match regex_end {
        Some(end) => {
            let options = rest[end + 1..].strip_prefix('$').map_or(Vec::new(), |o| o.split(',').collect());
            (&rest[..=end], options)
        }
        None => split_options(rest),
    };
    let mut trigger = serde_json::Map::new();
    let mut resource_types: Vec<&str> = Vec::new();
    let mut excluded_types: Vec<&str> = Vec::new();
    let mut if_domains = Vec::new();
    let mut unless_domains = Vec::new();
    let mut whole_page = false;
    let mut redirect = false;
    for option in options.iter().copied() {
        let (name, value) = option.split_once('=').unwrap_or((option, ""));
        let (negated, bare) = match name.strip_prefix('~') {
            Some(bare) => (true, bare),
            None => (false, name),
        };
        let kind = match bare {
            "script" => Some("script"),
            "image" => Some("image"),
            "stylesheet" | "css" => Some("style-sheet"),
            "xmlhttprequest" | "xhr" => Some("raw"),
            "font" => Some("font"),
            "media" => Some("media"),
            "websocket" => Some("websocket"),
            "ping" | "beacon" => Some("ping"),
            "other" | "object" => Some("other"),
            _ => None,
        };
        if let Some(kind) = kind {
            if negated {
                excluded_types.push(kind);
            } else {
                resource_types.push(kind);
            }
            continue;
        }
        match name {
            "third-party" | "3p" | "strict3p" => {
                trigger.insert("load-type".into(), json!(["third-party"]));
            }
            "~third-party" | "first-party" | "1p" | "~3p" | "strict1p" => {
                trigger.insert("load-type".into(), json!(["first-party"]));
            }
            "subdocument" | "frame" => {
                resource_types.push("document");
                trigger.insert("load-context".into(), json!(["child-frame"]));
            }
            "popup" => resource_types.push("popup"),
            "doc" | "document" => whole_page = true,
            "domain" | "from" => {
                for domain in value.split('|') {
                    let (negated, domain) = match domain.strip_prefix('~') {
                        Some(d) => (true, d),
                        None => (false, domain),
                    };
                    if domain.is_empty() || domain.contains('/') {
                        return Vec::new();
                    }
                    let domain = domain.to_ascii_lowercase();
                    let expanded: Vec<String> = match domain.strip_suffix(".*") {
                        Some(stem) => ENTITY_TLDS.iter().map(|tld| format!("*{stem}.{tld}")).collect(),
                        None => vec![format!("*{domain}")],
                    };
                    if negated {
                        unless_domains.extend(expanded);
                    } else {
                        if_domains.extend(expanded);
                    }
                }
            }
            "match-case" => {
                trigger.insert("url-filter-is-case-sensitive".into(), json!(true));
            }
            "important" | "all" => {}
            // A redirect blocks, then serves a stand-in; blocking alone is
            // faithful enough except for scripts, which pages may need.
            "redirect" if !exception => redirect = true,
            // Cosmetic-only exceptions: uBlock Origin handles those.
            "ghide" | "generichide" | "ehide" | "elemhide" | "shide" | "specifichide"
                if exception =>
            {
                return Vec::new();
            }
            // Anything else changes what the filter means in ways WebKit
            // can't follow; better unapplied than applied wrongly.
            _ => return Vec::new(),
        }
    }
    if !excluded_types.is_empty() {
        if !resource_types.is_empty() {
            return Vec::new();
        }
        resource_types = ALL_TYPES
            .iter()
            .copied()
            .filter(|t| !excluded_types.contains(t) && *t != "document" && *t != "popup")
            .collect();
    }
    if redirect && (resource_types.is_empty() || resource_types.contains(&"script")) {
        return Vec::new();
    }
    let url_filters: Vec<String> = if regex {
        // Regular-expression exceptions would go in every list, where one
        // WebKit rejects would take everything down.
        if exception {
            return Vec::new();
        }
        translate_regex(&pattern[1..pattern.len() - 1])
    } else {
        to_regex(pattern).into_iter().collect()
    };
    if url_filters.is_empty() {
        return Vec::new();
    }
    // A whole-page exception: nothing on pages it matches is blocked.
    if whole_page && exception {
        return url_filters
            .into_iter()
            .map(|top| Converted {
                exception: true,
                from_regex: regex,
                rule: json!({
                    "trigger": { "url-filter": ".*", "if-top-url": [top] },
                    "action": { "type": "ignore-previous-rules" },
                }),
            })
            .collect();
    }
    if whole_page {
        resource_types.push("document");
        trigger.insert("load-context".into(), json!(["top-frame"]));
    }
    if !resource_types.is_empty() {
        resource_types.sort_unstable();
        resource_types.dedup();
        trigger.insert("resource-type".into(), json!(resource_types));
    }
    // WebKit takes one or the other.
    if !if_domains.is_empty() {
        trigger.insert("if-domain".into(), json!(if_domains));
    } else if !unless_domains.is_empty() {
        trigger.insert("unless-domain".into(), json!(unless_domains));
    }
    let action = if exception {
        "ignore-previous-rules"
    } else {
        "block"
    };
    url_filters
        .into_iter()
        .map(|url_filter| {
            let mut trigger = trigger.clone();
            trigger.insert("url-filter".into(), json!(url_filter));
            Converted {
                exception,
                from_regex: regex,
                rule: json!({ "trigger": trigger, "action": { "type": action } }),
            }
        })
        .collect()
}

/// Where a regular-expression filter's closing `/` is, if the line is one.
fn regex_end(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut escaped = false;
    let mut in_class = false;
    for (i, &b) in bytes.iter().enumerate().skip(1) {
        match b {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => {
                let after = &rest[i + 1..];
                return (i > 1 && (after.is_empty() || after.starts_with('$'))).then_some(i);
            }
            _ => {}
        }
    }
    None
}

/// A filter's regular expression in the subset WebKit's rules accept, as
/// one or more expressions: shorthand classes spelled out, groups of
/// alternatives multiplied out (WebKit has no `|`). `None` of them for
/// anything it can't take: counted repeats, lookarounds, word boundaries.
fn translate_regex(re: &str) -> Vec<String> {
    const MAX_VARIANTS: usize = 12;
    // First, a token stream with shorthand classes replaced.
    let mut spelled = String::new();
    let mut chars = re.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(next) = chars.next() else {
                    return Vec::new();
                };
                let (inside, outside) = match next {
                    'd' => ("0-9", "[0-9]"),
                    'w' => ("A-Za-z0-9_", "[A-Za-z0-9_]"),
                    'D' if !in_class => ("", "[^0-9]"),
                    'W' if !in_class => ("", "[^A-Za-z0-9_]"),
                    '/' => ("/", "/"),
                    'b' | 'B' | 's' | 'S' | 'D' | 'W' | '1'..='9' | 'u' | 'x' | 'p' | 'k' => {
                        return Vec::new();
                    }
                    other => {
                        spelled.push('\\');
                        spelled.push(other);
                        continue;
                    }
                };
                spelled.push_str(if in_class { inside } else { outside });
            }
            '[' => {
                in_class = true;
                spelled.push(c);
            }
            ']' => {
                in_class = false;
                spelled.push(c);
            }
            '{' if !in_class => return Vec::new(),
            _ => spelled.push(c),
        }
    }
    let spelled = spelled.replace("(?:", "(");
    if spelled.contains("(?") {
        return Vec::new();
    }
    let Some(variants) = expand(&spelled, MAX_VARIANTS) else {
        return Vec::new();
    };
    variants
        .into_iter()
        .filter(|v| !v.is_empty() && v.len() > 3 && webkit_accepts(v))
        .collect()
}

/// Multiplies out `a(b|c)d` into `abd`, `acd` (and top-level `a|b`), up to
/// `limit` variants; groups under `*` or `+` can't be, so give up.
fn expand(re: &str, limit: usize) -> Option<Vec<String>> {
    let alternatives = split_top(re);
    let mut out = Vec::new();
    for alternative in alternatives {
        let mut partial = vec![String::new()];
        let bytes: Vec<char> = alternative.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == '\\' && i + 1 < bytes.len() {
                for p in &mut partial {
                    p.push(c);
                    p.push(bytes[i + 1]);
                }
                i += 2;
                continue;
            }
            if c == '[' {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != ']' {
                    if bytes[j] == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
                let class: String = bytes[i..=j.min(bytes.len() - 1)].iter().collect();
                for p in &mut partial {
                    p.push_str(&class);
                }
                i = j + 1;
                continue;
            }
            if c == '(' {
                let mut depth = 0;
                let mut j = i;
                while j < bytes.len() {
                    match bytes[j] {
                        '\\' => j += 1,
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if j >= bytes.len() {
                    return None;
                }
                let inner: String = bytes[i + 1..j].iter().collect();
                let quantifier = bytes.get(j + 1).copied();
                let has_alternatives = split_top(&inner).len() > 1;
                i = j + 1;
                if !has_alternatives {
                    // A plain group stays as it is.
                    let mut group = format!("({inner})");
                    if let Some(q @ ('?' | '*' | '+')) = quantifier {
                        group.push(q);
                        i += 1;
                    }
                    for p in &mut partial {
                        p.push_str(&group);
                    }
                    continue;
                }
                if matches!(quantifier, Some('*' | '+')) {
                    return None;
                }
                let mut choices = expand(&inner, limit)?;
                if quantifier == Some('?') {
                    choices.push(String::new());
                    i += 1;
                }
                let mut next = Vec::new();
                for p in &partial {
                    for choice in &choices {
                        next.push(format!("{p}({choice})"));
                        if next.len() > limit {
                            return None;
                        }
                    }
                }
                partial = next;
                continue;
            }
            for p in &mut partial {
                p.push(c);
            }
            i += 1;
        }
        out.extend(partial);
        if out.len() > limit {
            return None;
        }
    }
    // Empty groups WebKit wouldn't like.
    Some(out.into_iter().map(|v| v.replace("()", "")).collect())
}

/// Splits at `|` outside groups and classes.
fn split_top(re: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut depth = 0;
    let mut in_class = false;
    let mut escaped = false;
    for c in re.chars() {
        if escaped {
            escaped = false;
            parts.last_mut().expect("never empty").push(c);
            continue;
        }
        match c {
            '\\' => escaped = true,
            '[' => in_class = true,
            ']' => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => depth -= 1,
            '|' if !in_class && depth == 0 => {
                parts.push(String::new());
                continue;
            }
            _ => {}
        }
        parts.last_mut().expect("never empty").push(c);
    }
    parts
}

/// A conservative check that WebKit will compile `re`: ASCII, no `|` or
/// braces, balanced groups and classes, `^` only first or in a class, `$`
/// only last.
fn webkit_accepts(re: &str) -> bool {
    if !re.is_ascii() {
        return false;
    }
    let chars: Vec<char> = re.chars().collect();
    let mut depth = 0i32;
    let mut in_class = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                i += 1;
            }
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            '|' | '{' | '}' if !in_class => return false,
            '^' if !in_class && i != 0 => return false,
            '$' if !in_class && i != chars.len() - 1 => return false,
            _ => {}
        }
        i += 1;
    }
    depth == 0 && !in_class
}

/// A filter's pattern and options, split at the last `$` that begins a
/// plausible option list.
fn split_options(rest: &str) -> (&str, Vec<&str>) {
    if let Some(at) = rest.rfind('$') {
        let options = &rest[at + 1..];
        let looks_like_options = !options.is_empty()
            && options
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ",~=|.-_*".contains(c));
        if looks_like_options {
            return (&rest[..at], options.split(',').collect());
        }
    }
    (rest, Vec::new())
}

/// A filter pattern as a WebKit rule regex: `||` a domain and its
/// subdomains, `|` an anchor, `^` a separator, `*` anything.
fn to_regex(pattern: &str) -> Option<String> {
    let mut rest = pattern;
    let mut out = String::new();
    if let Some(r) = rest.strip_prefix("||") {
        out.push_str("^[^:]+:(//)?([^/]+\\.)?");
        rest = r;
    } else if let Some(r) = rest.strip_prefix('|') {
        out.push('^');
        rest = r;
    }
    let anchored_end = rest.ends_with('|') && !rest.ends_with("\\|");
    if anchored_end {
        rest = &rest[..rest.len() - 1];
    }
    // A pattern this short matches nearly everything.
    let meaningful = rest.chars().filter(|c| !matches!(c, '*' | '^')).count();
    if meaningful < 3 {
        return None;
    }
    for c in rest.chars() {
        match c {
            '*' => out.push_str(".*"),
            '^' => out.push_str("[^a-zA-Z0-9_.%-]"),
            '.' | '+' | '?' | '$' | '{' | '}' | '(' | ')' | '[' | ']' | '\\' | '|' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    if anchored_end {
        out.push('$');
    }
    while let Some(trimmed) = out.strip_prefix(".*") {
        out = trimmed.to_owned();
    }
    while out.ends_with(".*") && !out.ends_with("\\.*") {
        out.truncate(out.len() - 2);
    }
    (!out.is_empty()).then_some(out)
}

/// FNV-1a, for naming compiled lists by their content.
pub fn fingerprint(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(line: &str) -> Option<(bool, Value)> {
        convert(line).into_iter().next().map(|c| (c.exception, c.rule))
    }

    #[test]
    fn domain_anchors_become_subdomain_regexes() {
        let (exception, rule) = rule("||doubleclick.net^").unwrap();
        assert!(!exception);
        assert_eq!(
            rule["trigger"]["url-filter"],
            "^[^:]+:(//)?([^/]+\\.)?doubleclick\\.net[^a-zA-Z0-9_.%-]"
        );
        assert_eq!(rule["action"]["type"], "block");
    }

    #[test]
    fn options_map_to_trigger_fields_or_drop_the_filter() {
        let (_, r) = rule("||ads.example.com^$third-party,script,domain=a.com|b.org").unwrap();
        assert_eq!(r["trigger"]["load-type"], json!(["third-party"]));
        assert_eq!(r["trigger"]["resource-type"], json!(["script"]));
        assert_eq!(r["trigger"]["if-domain"], json!(["*a.com", "*b.org"]));
        let (_, r) = rule("/banner/ads*$image,domain=~news.example").unwrap();
        assert_eq!(r["trigger"]["unless-domain"], json!(["*news.example"]));
        assert!(rule("||example.com^$redirect=noopjs").is_none());
        assert!(rule("||example.com^$removeparam=utm_source").is_none());
        // Entity domains stand for the usual top-level domains.
        let (_, r) = rule("||example.com^$domain=google.*").unwrap();
        assert!(r["trigger"]["if-domain"].as_array().unwrap().contains(&json!("*google.de")));
    }

    #[test]
    fn exceptions_cosmetics_regexes_and_comments() {
        let (exception, r) = rule("@@||cdn.example.com^$script").unwrap();
        assert!(exception);
        assert_eq!(r["action"]["type"], "ignore-previous-rules");
        assert!(rule("example.com##.ad-banner").is_none());
        assert!(rule("youtube.com##+js(set-constant, x, undefined)").is_none());
        assert!(rule("/^https?:\\/\\/ads\\.[a-z]{2,3}\\//").is_none());
        assert!(rule("! Title: EasyList").is_none());
        assert!(rule("[Adblock Plus 2.0]").is_none());
        assert!(rule("||ab^").is_none());
    }

    #[test]
    fn plain_patterns_escape_and_anchor() {
        let (_, r) = rule("|https://track.example/p.gif?x=|").unwrap();
        assert_eq!(
            r["trigger"]["url-filter"],
            "^https://track\\.example/p\\.gif\\?x=$"
        );
        let (_, r) = rule("*/ads/*.js").unwrap();
        assert_eq!(r["trigger"]["url-filter"], "/ads/.*\\.js");
    }

    #[test]
    fn compiled_lists_end_with_exceptions_and_whitelist() {
        let lists = vec!["||a.com^\n||b.com^\n@@||ok.a.com^\n||a.com^".to_owned()];
        let chunks = compile(&lists, &["trusted.org".into(), "about-scheme".into()], "", "");
        assert_eq!(chunks.len(), 1);
        let rules: Vec<Value> = serde_json::from_str(&chunks[0]).unwrap();
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[2]["action"]["type"], "ignore-previous-rules");
        assert_eq!(rules[3]["trigger"]["if-domain"], json!(["*trusted.org"]));
        assert!(compile(&["! nothing".into()], &[], "", "").is_empty());
    }

    #[test]
    fn compiled_lists_are_written_as_serde_json_writes_arrays() {
        let lists = vec![
            "||a.com^\n||b.com^$script,third-party\n@@||ok.a.com^\n/ad(s|v)x/$image\n||a.com^".to_owned(),
        ];
        let chunks = compile(
            &lists,
            &["trusted.org".into()],
            "* tracker.example * block\nnews.example * 3p-script noop",
            "no-popups: * true",
        );
        assert_eq!(chunks.len(), 3);
        for chunk in &chunks {
            let parsed: Value = serde_json::from_str(chunk).unwrap();
            assert_eq!(&parsed.to_string(), chunk);
        }
    }

    #[test]
    fn badfilter_switches_off_what_it_names() {
        let lists = vec!["||a.com^$script\n||b.com^\n||a.com^$script,badfilter".to_owned()];
        let chunks = compile(&lists, &[], "", "");
        let rules: Vec<Value> = serde_json::from_str(&chunks[0]).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0]["trigger"]["url-filter"].as_str().unwrap().contains("b\\.com"));
    }

    #[test]
    fn negated_types_redirects_and_whole_pages() {
        let (_, r) = rule("||a.com^$~script,~image").unwrap();
        let types = r["trigger"]["resource-type"].as_array().unwrap();
        assert!(!types.contains(&json!("script")) && types.contains(&json!("raw")));
        // Image stand-ins become plain blocks; script ones stay out.
        assert!(rule("||a.com/pixel.gif$image,redirect=1x1.gif").is_some());
        assert!(rule("||a.com/ads.js$script,redirect=noopjs").is_none());
        assert!(rule("||a.com^$redirect-rule=noopjs").is_none());
        let (exception, r) = rule("@@||bank.example^$document").unwrap();
        assert!(exception);
        assert!(r["trigger"]["if-top-url"].is_array());
        let (_, r) = rule("||malware.example^$doc").unwrap();
        assert_eq!(r["trigger"]["load-context"], json!(["top-frame"]));
        assert!(rule("@@||site.example^$ghide").is_none());
    }

    #[test]
    fn regexes_are_spelled_out_for_webkit() {
        let found = convert(r"/^https?:\/\/(ads|track)\.example\.com\/\d+/$script");
        let filters: Vec<&str> = found
            .iter()
            .map(|c| c.rule["trigger"]["url-filter"].as_str().unwrap())
            .collect();
        assert_eq!(
            filters,
            [
                "^https?://(ads)\\.example\\.com/[0-9]+",
                "^https?://(track)\\.example\\.com/[0-9]+"
            ]
        );
        assert!(found.iter().all(|c| c.from_regex));
        assert!(convert(r"/ad(?=s)/").is_empty());
        assert!(convert(r"/\bads\b/").is_empty());
        assert!(convert(r"/(a|b)+banner/").is_empty());
    }

    #[test]
    fn dynamic_rules_and_switches() {
        let dynamic = dynamic_rules(
            "* * 3p-script block\nnews.example * 3p-script noop\n* tracker.example * block\nbehind-the-scene * * noop",
            "no-popups: * true\nno-popups: allowed.example false\nno-remote-fonts: fonts.example true\nno-large-media: * true",
        );
        assert_eq!(dynamic.blocks.len(), 4);
        // General before specific.
        assert!(dynamic.blocks[0]["trigger"]["if-domain"].is_null());
        assert_eq!(dynamic.blocks[3]["trigger"]["if-domain"], json!(["*fonts.example"]));
        assert_eq!(dynamic.allows.len(), 2);
        let chunks = compile(&["||a.com^".into()], &[], "* tracker.example * block", "");
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn list_urls_follow_the_extensions_registry() {
        let dir = std::env::temp_dir().join(format!("vamp-assets-{}", std::process::id()));
        fs::create_dir_all(dir.join("assets")).unwrap();
        fs::write(
            dir.join("assets/assets.json"),
            r#"{"easylist":{"content":"filters","contentURL":["https://a/easylist.txt","assets/x.txt"],"cdnURLs":["https://cdn/easylist.txt"]},
               "plowe-0":{"content":"filters","contentURL":"https://p/list.txt"},
               "assets.json":{"content":"internal","contentURL":"https://x"}}"#,
        )
        .unwrap();
        let urls = list_urls(
            &read_assets(&dir).unwrap(),
            &[
                "easylist".into(),
                "plowe-0".into(),
                "assets.json".into(),
                "missing".into(),
            ],
        );
        assert_eq!(
            urls,
            [
                ("easylist".to_owned(), "https://cdn/easylist.txt".to_owned()),
                ("plowe-0".to_owned(), "https://p/list.txt".to_owned())
            ]
        );
        assert_eq!(default_lists(&read_assets(&dir).unwrap()), ["easylist", "plowe-0"]);
        assert!(read_assets(&dir.join("missing")).is_none());
        let _ = fs::remove_dir_all(dir);
    }
}
