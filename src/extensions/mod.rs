//! Browser extensions: Firefox/Chrome WebExtensions, run by WebKit's own
//! extension support (`WKWebExtension…`, macOS 15.4+).
//!
//! Extensions live unpacked in
//! `~/Library/Application Support/Vamprowser/Extensions/<id>/`, with which
//! are enabled — and each one's stable origin — in `extensions.json` beside
//! them. Every tab's web view is made from
//! [`Extensions::webview_configuration_for`], which ties it to the one
//! `WKWebExtensionController`, so content scripts run in it; the browser
//! reports its tabs through the `tab_*` methods so the `tabs` and `windows`
//! APIs see them, as one window.
//!
//! Installing an extension grants the permissions and site access it declares.
//!
//! WebKit reads extensions asynchronously, so an install answers with what
//! the manifest says and [`ExtensionEvent::Changed`] follows once WebKit has
//! loaded it (or found it broken).
//!
//! Everything here is main-thread only, except
//! [`Extensions::download_from_amo`].

mod bridge;
mod compat;
mod package;

use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::Arc,
    time::{Duration, Instant},
};

use block2::RcBlock;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, Message,
    rc::Retained,
    runtime::{AnyClass, ProtocolObject},
};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSView};
use objc2_foundation::{NSDictionary, NSError, NSRect, NSSize, NSString, NSTimer, NSURL, NSUUID};
use objc2_web_kit::{
    WKWebExtension, WKWebExtensionContext, WKWebExtensionContextPermissionStatus,
    WKWebExtensionController, WKWebExtensionControllerConfiguration, WKWebExtensionTab,
    WKWebExtensionTabChangedProperties, WKWebView, WKWebViewConfiguration,
};
use serde::{Deserialize, Serialize};

use bridge::{ControllerDelegate, ExtensionTab, ExtensionWindow};
use package::Manifest;

/// An installed extension, for the extensions list.
#[derive(Clone, Debug)]
pub struct ExtensionInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub icon_png: Option<Vec<u8>>,
    /// Whether it has a toolbar button (`action`, `browser_action` or
    /// `page_action`).
    pub has_action: bool,
    /// Why it couldn't load, or what went wrong while it ran.
    pub errors: Vec<String>,
    pub homepage: Option<String>,
}

/// An enabled extension's toolbar button, as it stands for the active tab.
#[derive(Clone, Debug)]
pub struct ActionInfo {
    pub extension_id: String,
    pub label: String,
    pub icon: Option<Arc<gpui::Image>>,
    pub badge: String,
    pub enabled: bool,
}

/// What extensions ask of the browser.
#[derive(Clone, Debug)]
pub enum ExtensionEvent {
    /// Open a tab; an empty `url` means the browser's new-tab page. The URL
    /// may be the extension's own `webkit-extension://` page.
    OpenTab {
        url: String,
        active: bool,
        /// The `tabs.create` call waiting for this tab, if any: answer it
        /// with [`Extensions::answer_tab_request`].
        request: Option<u64>,
    },
    CloseTab {
        tab_id: u64,
    },
    ActivateTab {
        tab_id: u64,
    },
    /// The list or the toolbar buttons changed; render again.
    Changed,
    /// Only a toolbar button changed (its badge, most often): render again,
    /// with nothing else to look at.
    ActionChanged,
    /// A string an extension sent with `runtime.sendNativeMessage`.
    NativeMessage {
        extension_id: String,
        application: String,
        message: String,
    },
    /// An extension brought up to date with this version's compatibility
    /// pass, off the main thread, at launch: hand it to
    /// [`Extensions::finish_upgrade`].
    Upgraded { id: String, error: Option<String> },
}

/// The browser's extensions. Create one only when [`Extensions::supported`].
pub struct Extensions {
    shared: Rc<Shared>,
}

/// State reachable from WebKit's callbacks as well as from [`Extensions`].
///
/// WebKit calls back into us synchronously from many of its methods, and
/// those callbacks borrow `state`, so `state` is never borrowed across a
/// call into WebKit.
struct Shared {
    mtm: MainThreadMarker,
    controller: Retained<WKWebExtensionController>,
    /// The controller holds its delegate weakly.
    _delegate: Retained<ControllerDelegate>,
    window: Retained<ExtensionWindow>,
    state: RefCell<State>,
    background_timer: RefCell<Option<Retained<NSTimer>>>,
    events: Box<dyn Fn(ExtensionEvent)>,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    /// The browser's tabs, in order.
    tabs: Vec<Retained<ExtensionTab>>,
    active: Option<u64>,
    /// Where the next action popup should point, set by
    /// [`Extensions::perform_action`].
    popup_anchor: Option<(NSRect, Retained<NSView>)>,
    /// The extension whose action popup is showing.
    popup_open: Option<String>,
    /// Which extension's popup closed last, and when: a click on its button
    /// closes it (the popover dismisses itself on any click outside) before
    /// the click reaches the button, which mustn't then open it again.
    popup_closed: Option<(String, Instant)>,
    /// Watches the showing popover for its closing.
    popup_observer: Option<Retained<ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>>,
    /// `tabs.create` calls waiting for the browser to open their tab, by
    /// request number, with when they asked.
    pending_tabs: HashMap<u64, (Instant, TabCompletion)>,
    next_tab_request: u64,
    generation: u64,
}

type TabCompletion = RcBlock<dyn Fn(*mut ProtocolObject<dyn WKWebExtensionTab>, *mut NSError)>;

/// A `tabs.create` the browser hasn't answered within this long is given up.
const TAB_OPEN_PATIENCE: Duration = Duration::from_secs(10);
/// Icons are asked of WebKit at this size in points; its images carry
/// Retina pixels too, and the largest is kept.
const ICON_POINTS: f64 = 32.0;

struct Entry {
    id: String,
    /// The host of its `webkit-extension://` origin, kept across launches so
    /// its storage and pages keep their origin.
    uuid: String,
    enabled: bool,
    approved: Vec<String>,
    manifest: Manifest,
    /// Bumped on each (re)load, so a stale WebKit completion is ignored.
    generation: u64,
    context: Option<Retained<WKWebExtensionContext>>,
    /// Why it couldn't be read or loaded.
    load_error: Option<String>,
    /// The action icon rendered for each tab (`None` for no tab), since the
    /// toolbar asks for it every frame.
    action_icons: HashMap<Option<u64>, ActionIcon>,
    /// The remaining action properties also stay unchanged between WebKit's
    /// action notifications. Avoid Objective-C calls and string conversion
    /// on every toolbar render.
    action_info: HashMap<Option<u64>, ActionInfo>,
}

impl Entry {
    fn new(
        id: String,
        uuid: String,
        enabled: bool,
        approved: Vec<String>,
        manifest: Manifest,
        load_error: Option<String>,
    ) -> Self {
        Self {
            id,
            uuid,
            enabled,
            approved,
            manifest,
            generation: 0,
            context: None,
            load_error,
            action_icons: HashMap::new(),
            action_info: HashMap::new(),
        }
    }

    /// Its action was updated for one tab, or for every tab without its own
    /// (`tab` of `None`), and now shows `icon`: forgets whichever rendered
    /// icons that may have changed. Badge and title updates, far the most
    /// common (uBlock Origin's count on every page load), leave the image
    /// WebKit gives the same object, and so the rendered icon stays.
    fn action_updated(&mut self, tab: Option<u64>, icon: Option<&NSImage>) {
        let same = |cached: &ActionIcon| {
            cached.source.as_deref().map(std::ptr::from_ref) == icon.map(std::ptr::from_ref)
        };
        match tab {
            Some(tab) => {
                self.action_info.remove(&Some(tab));
                if !self.action_icons.get(&Some(tab)).is_some_and(same) {
                    self.action_icons.remove(&Some(tab));
                }
            }
            None => {
                self.action_info.clear();
                self.action_icons.retain(|_, cached| same(cached));
            }
        }
    }
}

struct ActionIcon {
    /// The image WebKit gave, kept to tell whether a later update changed
    /// it: WebKit caches its icons, and the same object means the same icon.
    source: Option<Retained<NSImage>>,
    image: Option<Arc<gpui::Image>>,
}

enum ActionCandidate {
    Cached(ActionInfo),
    Read {
        id: String,
        context: Retained<WKWebExtensionContext>,
        icon: Option<Option<Arc<gpui::Image>>>,
    },
}

/// `extensions.json`: which extensions are enabled, in list order.
#[derive(Default, Serialize, Deserialize)]
struct Registry {
    extensions: Vec<RegistryEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct RegistryEntry {
    id: String,
    enabled: bool,
    uuid: String,
    #[serde(default)]
    approved: Vec<String>,
}

impl Registry {
    fn path() -> Option<PathBuf> {
        Some(package::extensions_dir()?.join("extensions.json"))
    }

    fn load() -> Self {
        Self::path()
            .and_then(|path| fs::read(path).ok())
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("HOME is unset"))?;
        fs::create_dir_all(path.parent().expect("registry has a parent"))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        fs::rename(temporary, path)
    }
}

fn new_uuid() -> String {
    NSUUID::new().UUIDString().to_string().to_lowercase()
}

/// An extension unpacked and patched in a staging folder by
/// [`Extensions::prepare`], waiting to be installed. Dropping it
/// uninstalled removes the folder.
#[derive(Debug)]
pub struct Prepared {
    root: PathBuf,
    staging: PathBuf,
    manifest: Manifest,
    /// The installed version this updates, if it's an update.
    replaces: Option<String>,
}

impl Prepared {
    /// Marks this as the update of installed version `version`: it's only
    /// installed over that version, and keeps it switched on or off.
    pub fn replacing(mut self, version: String) -> Self {
        self.replaces = Some(version);
        self
    }
}

impl Drop for Prepared {
    fn drop(&mut self) {
        if !self.staging.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.staging);
        }
    }
}

/// Keep the old package until the new one has reached its installed path.
/// A failed second rename puts it back before returning.
fn replace_package(root: &Path, id: &str, staging: &Path) -> std::io::Result<()> {
    let dir = root.join(id);
    let backup = root.join(format!(".previous-{id}--{}", new_uuid()));
    let had_old = dir.exists();
    if had_old {
        fs::rename(&dir, &backup)?;
    }
    if let Err(err) = fs::rename(staging, &dir) {
        if had_old {
            fs::rename(&backup, &dir)?;
        }
        return Err(err);
    }
    if had_old {
        let _ = fs::remove_dir_all(backup);
    }
    Ok(())
}

fn restore_interrupted_replacements(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let Some(id) = name.strip_prefix(".previous-").and_then(|s| s.rsplit_once("--")).map(|(id, _)| id) else { continue };
        let dir = root.join(id);
        if dir.exists() {
            let _ = fs::remove_dir_all(entry.path());
        } else if let Err(err) = fs::rename(entry.path(), &dir) {
            eprintln!("Could not restore extension {id}: {err}");
        }
    }
}

impl Extensions {
    /// Whether this macOS has WebKit's extension support (15.4 and later).
    pub fn supported() -> bool {
        AnyClass::get(c"WKWebExtensionController").is_some()
    }

    /// Starts the extension controller and loads the installed extensions.
    /// `events` is called on the main thread, possibly from inside WebKit, so
    /// it should only queue work.
    ///
    /// Panics off the main thread, and where WebKit has no extension support.
    /// `background` carries what work off the main thread reports, as
    /// events like the rest.
    pub fn new(
        events: impl Fn(ExtensionEvent) + 'static,
        background: async_channel::Sender<ExtensionEvent>,
    ) -> Self {
        let mtm = MainThreadMarker::new().expect("extensions are main-thread only");
        let shared = Rc::new_cyclic(|weak: &Weak<Shared>| {
            // The default configuration is persistent: extension storage
            // survives relaunches, keyed by each context's unique identifier.
            let configuration = unsafe {
                // SAFETY: plain constructors on the main thread.
                WKWebExtensionControllerConfiguration::defaultConfiguration(mtm)
            };
            // SAFETY: as above.
            let controller = unsafe {
                WKWebExtensionController::initWithConfiguration(
                    WKWebExtensionController::alloc(mtm),
                    &configuration,
                )
            };
            let delegate = ControllerDelegate::new(mtm, weak.clone());
            // SAFETY: `delegate` is kept alive in `Shared` for as long as the
            // controller, which only holds it weakly.
            unsafe { controller.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
            Shared {
                mtm,
                controller,
                _delegate: delegate,
                window: ExtensionWindow::new(mtm, weak.clone()),
                state: RefCell::default(),
                background_timer: RefCell::default(),
                events: Box::new(events),
            }
        });
        let this = Self { shared };
        this.load_installed(background);
        this
    }

    /// Adds every extension folder, in `extensions.json` order, then any it
    /// doesn't mention, and starts WebKit reading them.
    fn load_installed(&self, background: async_channel::Sender<ExtensionEvent>) {
        let Some(root) = package::extensions_dir() else {
            return;
        };
        restore_interrupted_replacements(&root);
        let mut known = Registry::load().extensions;
        let mut others: Vec<String> = fs::read_dir(&root)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().join("manifest.json").is_file())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|id| !id.starts_with('.') && !known.iter().any(|k| k.id == *id))
            .collect();
        others.sort();
        // Saved only if a folder appeared or vanished since last time.
        let mut changed = !others.is_empty();
        known.extend(others.into_iter().map(|id| RegistryEntry {
            id,
            enabled: true,
            uuid: new_uuid(),
            approved: Vec::new(),
        }));
        let mut ids = Vec::new();
        // Behind this version's compatibility pass: brought up to date on
        // another thread (it rewrites every script, and uBlock Origin has
        // megabytes of them), then loaded.
        let mut behind = Vec::new();
        {
            let mut state = self.shared.state.borrow_mut();
            for record in known {
                let dir = root.join(&record.id);
                if !dir.is_dir() {
                    changed = true;
                    continue;
                }
                let current = compat::is_current(&dir);
                let (manifest, load_error) = match package::read_manifest(&dir) {
                    Ok(manifest) => {
                        if current {
                            ids.push(record.id.clone());
                        } else {
                            behind.push((record.id.clone(), dir.clone()));
                        }
                        (manifest, None)
                    }
                    // Listed, so it can be seen and removed; WebKit would
                    // only reject it too.
                    Err(err) => (
                        Manifest {
                            name: record.id.clone(),
                            ..Manifest::default()
                        },
                        Some(err),
                    ),
                };
                state.entries.push(Entry::new(
                    record.id,
                    record.uuid,
                    record.enabled,
                    record.approved,
                    manifest,
                    load_error,
                ));
            }
        }
        if changed {
            self.shared.save_registry();
        }
        for id in ids {
            self.shared.start_loading(&id);
        }
        if !behind.is_empty() {
            std::thread::spawn(move || {
                for (id, dir) in behind {
                    let error = compat::apply(&dir).err();
                    let _ = background.try_send(ExtensionEvent::Upgraded { id, error });
                }
            });
        }
    }

    /// Loads an extension the compatibility pass has brought up to date.
    pub fn finish_upgrade(&self, id: &str, error: Option<String>) {
        if let Some(err) = error {
            eprintln!("[extension {id}] compatibility pass failed: {err}");
        }
        let Some(dir) = package::extensions_dir().map(|root| root.join(id)) else {
            return;
        };
        // The pass adjusts the manifest too.
        let manifest = package::read_manifest(&dir);
        let known = self
            .shared
            .with_entry(id, |entry| {
                if let Ok(manifest) = manifest {
                    entry.manifest = manifest;
                }
            })
            .is_some();
        if known {
            self.shared.start_loading(id);
        }
    }

    /// A fresh configuration for a tab's web view, tied to the extension
    /// controller. The browser adds its own settings to it.
    fn webview_configuration(&self) -> Retained<WKWebViewConfiguration> {
        // SAFETY: a plain constructor, and a valid controller for the
        // configuration to retain.
        unsafe {
            let configuration = WKWebViewConfiguration::new(self.shared.mtm);
            configuration.setWebExtensionController(Some(&self.shared.controller));
            configuration
        }
    }

    /// The configuration for a tab about to load `url`: an extension's own
    /// pages (`webkit-extension://<its uuid>/…`, like uBlock Origin's logger
    /// or dashboard) only load in a web view made from that extension's
    /// configuration; everything else uses [`Extensions::webview_configuration`].
    pub fn webview_configuration_for(&self, url: &str) -> Retained<WKWebViewConfiguration> {
        let host = url
            .strip_prefix("webkit-extension://")
            .and_then(|rest| rest.split('/').next())
            .map(str::to_ascii_lowercase);
        let context = host.and_then(|host| {
            let state = self.shared.state.borrow();
            state
                .entries
                .iter()
                .find(|e| e.uuid.eq_ignore_ascii_case(&host))
                .and_then(|e| e.context.clone())
        });
        // SAFETY: a plain read of a loaded context's configuration.
        context
            .and_then(|context| unsafe { context.webViewConfiguration() })
            .unwrap_or_else(|| self.webview_configuration())
    }

    /// Installs (or updates) the extension in an `.xpi`/`.zip` package or an
    /// unpacked folder. It is copied into the extensions folder under the id
    /// its manifest gives Firefox, else under its name, and enabled.
    /// Unpacks and patches the extension at `path` beside the installed
    /// ones, ready for [`Extensions::install_prepared`]. Blocking — it reads
    /// and rewrites every file in the package — so call it off the main
    /// thread.
    pub fn prepare(path: &Path) -> Result<Prepared, String> {
        let root = package::extensions_dir().ok_or("HOME is unset")?;
        fs::create_dir_all(&root).map_err(|err| format!("Could not create {root:?}: {err}"))?;
        let staging = package::staging_dir(&root);
        let manifest = package::unpack(path, &staging)
            .and_then(|()| compat::apply(&staging))
            .and_then(|()| package::read_manifest(&staging));
        match manifest {
            Ok(manifest) => Ok(Prepared {
                root,
                staging,
                manifest,
                replaces: None,
            }),
            Err(err) => {
                let _ = fs::remove_dir_all(&staging);
                Err(err)
            }
        }
    }

    /// Puts a prepared extension in place, replacing (and keeping the
    /// storage of) any older version, and loads it. Quick: a rename and
    /// the registry. An update whose extension was removed or changed
    /// meanwhile is dropped, with `Ok(None)`.
    pub fn install_prepared(&mut self, mut prepared: Prepared) -> Result<Option<ExtensionInfo>, String> {
        if let Some(version) = &prepared.replaces {
            let state = self.shared.state.borrow();
            let current = state.entries.iter().find(|e| e.id == prepared.manifest.id);
            if current.is_none_or(|entry| entry.manifest.version != *version) {
                return Ok(None);
            }
        }
        let update = prepared.replaces.is_some();
        let root = prepared.root.clone();
        let staging = std::mem::take(&mut prepared.staging);
        let manifest = std::mem::take(&mut prepared.manifest);
        let id = manifest.id.clone();
        // An update replaces the running copy.
        if let Some(context) = self.shared.context(&id) {
            self.shared.unload(&context);
        }
        if let Err(err) = replace_package(&root, &id, &staging) {
            let _ = fs::remove_dir_all(&staging);
            if let Some(context) = self.shared.context(&id) {
                self.shared.load(&id, &context);
            }
            return Err(format!("Could not replace extension {id}: {err}"));
        }
        let enabled = {
            let mut state = self.shared.state.borrow_mut();
            // An update keeps its origin, and so its storage, and stays
            // switched off if it was; installing by hand switches it on.
            match state.entries.iter_mut().find(|e| e.id == id) {
                Some(entry) => {
                    let uuid = std::mem::take(&mut entry.uuid);
                    let approved = std::mem::take(&mut entry.approved);
                    let enabled = entry.enabled || !update;
                    *entry = Entry::new(id.clone(), uuid, enabled, approved, manifest, None);
                    enabled
                }
                None => {
                    let entry = Entry::new(id.clone(), new_uuid(), true, Vec::new(), manifest, None);
                    state.entries.push(entry);
                    true
                }
            }
        };
        self.shared.save_registry();
        if enabled {
            self.shared.start_loading(&id);
        }
        let state = self.shared.state.borrow();
        state
            .entries
            .iter()
            .find(|e| e.id == id)
            .map(|entry| Some(info(entry)))
            .ok_or_else(|| "Installed extension vanished".into())
    }

    /// Downloads the current version of an add-on from addons.mozilla.org —
    /// given its page address or bare slug — to a temporary `.xpi` for
    /// [`Extensions::prepare`]. Blocking; call it off the main
    /// thread.
    pub fn download_from_amo(query: &str) -> Result<PathBuf, String> {
        package::download_from_amo(query)
    }

    /// A newer version of add-on `id` than `installed`, downloaded from
    /// addons.mozilla.org, with its version. Blocking.
    pub fn newer_on_amo(id: &str, installed: &str) -> Result<Option<(String, PathBuf)>, String> {
        package::newer_on_amo(id, installed)
    }

    /// The installed extensions, in install order.
    pub fn list(&self) -> Vec<ExtensionInfo> {
        let state = self.shared.state.borrow();
        state.entries.iter().map(info).collect()
    }

    /// Whether extension `id` is installed and switched on.
    pub fn is_enabled(&self, id: &str) -> bool {
        let state = self.shared.state.borrow();
        state.entries.iter().any(|e| e.id == id && e.enabled)
    }

    /// Turns an extension on or off, and remembers it.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        let changed = self.shared.with_entry(id, |entry| {
            if entry.enabled == enabled {
                return None;
            }
            entry.enabled = enabled;
            entry.action_icons.clear();
            entry.action_info.clear();
            Some(entry.context.clone())
        });
        let Some(context) = changed.flatten() else {
            return;
        };
        self.shared.save_registry();
        if enabled {
            self.shared.start_loading(id);
        } else if let Some(context) = context {
            self.shared.unload(&context);
        }
    }

    /// Uninstalls an extension, deleting its files. Its stored data stays
    /// with WebKit, keyed by id, and comes back if it is reinstalled.
    pub fn remove(&mut self, id: &str) {
        let entry = {
            let mut state = self.shared.state.borrow_mut();
            let Some(index) = state.entries.iter().position(|e| e.id == id) else {
                return;
            };
            state.entries.remove(index)
        };
        if let Some(context) = &entry.context {
            self.shared.unload(context);
        }
        if let Some(root) = package::extensions_dir() {
            let _ = fs::remove_dir_all(root.join(&entry.id));
        }
        self.shared.save_registry();
    }

    /// Toolbar buttons of the enabled extensions that have one, as they
    /// stand for the active tab.
    pub fn actions(&self) -> Vec<ActionInfo> {
        let tab = self.shared.active_tab();
        let tab_id = tab.as_ref().map(|tab| tab.ivars().id);
        let candidates: Vec<_> = {
            let state = self.shared.state.borrow();
            state
                .entries
                .iter()
                .filter(|e| e.enabled && e.manifest.has_action)
                .filter_map(|e| {
                    if let Some(info) = e.action_info.get(&tab_id) {
                        return Some(ActionCandidate::Cached(info.clone()));
                    }
                    Some(ActionCandidate::Read {
                        id: e.id.clone(),
                        context: e.context.clone()?,
                        icon: e.action_icons.get(&tab_id).map(|icon| icon.image.clone()),
                    })
                })
                .collect()
        };
        let mut actions = Vec::new();
        for candidate in candidates {
            let (id, context, cached) = match candidate {
                ActionCandidate::Cached(info) => {
                    actions.push(info);
                    continue;
                }
                ActionCandidate::Read { id, context, icon } => (id, context, icon),
            };
            // SAFETY: reads on a live context; the tab is ours.
            let action = unsafe {
                if !context.isLoaded() {
                    continue;
                }
                context.actionForTab(tab.as_deref().map(ProtocolObject::from_ref))
            };
            let Some(action) = action else {
                continue;
            };
            let icon = cached.unwrap_or_else(|| {
                // SAFETY: as above.
                let source = unsafe { action.iconForSize(NSSize::new(ICON_POINTS, ICON_POINTS)) };
                let image = source
                    .as_deref()
                    .and_then(png_from_image)
                    .map(|png| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png)));
                let cached = ActionIcon {
                    source,
                    image: image.clone(),
                };
                self.shared.with_entry(&id, |entry| {
                    entry.action_icons.insert(tab_id, cached);
                });
                image
            });
            // SAFETY: plain property reads.
            let (label, badge, enabled) =
                unsafe { (action.label(), action.badgeText(), action.isEnabled()) };
            let info = ActionInfo {
                extension_id: id,
                label: label.to_string(),
                icon,
                badge: badge.to_string(),
                enabled,
            };
            self.shared.with_entry(&info.extension_id, |entry| {
                entry.action_info.insert(tab_id, info.clone());
            });
            actions.push(info);
        }
        actions
    }

    /// Clicks an extension's toolbar button. If it has a popup, the popup
    /// opens in a native popover — which floats above web views — pointing
    /// at `anchor`, a rectangle in `view` given top-left-origin like GPUI's
    /// window coordinates (flipped here if `view` isn't).
    pub fn perform_action(&self, extension_id: &str, anchor: NSRect, view: &NSView) {
        let Some(context) = self.shared.context(extension_id) else {
            return;
        };
        let tab = self.shared.active_tab();
        {
            let mut state = self.shared.state.borrow_mut();
            // The button toggles its popup.
            if state.popup_open.as_deref() == Some(extension_id) {
                drop(state);
                // SAFETY: a loaded context and our own tab.
                if let Some(action) =
                    unsafe { context.actionForTab(tab.as_deref().map(ProtocolObject::from_ref)) }
                {
                    // SAFETY: closing a popup that is showing.
                    unsafe { action.closePopup() };
                }
                return;
            }
            if let Some((closed, at)) = &state.popup_closed
                && closed == extension_id
                && at.elapsed() < Duration::from_millis(400)
            {
                return;
            }
            state.popup_anchor = Some((anchor, view.retain()));
        }
        // The popup's first act is usually to reach the background, and
        // WebKit refuses a runtime.connect to a background it has unloaded
        // rather than waking it. So wake it, then run the action.
        let pending = std::cell::Cell::new(Some((context.clone(), tab)));
        let run = RcBlock::new(move |_error: *mut NSError| {
            let Some((context, tab)) = pending.take() else {
                return;
            };
            // SAFETY: a loaded context and our own tab; WebKit may call the
            // delegate's popup method from inside, which is why nothing is
            // borrowed here.
            unsafe { context.performActionForTab(tab.as_deref().map(ProtocolObject::from_ref)) };
        });
        // SAFETY: a live context and a block of the documented type, called
        // once on the main thread.
        unsafe { context.loadBackgroundContentWithCompletionHandler(&run) };
    }

    /// The browser opened a tab, whose page is in `webview` (made from
    /// [`Extensions::webview_configuration_for`]). It goes at the end.
    pub fn tab_opened(&mut self, tab_id: u64, webview: &WKWebView) {
        let tab = ExtensionTab::new(
            self.shared.mtm,
            tab_id,
            webview.retain(),
            Rc::downgrade(&self.shared),
        );
        self.shared.state.borrow_mut().tabs.push(tab.clone());
        // SAFETY: `tab` conforms to the protocol and is kept in `state`.
        unsafe {
            self.shared
                .controller
                .didOpenTab(ProtocolObject::from_ref(&*tab))
        };
    }

    /// Answers `tabs.create` call `request` with the tab the browser opened
    /// for it: `tab_id`, or no tab if it opened none extensions can see (a
    /// private tab, the new-tab page). Any others that waited too long,
    /// their answer lost, are told there's no tab.
    pub fn answer_tab_request(&mut self, request: u64, tab_id: Option<u64>) {
        let (answer, tab, stale) = {
            let mut state = self.shared.state.borrow_mut();
            let answer = state.pending_tabs.remove(&request).map(|(_, c)| c);
            let tab = tab_id.and_then(|id| state.tabs.iter().find(|t| t.ivars().id == id).cloned());
            let expired: Vec<u64> = state
                .pending_tabs
                .iter()
                .filter(|(_, (asked, _))| asked.elapsed() >= TAB_OPEN_PATIENCE)
                .map(|(r, _)| *r)
                .collect();
            let stale: Vec<TabCompletion> = expired
                .into_iter()
                .filter_map(|r| state.pending_tabs.remove(&r).map(|(_, c)| c))
                .collect();
            (answer, tab, stale)
        };
        if let Some(completion) = answer {
            let tab: *mut ProtocolObject<dyn WKWebExtensionTab> = match &tab {
                Some(tab) => Retained::as_ptr(tab).cast_mut().cast(),
                None => std::ptr::null_mut(),
            };
            completion.call((tab, std::ptr::null_mut()));
        }
        for completion in stale {
            completion.call((std::ptr::null_mut(), std::ptr::null_mut()));
        }
    }

    /// The browser closed a tab.
    pub fn tab_closed(&mut self, tab_id: u64) {
        let tab = {
            let mut state = self.shared.state.borrow_mut();
            let Some(index) = state.tabs.iter().position(|t| t.ivars().id == tab_id) else {
                return;
            };
            if state.active == Some(tab_id) {
                state.active = None;
            }
            for entry in &mut state.entries {
                entry.action_icons.remove(&Some(tab_id));
                entry.action_info.remove(&Some(tab_id));
            }
            state.tabs.remove(index)
        };
        // SAFETY: a tab WebKit was told about.
        unsafe {
            self.shared
                .controller
                .didCloseTab_windowIsClosing(ProtocolObject::from_ref(&*tab), false)
        };
    }

    /// The browser switched to another tab.
    pub fn tab_activated(&mut self, tab_id: u64) {
        let (tab, previous) = {
            let mut state = self.shared.state.borrow_mut();
            if state.active == Some(tab_id) {
                return;
            }
            let find = |id: Option<u64>| {
                state
                    .tabs
                    .iter()
                    .find(|t| Some(t.ivars().id) == id)
                    .cloned()
            };
            let (tab, previous) = (find(Some(tab_id)), find(state.active));
            let Some(tab) = tab else {
                return;
            };
            state.active = Some(tab_id);
            for entry in &mut state.entries {
                entry.action_info.remove(&Some(tab_id));
            }
            (tab, previous)
        };
        // SAFETY: tabs WebKit was told about.
        unsafe {
            self.shared.controller.didActivateTab_previousActiveTab(
                ProtocolObject::from_ref(&*tab),
                previous.as_deref().map(ProtocolObject::from_ref),
            )
        };
    }

    /// A tab's title or address changed.
    pub fn tab_updated(&mut self, tab_id: u64, title: &str, url: &str) {
        let Some(tab) = self.shared.tab(tab_id) else {
            return;
        };
        let mut changed = WKWebExtensionTabChangedProperties::None;
        let ivars = tab.ivars();
        if *ivars.title.borrow() != title {
            *ivars.title.borrow_mut() = title.to_owned();
            changed |= WKWebExtensionTabChangedProperties::Title;
        }
        if *ivars.url.borrow() != url {
            *ivars.url.borrow_mut() = url.to_owned();
            changed |= WKWebExtensionTabChangedProperties::URL;
        }
        if changed != WKWebExtensionTabChangedProperties::None {
            for entry in &mut self.shared.state.borrow_mut().entries {
                entry.action_info.remove(&Some(tab_id));
            }
            // SAFETY: a tab WebKit was told about.
            unsafe {
                self.shared
                    .controller
                    .didChangeTabProperties_forTab(changed, ProtocolObject::from_ref(&*tab))
            };
        }
    }
}

impl Shared {
    fn emit(&self, event: ExtensionEvent) {
        (self.events)(event);
    }

    fn tab(&self, tab_id: u64) -> Option<Retained<ExtensionTab>> {
        let state = self.state.borrow();
        state.tabs.iter().find(|t| t.ivars().id == tab_id).cloned()
    }

    fn active_tab(&self) -> Option<Retained<ExtensionTab>> {
        let active = self.state.borrow().active?;
        self.tab(active)
    }

    /// Runs `f` on extension `id`'s entry, if it's still there. `state` is
    /// borrowed meanwhile, so `f` mustn't call into WebKit.
    fn with_entry<R>(&self, id: &str, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
        let mut state = self.state.borrow_mut();
        state.entries.iter_mut().find(|e| e.id == id).map(f)
    }

    fn context(&self, id: &str) -> Option<Retained<WKWebExtensionContext>> {
        self.with_entry(id, |entry| entry.context.clone()).flatten()
    }

    /// The id of the extension running in `context`.
    fn id_of(&self, context: &WKWebExtensionContext) -> Option<String> {
        let state = self.state.borrow();
        state
            .entries
            .iter()
            .find(|e| {
                e.context
                    .as_deref()
                    .is_some_and(|c| std::ptr::eq(c, context))
            })
            .map(|e| e.id.clone())
    }

    fn save_registry(&self) {
        let registry = Registry {
            extensions: self
                .state
                .borrow()
                .entries
                .iter()
                .map(|e| RegistryEntry {
                    id: e.id.clone(),
                    enabled: e.enabled,
                    uuid: e.uuid.clone(),
                    approved: e.approved.clone(),
                })
                .collect(),
        };
        if let Err(err) = registry.save() {
            eprintln!("Could not save extensions.json: {err}");
        }
    }

    /// Asks WebKit to read an extension's folder; [`Shared::finish_loading`]
    /// takes it from there.
    fn start_loading(self: &Rc<Self>, id: &str) {
        let Some(dir) = package::extensions_dir().map(|root| root.join(id)) else {
            return;
        };
        let generation = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.generation
        };
        if self
            .with_entry(id, |entry| entry.generation = generation)
            .is_none()
        {
            return;
        }
        let url =
            NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&dir.to_string_lossy()), true);
        let weak = Rc::downgrade(self);
        let id = id.to_owned();
        let done = RcBlock::new(move |extension: *mut WKWebExtension, error: *mut NSError| {
            let Some(shared) = weak.upgrade() else {
                return;
            };
            // SAFETY: WebKit passes a valid (or nil) extension and error,
            // alive for the duration of the call; retaining keeps them.
            let (extension, error) =
                unsafe { (Retained::retain(extension), Retained::retain(error)) };
            shared.finish_loading(&id, generation, extension, error);
        });
        // SAFETY: `url` is a file URL, and WebKit copies the block.
        unsafe {
            WKWebExtension::extensionWithResourceBaseURL_completionHandler(&url, &done, self.mtm)
        };
    }

    /// Makes a context for a freshly read extension, grants it what it asks
    /// for, and loads it if enabled.
    fn finish_loading(
        self: &Rc<Self>,
        id: &str,
        generation: u64,
        extension: Option<Retained<WKWebExtension>>,
        error: Option<Retained<NSError>>,
    ) {
        let current = self.with_entry(id, |entry| {
            (entry.generation == generation).then(|| (entry.uuid.clone(), entry.enabled))
        });
        let Some((uuid, enabled)) = current.flatten() else {
            return;
        };
        let Some(extension) = extension else {
            let message = error.map_or_else(
                || "WebKit could not read this extension".to_owned(),
                |error| error.localizedDescription().to_string(),
            );
            self.with_entry(id, |entry| entry.load_error = Some(message));
            self.emit(ExtensionEvent::Changed);
            return;
        };
        let requested = unsafe { requested_access(&extension) };
        let approved = self.with_entry(id, |entry| entry.approved.clone()).unwrap_or_default();
        let missing: Vec<String> = requested.into_iter().filter(|item| !approved.contains(item)).collect();
        if enabled && !missing.is_empty() {
            self.with_entry(id, |entry| entry.approved.extend(missing));
            self.save_registry();
        }
        let approved = self.with_entry(id, |entry| entry.approved.clone()).unwrap_or_default();
        // SAFETY: a context for a valid extension, configured before it is
        // loaded, as WebKit requires.
        let context = unsafe {
            let context = WKWebExtensionContext::contextForExtension(&extension);
            // A stable identifier keeps its storage across launches, and a
            // stable base URL keeps its pages' origin.
            context.setUniqueIdentifier(&NSString::from_str(id));
            if let Some(base) =
                NSURL::URLWithString(&NSString::from_str(&format!("webkit-extension://{uuid}/")))
            {
                context.setBaseURL(&base);
            }
            context.setInspectable(true);
            context.setInspectionName(extension.displayName().as_deref());
            grant_approved(&context, &extension, &approved);
            context
        };
        // SAFETY: a plain read.
        let icon = unsafe { extension.iconForSize(NSSize::new(ICON_POINTS, ICON_POINTS)) }
            .and_then(|image| png_from_image(&image));
        let found = self.with_entry(id, |entry| {
            entry.context = Some(context.clone());
            entry.load_error = None;
            entry.action_icons.clear();
            entry.action_info.clear();
            if icon.is_some() {
                entry.manifest.icon_png = icon;
            }
        });
        if found.is_none() {
            return;
        }
        if enabled {
            self.load(id, &context);
        }
        self.emit(ExtensionEvent::Changed);
    }

    /// Starts an extension running, noting any failure against it.
    fn load(self: &Rc<Self>, id: &str, context: &WKWebExtensionContext) {
        // SAFETY: WebKit calls the delegate and the tab/window objects from
        // inside; nothing is borrowed across it.
        let result = unsafe { self.controller.loadExtensionContext_error(context) };
        let error = result
            .err()
            .map(|error| error.localizedDescription().to_string());
        self.with_entry(id, |entry| entry.load_error = error);
        self.update_background_timer();
    }

    fn unload(self: &Rc<Self>, context: &WKWebExtensionContext) {
        if let Some(id) = self.id_of(context) {
            self.with_entry(&id, |entry| entry.action_info.clear());
        }
        // SAFETY: as in `load`. Unloading one that isn't loaded is a
        // harmless error.
        let _ = unsafe { self.controller.unloadExtensionContext_error(context) };
        self.update_background_timer();
        self.emit(ExtensionEvent::Changed);
    }

    fn transient_backgrounds(&self) -> Vec<Retained<WKWebExtensionContext>> {
        let contexts: Vec<_> = self
            .state
            .borrow()
            .entries
            .iter()
            .filter(|entry| entry.enabled)
            .filter_map(|entry| entry.context.clone())
            .collect();
        contexts
            .into_iter()
            .filter(|context| {
                // SAFETY: plain property reads, with no state borrow held.
                unsafe {
                    let extension = context.webExtension();
                    context.isLoaded()
                        && extension.hasBackgroundContent()
                        && !extension.hasPersistentBackgroundContent()
                }
            })
            .collect()
    }

    /// WebKit does not wake some MV3 backgrounds for runtime.connect.
    /// Keep that workaround only while an enabled extension needs it;
    /// browsers without one should have no periodic extension wakeups.
    fn update_background_timer(self: &Rc<Self>) {
        if self.transient_backgrounds().is_empty() {
            if let Some(timer) = self.background_timer.borrow_mut().take() {
                timer.invalidate();
            }
            return;
        }
        if self.background_timer.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let done = RcBlock::new(|_error: *mut NSError| {});
        let tick = RcBlock::new(move |timer: std::ptr::NonNull<NSTimer>| {
            let Some(shared) = weak.upgrade() else {
                // SAFETY: the timer stays live during its callback.
                unsafe { timer.as_ref() }.invalidate();
                return;
            };
            let contexts = shared.transient_backgrounds();
            if contexts.is_empty() {
                shared.background_timer.borrow_mut().take();
                // SAFETY: as above.
                unsafe { timer.as_ref() }.invalidate();
                return;
            }
            for context in contexts {
                // SAFETY: live, loaded contexts; no state borrow is held.
                unsafe { context.loadBackgroundContentWithCompletionHandler(&done) };
            }
        });
        // SAFETY: main run-loop timer with the documented block signature.
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(15.0, true, &tick) };
        timer.setTolerance(3.0);
        *self.background_timer.borrow_mut() = Some(timer);
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(timer) = self.background_timer.get_mut().take() {
            timer.invalidate();
        }
    }
}

unsafe fn requested_access(extension: &WKWebExtension) -> Vec<String> {
    let mut access = Vec::new();
    unsafe {
        for permission in &extension.requestedPermissions() {
            access.push(format!("permission:{permission}"));
        }
        for pattern in &extension.allRequestedMatchPatterns() {
            access.push(format!("site:{}", pattern.string()));
        }
    }
    access.sort();
    access.dedup();
    access
}

unsafe fn grant_approved(context: &WKWebExtensionContext, extension: &WKWebExtension, approved: &[String]) {
    let granted = WKWebExtensionContextPermissionStatus::GrantedExplicitly;
    unsafe {
        for permission in &extension.requestedPermissions() {
            if approved.contains(&format!("permission:{permission}")) {
                context.setPermissionStatus_forPermission(granted, &permission);
            }
        }
        for pattern in &extension.allRequestedMatchPatterns() {
            if approved.contains(&format!("site:{}", pattern.string())) {
                context.setPermissionStatus_forMatchPattern(granted, &pattern);
            }
        }
    }
}

/// What the extensions list shows of `entry`: WebKit's localized names and
/// errors once it has read the extension, else what the manifest says.
fn info(entry: &Entry) -> ExtensionInfo {
    let mut info = ExtensionInfo {
        id: entry.id.clone(),
        name: entry.manifest.name.clone(),
        version: entry.manifest.version.clone(),
        description: entry.manifest.description.clone(),
        enabled: entry.enabled,
        icon_png: entry.manifest.icon_png.clone(),
        has_action: entry.manifest.has_action,
        errors: entry.load_error.iter().cloned().collect(),
        homepage: entry.manifest.homepage.clone(),
    };
    if let Some(context) = &entry.context {
        // SAFETY: plain property reads; none calls back into us.
        let (extension, context_errors) = unsafe { (context.webExtension(), context.errors()) };
        // WebKit's names are localized for the user's locale.
        // SAFETY: as above.
        unsafe {
            if let Some(name) = extension.displayName() {
                info.name = name.to_string();
            }
            if let Some(version) = extension.displayVersion() {
                info.version = version.to_string();
            }
            if let Some(description) = extension.displayDescription() {
                info.description = description.to_string();
            }
        }
        for error in &context_errors {
            let message = error.localizedDescription().to_string();
            if !info.errors.contains(&message) {
                info.errors.push(message);
            }
        }
    }
    info
}

/// An `NSImage` as PNG bytes, from its largest bitmap.
fn png_from_image(image: &NSImage) -> Option<Vec<u8>> {
    let tiff = image.TIFFRepresentation()?;
    let largest = NSBitmapImageRep::imageRepsWithData(&tiff)
        .iter()
        .filter_map(|rep| rep.downcast::<NSBitmapImageRep>().ok())
        .max_by_key(|rep| rep.pixelsWide())?;
    // SAFETY: an empty property dictionary is valid for any file type.
    let png = unsafe {
        largest.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(png.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_changes_invalidate_only_the_affected_tab_until_defaults_change() {
        let mut entry = Entry::new(
            "example".into(),
            "origin".into(),
            true,
            Vec::new(),
            Manifest::default(),
            None,
        );
        for tab in [None, Some(1), Some(2)] {
            entry.action_info.insert(
                tab,
                ActionInfo {
                    extension_id: entry.id.clone(),
                    label: "action".into(),
                    icon: None,
                    badge: "1".into(),
                    enabled: true,
                },
            );
            entry.action_icons.insert(
                tab,
                ActionIcon {
                    source: None,
                    image: None,
                },
            );
        }
        entry.action_updated(Some(1), None);
        assert!(!entry.action_info.contains_key(&Some(1)));
        assert!(entry.action_info.contains_key(&Some(2)));
        assert!(entry.action_info.contains_key(&None));
        // A badge/title change keeps the already rendered, unchanged icon.
        assert_eq!(entry.action_icons.len(), 3);
        entry.action_updated(None, None);
        assert!(entry.action_info.is_empty());
        assert_eq!(entry.action_icons.len(), 3);
    }

    #[test]
    fn old_extension_registry_has_no_implicit_approvals() {
        let record: RegistryEntry = serde_json::from_str(
            r#"{"id":"example","enabled":true,"uuid":"abc"}"#
        ).unwrap();
        assert!(record.approved.is_empty());
    }

    #[test]
    fn package_replacement_preserves_old_copy_on_failure() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("vamp-replace-{}-{nanos}", std::process::id()));
        let old = root.join("example");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("old.txt"), b"old").unwrap();
        assert!(replace_package(&root, "example", &root.join("missing")).is_err());
        assert_eq!(fs::read(old.join("old.txt")).unwrap(), b"old");

        let staging = root.join("incoming");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("new.txt"), b"new").unwrap();
        replace_package(&root, "example", &staging).unwrap();
        assert_eq!(fs::read(old.join("new.txt")).unwrap(), b"new");
        assert!(!old.join("old.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_package_swap_restores_old_copy() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("vamp-recover-{}-{nanos}", std::process::id()));
        let backup = root.join(".previous-example--123");
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join("manifest.json"), b"old").unwrap();
        restore_interrupted_replacements(&root);
        assert_eq!(fs::read(root.join("example/manifest.json")).unwrap(), b"old");
        let _ = fs::remove_dir_all(root);
    }
}
