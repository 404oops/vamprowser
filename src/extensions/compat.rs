//! Smooths over the ways Firefox add-ons expect more than WebKit gives,
//! by adjusting the unpacked extension on disk before WebKit reads it:
//!
//! - WebKit decodes extension scripts and styles with no declared charset
//!   as Latin-1, where Firefox and Chrome assume UTF-8. A bundle with any
//!   non-ASCII character then fails to parse (Proton Pass's background
//!   script does, and the extension never starts). A UTF-8 byte-order mark
//!   settles the encoding without changing a character of the code.
//! - [`SCRIPT`] runs before the extension's own code in every background,
//!   content-script and page context; see its comments.
//!
//! Every step is idempotent, and [`VERSION`] reapplies them to extensions
//! installed before a change here.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::Value;

/// Bumped whenever [`apply`] learns something new.
const VERSION: u32 = 19;
const MARKER: &str = ".vamprowser-compat";
const SCRIPT_FILE: &str = "vamprowser-compat.js";
const WORKER_FILE: &str = "vamprowser-worker.js";
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Runs first in each of the extension's JavaScript contexts.
const SCRIPT: &str = r#"/* Vamprowser: bridges Firefox add-on expectations and WebKit's WebExtensions. */
(() => {
  if (globalThis.__vamprowserCompat) return;
  globalThis.__vamprowserCompat = true;

  // Firefox gives content scripts `window.wrappedJSObject`, the page's own
  // view of the window; WebKit keeps content scripts in a separate world
  // with no such bridge. Pointing it at the content script's window keeps
  // add-ons that check it (uBlock Origin's scriptlets) from throwing.
  if (typeof window !== 'undefined' && typeof location !== 'undefined'
      && location.protocol !== 'webkit-extension:' && !('wrappedJSObject' in window)) {
    try {
      Object.defineProperty(window, 'wrappedJSObject', { value: window, configurable: true });
    } catch (_) {}
  }

  // Everything below adjusts WebKit's own namespace objects in place: they
  // are stable objects, and WebKit finds listeners through them, so
  // replacing `browser` or `chrome` with a stand-in silences every event.
  // WebKit's namespace objects are wrappers it discards once nothing in
  // JavaScript holds them, making fresh ones on the next read — without
  // anything set on them here. Holding every one keeps them, and the fixes,
  // for the life of the page.
  const held = new Set();
  const hold = (object) => {
    if (object && typeof object === 'object' && !held.has(object)) {
      held.add(object);
      const proto = Object.getPrototypeOf(object);
      if (proto && proto !== Object.prototype) held.add(proto);
    }
    return object;
  };
  Object.defineProperty(globalThis, '__vamprowserHeld', { value: held });

  const define = (object, key, value) => {
    try {
      Object.defineProperty(object, key, { value, configurable: true, writable: true });
    } catch (_) {}
  };

  // Firefox's API functions work called bare; add-ons keep `const getURL =
  // browser.runtime.getURL` and call it later (uBlock Origin does). WebKit's
  // need their namespace as `this`, so give each namespace bound copies.
  const bindAll = (namespace, depth) => {
    if (!namespace || typeof namespace !== 'object' || depth > 2) return;
    hold(namespace);
    let keys = [];
    try {
      keys = Object.getOwnPropertyNames(Object.getPrototypeOf(namespace) || {})
        .concat(Object.getOwnPropertyNames(namespace));
    } catch (_) { return; }
    for (const key of new Set(keys)) {
      if (key === 'constructor' || /^on[A-Z]/.test(key)) continue;
      let value;
      try { value = namespace[key]; } catch (_) { continue; }
      if (typeof value === 'function') define(namespace, key, value.bind(namespace));
      else if (value && typeof value === 'object' && !Array.isArray(value)) bindAll(value, depth + 1);
    }
  };

  // Events Firefox has and WebKit doesn't (runtime.onUpdateAvailable, …):
  // add-ons subscribe to them unguarded at startup, and one TypeError there
  // stops the whole background script. A silent event lets them carry on.
  const silentEvent = () => ({
    addListener() {},
    removeListener() {},
    hasListener: () => false,
    hasListeners: () => false,
  });
  const FIREFOX_EVENTS = {
    runtime: ['onUpdateAvailable', 'onSuspend', 'onSuspendCanceled', 'onRestartRequired', 'onBrowserUpdateAvailable'],
    tabs: ['onHighlighted', 'onZoomChange', 'onAttached', 'onDetached', 'onMoved', 'onReplaced'],
    windows: ['onBoundsChanged'],
    webNavigation: ['onCreatedNavigationTarget', 'onHistoryStateUpdated', 'onReferenceFragmentUpdated', 'onTabReplaced'],
  };

  let done = null;
  for (const name of ['browser', 'chrome']) {
    const api = globalThis[name];
    if (!api || typeof api !== 'object') continue;
    // Some add-ons replace `browser` and `chrome` with locked-down stand-ins
    // once they've taken their own references, so injected code can't use
    // them (Proton Pass does). Firefox doesn't mind; WebKit finds listeners
    // through these globals, so the add-on's background would never hear
    // another message. Keep the real API here and let replacements pass.
    try {
      Object.defineProperty(globalThis, name, {
        configurable: false,
        enumerable: true,
        get: () => api,
        set: () => {},
      });
    } catch (_) {}
    // Where `chrome` is the very object `browser` is, it's fixed already.
    if (api === done) continue;
    done = api;
    hold(api);
    for (const key of Object.getOwnPropertyNames(api)) {
      try { hold(api[key]); } catch (_) {}
    }
    // WebKit recreates some namespace objects after startup (the menus one,
    // at least), dropping anything set on them. Their methods live on a
    // prototype that persists, so the fixes below go there; `this` is the
    // namespace a method is called on, or the one seen here if called bare.
    const owner = (namespace, key) => {
      for (let o = namespace; o && o !== Object.prototype; o = Object.getPrototypeOf(o)) {
        if (Object.prototype.hasOwnProperty.call(o, key)) return o;
      }
      const proto = Object.getPrototypeOf(namespace);
      return proto && proto !== Object.prototype ? proto : namespace;
    };
    const patch = (namespace, key, wrap) => {
      if (!namespace || typeof namespace !== 'object') return;
      hold(namespace);
      const home = owner(namespace, key);
      const original = home[key];
      if (original && original.__vamprowser) return;
      const replacement = function (...args) {
        const self = this && typeof this === 'object' ? this : namespace;
        return wrap(original, self, args);
      };
      replacement.__vamprowser = true;
      define(home, key, replacement);
    };

    // Where WebKit won't let a method be redefined at all (i18n.getMessage),
    // the add-on gets a stand-in namespace: the real one's functions bound
    // to it, its other members as they are, and the fix on top. Only for
    // namespaces WebKit doesn't deliver events through, so listeners on the
    // real one keep working.
    const standIn = (name, key, wrap) => {
      const namespace = api[name];
      if (!namespace || (namespace[key] && namespace[key].__vamprowser)) return;
      patch(namespace, key, wrap);
      if (namespace[key] && namespace[key].__vamprowser) return;
      const copy = {};
      const keys = new Set(Object.getOwnPropertyNames(Object.getPrototypeOf(namespace) || {})
        .concat(Object.getOwnPropertyNames(namespace)));
      for (const member of keys) {
        if (member === 'constructor') continue;
        let value;
        try { value = namespace[member]; } catch (_) { continue; }
        copy[member] = typeof value === 'function' ? value.bind(namespace) : value;
      }
      const original = namespace[key];
      copy[key] = function (...args) { return wrap(original, namespace, args); };
      copy[key].__vamprowser = true;
      define(api, name, copy);
    };

    // Firefox takes runtime.sendMessage(ownId, message) and connect(ownId, …)
    // as messages within the add-on. WebKit takes any id as another
    // extension's, so the add-on's own background never hears them (Proton
    // Pass's popup then reports it can't start). Drop an id that is our own.
    for (const call of ['sendMessage', 'connect']) {
      patch(api.runtime, call, (original, runtime, args) => {
        if (typeof args[0] === 'string' && args[0] === runtime.id && (args.length > 1 || call === 'connect')) {
          args.shift();
        }
        return original.apply(runtime, args);
      });
    }

    // Firefox accepts context-menu URL patterns for any scheme (uBlock
    // Origin's "abp:*" subscribe links); WebKit throws on them, which can
    // stop a background script mid-start. Keep the patterns WebKit takes,
    // and skip an item that has none left rather than show it everywhere.
    const webPattern = (pattern) => pattern === '<all_urls>' || /^(\*|https?|wss?|file|ftp):\/\//.test(pattern);
    for (const ns of ['menus', 'contextMenus']) {
      patch(api[ns], 'create', (original, menus, [properties, callback]) => {
        const props = Object.assign({}, properties);
        for (const key of ['targetUrlPatterns', 'documentUrlPatterns']) {
          if (!Array.isArray(props[key])) continue;
          const kept = props[key].filter(webPattern);
          if (kept.length === 0) {
            if (typeof callback === 'function') setTimeout(callback, 0);
            return props.id;
          }
          props[key] = kept;
        }
        try {
          return original.call(menus, props, callback);
        } catch (error) {
          console.warn('Vamprowser: menu item skipped:', error && error.message);
          return props.id;
        }
      });
    }

    // Functions Firefox has and WebKit doesn't, which add-ons call unguarded
    // during startup (uBlock Origin stops at handlerBehaviorChanged).
    if (api.webRequest && typeof api.webRequest.handlerBehaviorChanged !== 'function') {
      patch(api.webRequest, 'handlerBehaviorChanged', (_original, _webRequest, [callback]) => {
        if (typeof callback === 'function') setTimeout(callback, 0);
        return Promise.resolve();
      });
    }

    // Firefox answers getMessage('') or an unknown name with ''; WebKit throws.
    standIn('i18n', 'getMessage', (original, i18n, [name, substitutions]) => {
      if (!name) return '';
      try {
        return original.call(i18n, name, substitutions) ?? '';
      } catch (_) {
        return '';
      }
    });

    // In a popup, Firefox's tabs.getCurrent() gives nothing; WebKit gives the
    // active tab, and add-ons then lay the popup out as a full tab (Proton
    // Pass opens a sliver tall). A page's own tab shows the page's address.
    if (typeof location !== 'undefined') {
      patch(api.tabs, 'getCurrent', (original, tabs, [callback]) => {
        const tab = Promise.resolve(original.call(tabs))
          .then((tab) => (tab && tab.url === location.href ? tab : undefined), () => undefined);
        if (typeof callback === 'function') {
          tab.then(callback);
          return undefined;
        }
        return tab;
      });
    }

    // Firefox won't run an add-on's content scripts in its own pages; WebKit
    // will, and uBlock Origin's, injected into its dashboard, take over the
    // dashboard's own messaging so its buttons stop working. Leave the
    // add-on's pages alone, as Firefox does.
    const ownPage = (tabs, tabId) => (typeof tabId !== 'number'
      ? Promise.resolve(false)
      : Promise.resolve(tabs.get(tabId)).then(
        (tab) => !!(tab && typeof tab.url === 'string' && tab.url.startsWith('webkit-extension:')),
        () => false));
    for (const call of ['executeScript', 'insertCSS', 'removeCSS']) {
      patch(api.tabs, call, (original, tabs, args) => {
        const callback = typeof args[args.length - 1] === 'function' ? args.pop() : undefined;
        const tabId = typeof args[0] === 'number' ? args[0] : undefined;
        const run = ownPage(tabs, tabId).then((own) => (own ? [] : original.apply(tabs, args)));
        if (callback) {
          run.then(callback, () => callback(undefined));
          return undefined;
        }
        return run;
      });
    }

    for (const [ns, events] of Object.entries(FIREFOX_EVENTS)) {
      const namespace = api[ns];
      if (!namespace) continue;
      for (const event of events) {
        if (!(event in namespace)) define(owner(namespace, event), event, silentEvent());
      }
    }

    // Last, so the bound copies wrap the fixed methods.
    for (const key of Object.getOwnPropertyNames(api)) {
      let namespace;
      try { namespace = api[key]; } catch (_) { continue; }
      bindAll(namespace, 0);
    }
  }
})();
"#;

/// Whether the extension in `dir` has had this version's adjustments.
pub fn is_current(dir: &Path) -> bool {
    fs::read_to_string(dir.join(MARKER))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        == Some(VERSION)
}

/// Adjusts the unpacked extension in `dir`, if it hasn't been already.
pub fn apply(dir: &Path) -> Result<(), String> {
    let marker = dir.join(MARKER);
    let current = fs::read_to_string(&marker)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    if current == Some(VERSION) {
        return Ok(());
    }
    // WebKit has no idle callbacks, and some of the extension's scripts
    // schedule work with them.
    let mut script = BOM.to_vec();
    script.extend_from_slice(IDLE_POLYFILL.as_bytes());
    script.extend_from_slice(SCRIPT.as_bytes());
    fs::write(dir.join(SCRIPT_FILE), script).map_err(|err| err.to_string())?;
    for file in files(dir).map_err(|err| err.to_string())? {
        let extension = file
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let ours = file.file_name().and_then(|n| n.to_str()) == Some(SCRIPT_FILE);
        match extension.as_str() {
            "js" | "mjs" if !ours => prepare_source(&file, true),
            "css" => prepare_source(&file, false),
            "html" | "htm" => inject_into_page(&file),
            _ => Ok(()),
        }
        .map_err(|err| err.to_string())?;
    }
    patch_manifest(dir)?;
    bridge_ublock(dir)?;
    fs::write(marker, VERSION.to_string()).map_err(|err| err.to_string())
}

const UBLOCK_BRIDGE_FILE: &str = "vamprowser-ublock.js";

/// Runs in uBlock Origin's background page and tells the browser which
/// filter lists it has selected and which sites it's switched off for, so
/// the browser can enforce its network filters (see `crate::filters`).
/// Reports on start, whenever those settings change, and every few hours.
const UBLOCK_BRIDGE: &str = r#"/* Vamprowser: reports uBlock Origin's filter settings to the browser. */
(() => {
  const api = globalThis.browser;
  if (!api || !api.runtime || typeof api.runtime.sendNativeMessage !== 'function') return;
  const keys = ['selectedFilterLists', 'netWhitelist', 'user-filters', 'dynamicFilteringString', 'hostnameSwitchesString'];
  const text = (value) => typeof value === 'string' ? value : '';
  let last = '';
  const report = (always) => api.storage.local.get(keys)
    .then((stored) => {
      const whitelist = stored.netWhitelist;
      const message = JSON.stringify({
        selectedFilterLists: Array.isArray(stored.selectedFilterLists) ? stored.selectedFilterLists : [],
        netWhitelist: typeof whitelist === 'string' ? whitelist.split(String.fromCharCode(10)) : (Array.isArray(whitelist) ? whitelist : []),
        userFilters: text(stored['user-filters']),
        dynamicFilteringString: text(stored.dynamicFilteringString),
        hostnameSwitchesString: text(stored.hostnameSwitchesString),
      });
      if (!always && message === last) return;
      last = message;
      api.runtime.sendNativeMessage('dev.oops404.vamprowser.ublock', message);
    })
    .catch(() => {});
  setTimeout(() => report(true), 3000);

  // uBlock Origin saves its settings from this same page, and WebKit
  // doesn't tell a page about its own storage changes, so watch its writes
  // and report once a burst of them has landed and paused. This runs before
  // uBlock Origin's scripts, so they pick up the watched methods. The
  // compatibility script gives each storage area bound copies of its
  // methods, and WebKit keeps the originals on a shared prototype, so both
  // are wrapped, in `browser` and `chrome` alike.
  let pending = 0;
  const soon = () => {
    clearTimeout(pending);
    pending = setTimeout(() => report(false), 500);
  };
  const touches = (method, [items]) => {
    if (method === 'clear' || items == null) return true;
    const names = typeof items === 'string' ? [items] : (Array.isArray(items) ? items : Object.keys(items));
    return names.some((name) => keys.includes(name));
  };
  const watch = (area) => {
    if (!area || typeof area !== 'object') return;
    for (const home of [area, Object.getPrototypeOf(area)]) {
      if (!home || home === Object.prototype) continue;
      for (const method of ['set', 'remove', 'clear']) {
        if (!Object.prototype.hasOwnProperty.call(home, method)) continue;
        const original = home[method];
        if (typeof original !== 'function' || original.__vamprowserWatched) continue;
        const watched = function (...args) {
          const result = original.apply(this && typeof this === 'object' ? this : area, args);
          if (touches(method, args)) Promise.resolve(result).then(soon, soon);
          return result;
        };
        watched.__vamprowserWatched = true;
        try {
          Object.defineProperty(home, method, { value: watched, configurable: true, writable: true });
        } catch (_) {}
      }
    }
  };
  for (const name of ['browser', 'chrome']) {
    try { watch(globalThis[name].storage.local); } catch (_) {}
  }
  // Seldom, in case a write slipped past the watch.
  setInterval(() => report(false), 60 * 1000);
  // Now and then regardless, so lists refresh on schedule.
  setInterval(() => report(true), 6 * 60 * 60 * 1000);
})();
"#;

/// Gives uBlock Origin its bridge script and the permission it sends with.
fn bridge_ublock(dir: &Path) -> Result<(), String> {
    let path = dir.join("manifest.json");
    let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    let Ok(mut manifest) = serde_json::from_str::<Value>(&text) else {
        return Ok(());
    };
    let id = manifest
        .pointer("/browser_specific_settings/gecko/id")
        .or_else(|| manifest.pointer("/applications/gecko/id"))
        .and_then(Value::as_str);
    if id != Some(crate::filters::UBLOCK_ID) {
        return Ok(());
    }
    fs::write(dir.join(UBLOCK_BRIDGE_FILE), UBLOCK_BRIDGE).map_err(|err| err.to_string())?;
    let permission = Value::String("nativeMessaging".into());
    if let Some(permissions) = manifest
        .get_mut("permissions")
        .and_then(Value::as_array_mut)
        && !permissions.contains(&permission)
    {
        permissions.push(permission);
    }
    let page = manifest
        .pointer("/background/page")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match page {
        Some(page) => {
            let file = dir.join(page.trim_start_matches('/'));
            let html = fs::read_to_string(&file).map_err(|err| err.to_string())?;
            if !html.contains(UBLOCK_BRIDGE_FILE) {
                let ours = format!("<script src=\"/{SCRIPT_FILE}\" charset=\"utf-8\"></script>");
                let tag = format!("{ours}<script src=\"/{UBLOCK_BRIDGE_FILE}\"></script>");
                let html = if html.contains(&ours) {
                    html.replacen(&ours, &tag, 1)
                } else {
                    format!("{tag}{html}")
                };
                fs::write(&file, html).map_err(|err| err.to_string())?;
            }
        }
        None => {
            // Right after the compatibility script, before uBlock Origin's
            // own, so they see the watched storage methods.
            if let Some(scripts) = manifest
                .pointer_mut("/background/scripts")
                .and_then(Value::as_array_mut)
            {
                let ours = Value::String(UBLOCK_BRIDGE_FILE.into());
                scripts.retain(|script| *script != ours);
                let at = scripts
                    .iter()
                    .position(|script| script == SCRIPT_FILE)
                    .map_or(0, |at| at + 1);
                scripts.insert(at, ours);
            }
        }
    }
    let text = serde_json::to_string_pretty(&manifest).map_err(|err| err.to_string())?;
    fs::write(&path, text).map_err(|err| err.to_string())
}

/// Every file under `dir`, recursively, except our marker.
fn files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(next)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// A guarded `requestIdleCallback`, first in [`SCRIPT_FILE`] and in scripts
/// that use idle callbacks: those include web-accessible scripts an add-on
/// injects into pages (Proton Pass's autofill dropdown), where [`SCRIPT`]
/// never loads.
const PREVIOUS_IDLE_POLYFILL: &str = "/*vamprowser-idle*/if(typeof requestIdleCallback!=='function'){\
globalThis.requestIdleCallback=function(c){var s=Date.now();return setTimeout(function(){\
c({didTimeout:false,timeRemaining:function(){return Math.max(0,50-(Date.now()-s))}})},1)};\
globalThis.cancelIdleCallback=function(i){clearTimeout(i)}}\n";

// WebKit has no native idle callback in some extension contexts. Yield a
// frame between batches and keep each batch below a typical frame. The deadline
// starts when the callback runs, so time spent queued cannot starve an
// extension into an endless stream of zero-budget callbacks.
const IDLE_POLYFILL: &str = "/*vamprowser-idle*/if(typeof requestIdleCallback!=='function'){\
globalThis.requestIdleCallback=function(c,o){var q=Date.now(),t=o&&typeof o.timeout==='number'\
&&isFinite(o.timeout)?Math.max(0,o.timeout):Infinity;return setTimeout(function(){\
var s=Date.now(),d=s-q>=t;c({didTimeout:d,timeRemaining:function(){\
return d?0:Math.max(0,8-(Date.now()-s))}})},Math.min(16,t))};\
globalThis.cancelIdleCallback=function(i){clearTimeout(i)}}\n";

/// Prepends a UTF-8 byte-order mark to a non-ASCII script or style sheet
/// without one and, for a `script` that uses idle callbacks,
/// [`IDLE_POLYFILL`]. Writes only if either was missing.
fn prepare_source(file: &Path, script: bool) -> io::Result<()> {
    let data = fs::read(file)?;
    let marked = data.starts_with(BOM);
    let body = data.strip_prefix(BOM).unwrap_or(&data);
    let previous = if script {
        body.strip_prefix(PREVIOUS_IDLE_POLYFILL.as_bytes())
    } else {
        None
    };
    let body = previous.unwrap_or(body);
    let polyfill = script
        && !body.starts_with(IDLE_POLYFILL.as_bytes())
        && (previous.is_some()
            || body
                .windows(b"requestIdleCallback".len())
                .any(|w| w == b"requestIdleCallback"));
    // Plain ASCII decodes the same either way; it gets no mark.
    let mark = !marked && !body.is_ascii();
    if !polyfill && !mark {
        return Ok(());
    }
    let mut out = Vec::with_capacity(data.len() + IDLE_POLYFILL.len() + BOM.len());
    if marked || mark {
        out.extend_from_slice(BOM);
    }
    if polyfill {
        out.extend_from_slice(IDLE_POLYFILL.as_bytes());
    }
    out.extend_from_slice(body);
    fs::write(file, out)
}

/// Loads the compatibility script first in an extension page (popups,
/// options, background pages).
fn inject_into_page(file: &Path) -> io::Result<()> {
    let html = fs::read_to_string(file)?;
    if html.contains(SCRIPT_FILE) {
        return Ok(());
    }
    let tag = format!("<script src=\"/{SCRIPT_FILE}\" charset=\"utf-8\"></script>");
    let lower = html.to_ascii_lowercase();
    let at = lower
        .find("<head")
        .and_then(|start| lower[start..].find('>').map(|end| start + end + 1))
        .or_else(|| {
            lower
                .find("<html")
                .and_then(|start| lower[start..].find('>').map(|end| start + end + 1))
        })
        .unwrap_or(0);
    let mut out = String::with_capacity(html.len() + tag.len());
    out.push_str(&html[..at]);
    out.push_str(&tag);
    out.push_str(&html[at..]);
    fs::write(file, out)
}

/// Puts the compatibility script first in background scripts and in every
/// content script that has extension APIs (not those in the page's world).
fn patch_manifest(dir: &Path) -> Result<(), String> {
    let path = dir.join("manifest.json");
    let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    // A manifest WebKit can read but serde can't (comments, say) is left as
    // it is; the byte-order marks still help.
    let Ok(mut manifest) = serde_json::from_str::<Value>(&text) else {
        return Ok(());
    };
    let ours = Value::String(SCRIPT_FILE.into());
    let prepend = |list: &mut Value| {
        if let Some(items) = list.as_array_mut()
            && !items.contains(&ours)
        {
            items.insert(0, ours.clone());
        }
    };
    if let Some(background) = manifest.get_mut("background") {
        if let Some(scripts) = background.get_mut("scripts") {
            prepend(scripts);
        }
        let worker = background
            .get("service_worker")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(worker) = worker.filter(|w| w != WORKER_FILE) {
            let module = background.get("type").and_then(Value::as_str) == Some("module");
            let worker = worker.trim_start_matches('/');
            let loader = if module {
                format!("import '/{SCRIPT_FILE}';\nimport '/{worker}';\n")
            } else {
                format!("importScripts('/{SCRIPT_FILE}', '/{worker}');\n")
            };
            fs::write(dir.join(WORKER_FILE), loader).map_err(|err| err.to_string())?;
            background["service_worker"] = Value::String(WORKER_FILE.into());
        }
    }
    if let Some(scripts) = manifest
        .get_mut("content_scripts")
        .and_then(Value::as_array_mut)
    {
        for entry in scripts {
            let in_page_world = entry.get("world").and_then(Value::as_str) == Some("MAIN");
            if in_page_world {
                continue;
            }
            if let Some(js) = entry.get_mut("js") {
                prepend(js);
            }
        }
    }
    fill_empty_commands(&mut manifest);
    let text = serde_json::to_string_pretty(&manifest).map_err(|err| err.to_string())?;
    fs::write(&path, text).map_err(|err| err.to_string())
}

/// Firefox accepts a bare `{}` for a command, as uBlock Origin declares
/// `_execute_browser_action`; WebKit calls that invalid and drops it with an
/// error on the extensions page. A description makes it whole again.
fn fill_empty_commands(manifest: &mut Value) {
    let title = ["action", "browser_action"]
        .iter()
        .find_map(|key| manifest.get(key)?.get("default_title")?.as_str())
        .unwrap_or("Open the extension")
        .to_owned();
    let Some(commands) = manifest.get_mut("commands").and_then(Value::as_object_mut) else {
        return;
    };
    for command in commands.values_mut() {
        if command.as_object().is_none_or(|fields| fields.is_empty()) {
            *command = serde_json::json!({ "description": title });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_idle_callback_budget_without_changing_extension_code() {
        let file = std::env::temp_dir().join(format!("vamp-idle-upgrade-{}.js", std::process::id()));
        let source = "requestIdleCallback(run); // ©";
        fs::write(&file, [BOM, PREVIOUS_IDLE_POLYFILL.as_bytes(), source.as_bytes()].concat()).unwrap();
        prepare_source(&file, true).unwrap();
        let once = fs::read(&file).unwrap();
        assert_eq!(once, [BOM, IDLE_POLYFILL.as_bytes(), source.as_bytes()].concat());
        prepare_source(&file, true).unwrap();
        assert_eq!(fs::read(&file).unwrap(), once);
        let _ = fs::remove_file(file);
    }

    fn temp_extension(manifest: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vamp-compat-{}-{}",
            std::process::id(),
            manifest.len()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("ui")).unwrap();
        fs::write(dir.join("manifest.json"), manifest).unwrap();
        dir
    }

    #[test]
    fn marks_non_ascii_sources_and_injects_everywhere_once() {
        let dir = temp_extension(
            r#"{"manifest_version":3,"name":"t","version":"1",
               "background":{"scripts":["bg.js"]},
               "content_scripts":[{"matches":["<all_urls>"],"js":["cs.js"]},
                                  {"matches":["<all_urls>"],"js":["main.js"],"world":"MAIN"}]}"#,
        );
        fs::write(dir.join("bg.js"), "const s = '© 2025';").unwrap();
        fs::write(dir.join("cs.js"), "plain();").unwrap();
        fs::write(dir.join("idle.js"), "requestIdleCallback(go);").unwrap();
        fs::write(
            dir.join("ui/popup.html"),
            "<html><head><title>x</title></head></html>",
        )
        .unwrap();
        apply(&dir).unwrap();
        apply(&dir).unwrap();
        assert!(fs::read(dir.join("bg.js")).unwrap().starts_with(BOM));
        assert!(!fs::read(dir.join("cs.js")).unwrap().starts_with(BOM));
        let idle = fs::read_to_string(dir.join("idle.js")).unwrap();
        assert_eq!(idle.matches("/*vamprowser-idle*/").count(), 1);
        assert!(idle.ends_with("requestIdleCallback(go);"));
        let ours = fs::read_to_string(dir.join(SCRIPT_FILE)).unwrap();
        assert_eq!(ours.matches("/*vamprowser-idle*/").count(), 1);
        let page = fs::read_to_string(dir.join("ui/popup.html")).unwrap();
        assert_eq!(page.matches(SCRIPT_FILE).count(), 1);
        assert!(page.starts_with("<html><head><script src=\"/vamprowser-compat.js\""));
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["background"]["scripts"][0], SCRIPT_FILE);
        assert_eq!(
            manifest["background"]["scripts"].as_array().unwrap().len(),
            2
        );
        assert_eq!(manifest["content_scripts"][0]["js"][0], SCRIPT_FILE);
        assert_eq!(manifest["content_scripts"][1]["js"][0], "main.js");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_commands_get_a_description() {
        let dir = temp_extension(
            r#"{"manifest_version":2,"name":"t","version":"1",
               "browser_action":{"default_title":"uBlock Origin"},
               "commands":{"_execute_browser_action":{},"zap":{"description":"Zap"}}}"#,
        );
        apply(&dir).unwrap();
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(
            manifest["commands"]["_execute_browser_action"]["description"],
            "uBlock Origin"
        );
        assert_eq!(manifest["commands"]["zap"]["description"], "Zap");
    }

    #[test]
    fn service_workers_load_through_a_wrapper() {
        let dir = temp_extension(
            r#"{"manifest_version":3,"name":"t","version":"1","background":{"service_worker":"sw.js"}}"#,
        );
        fs::write(dir.join("sw.js"), "self;").unwrap();
        apply(&dir).unwrap();
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["background"]["service_worker"], WORKER_FILE);
        let loader = fs::read_to_string(dir.join(WORKER_FILE)).unwrap();
        assert!(loader.contains("importScripts('/vamprowser-compat.js', '/sw.js')"));
        let _ = fs::remove_dir_all(dir);
    }
}
