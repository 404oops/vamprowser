//! The Objective-C objects WebKit asks about the browser: its tabs, its one
//! window, and the controller delegate that opens tabs, grants permissions
//! and shows action popups.

use std::{
    cell::RefCell,
    ptr,
    rc::{Rc, Weak},
    time::Instant,
};

use block2::RcBlock;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, NSObjectProtocol, ProtocolObject},
};
use objc2::{Message, runtime::AnyObject};
use objc2_app_kit::{NSPopover, NSPopoverDidCloseNotification, NSView};
use objc2_foundation::{
    NSArray, NSDate, NSError, NSNotification, NSNotificationCenter, NSPoint, NSRect, NSRectEdge,
    NSSet, NSSize, NSString, NSTimer, NSURL, NSURLRequest,
};
use objc2_web_kit::{
    WKWebExtensionAction, WKWebExtensionContext, WKWebExtensionController,
    WKWebExtensionControllerDelegate, WKWebExtensionMatchPattern, WKWebExtensionPermission,
    WKWebExtensionTab, WKWebExtensionTabConfiguration, WKWebExtensionWindow,
    WKWebExtensionWindowConfiguration, WKWebExtensionWindowState, WKWebExtensionWindowType,
    WKWebView,
};

use super::{ExtensionEvent, ICON_POINTS, Shared};

type ErrorCompletion = block2::DynBlock<dyn Fn(*mut NSError)>;

/// Answers a WebKit request that has nothing to report.
fn succeed(completion: &ErrorCompletion) {
    completion.call((ptr::null_mut(),));
}

pub struct TabIvars {
    /// The browser's id for the tab.
    pub id: u64,
    pub webview: Retained<WKWebView>,
    pub title: RefCell<String>,
    pub url: RefCell<String>,
    shared: Weak<Shared>,
}

define_class!(
    /// One browser tab, as extensions see it.
    // SAFETY: NSObject has no subclassing requirements, and this class
    // doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VamprowserExtensionTab"]
    #[ivars = TabIvars]
    pub struct ExtensionTab;

    unsafe impl NSObjectProtocol for ExtensionTab {}

    unsafe impl WKWebExtensionTab for ExtensionTab {
        #[unsafe(method_id(windowForWebExtensionContext:))]
        fn window_for(
            &self,
            _context: &WKWebExtensionContext,
        ) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
            window_of(&self.ivars().shared)
        }

        #[unsafe(method(indexInWindowForWebExtensionContext:))]
        fn index_in_window(&self, _context: &WKWebExtensionContext) -> usize {
            self.ivars()
                .shared
                .upgrade()
                .and_then(|shared| {
                    let state = shared.state.borrow();
                    state
                        .tabs
                        .iter()
                        .position(|t| t.ivars().id == self.ivars().id)
                })
                // NSNotFound
                .unwrap_or(isize::MAX as usize)
        }

        #[unsafe(method_id(webViewForWebExtensionContext:))]
        fn webview_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<WKWebView>> {
            Some(self.ivars().webview.clone())
        }

        #[unsafe(method_id(titleForWebExtensionContext:))]
        fn title_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<NSString>> {
            self.title()
        }

        #[unsafe(method_id(urlForWebExtensionContext:))]
        fn url_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<NSURL>> {
            self.url()
        }

        #[unsafe(method(isLoadingCompleteForWebExtensionContext:))]
        fn loading_complete(&self, _context: &WKWebExtensionContext) -> bool {
            // SAFETY: a plain read on our web view.
            !unsafe { self.ivars().webview.isLoading() }
        }

        #[unsafe(method(isSelectedForWebExtensionContext:))]
        fn is_selected(&self, _context: &WKWebExtensionContext) -> bool {
            self.ivars()
                .shared
                .upgrade()
                .is_some_and(|shared| shared.state.borrow().active == Some(self.ivars().id))
        }

        #[unsafe(method(shouldGrantPermissionsOnUserGestureForWebExtensionContext:))]
        fn grant_on_gesture(&self, _context: &WKWebExtensionContext) -> bool {
            // What makes `activeTab` work when the action is clicked.
            true
        }

        #[unsafe(method(loadURL:forWebExtensionContext:completionHandler:))]
        fn load_url(
            &self,
            url: &NSURL,
            context: &WKWebExtensionContext,
            completion: &ErrorCompletion,
        ) {
            // A web page's view can't load an extension page (WebKit fails
            // it with "resource unavailable"), so the browser swaps views.
            if url.scheme().is_some_and(|scheme| scheme.to_string() == "webkit-extension")
                && let (Some(shared), Some(url)) =
                    (self.ivars().shared.upgrade(), url.absoluteString())
            {
                shared.emit(ExtensionEvent::LoadTab {
                    tab_id: self.ivars().id,
                    url: url.to_string(),
                });
                succeed(completion);
                return;
            }
            // SAFETY: loading a request in our own web view.
            let _ = unsafe {
                self.ivars()
                    .webview
                    .loadRequest(&NSURLRequest::requestWithURL(url))
            };
            succeed(completion);
        }

        #[unsafe(method(reloadFromOrigin:forWebExtensionContext:completionHandler:))]
        fn reload(
            &self,
            from_origin: bool,
            _context: &WKWebExtensionContext,
            completion: &ErrorCompletion,
        ) {
            let webview = &self.ivars().webview;
            // SAFETY: navigating our own web view.
            let _ = unsafe {
                if from_origin {
                    webview.reloadFromOrigin()
                } else {
                    webview.reload()
                }
            };
            succeed(completion);
        }

        #[unsafe(method(goBackForWebExtensionContext:completionHandler:))]
        fn go_back(&self, _context: &WKWebExtensionContext, completion: &ErrorCompletion) {
            // SAFETY: navigating our own web view.
            let _ = unsafe { self.ivars().webview.goBack() };
            succeed(completion);
        }

        #[unsafe(method(goForwardForWebExtensionContext:completionHandler:))]
        fn go_forward(&self, _context: &WKWebExtensionContext, completion: &ErrorCompletion) {
            // SAFETY: navigating our own web view.
            let _ = unsafe { self.ivars().webview.goForward() };
            succeed(completion);
        }

        #[unsafe(method(activateForWebExtensionContext:completionHandler:))]
        fn activate(&self, _context: &WKWebExtensionContext, completion: &ErrorCompletion) {
            if let Some(shared) = self.ivars().shared.upgrade() {
                shared.emit(ExtensionEvent::ActivateTab {
                    tab_id: self.ivars().id,
                });
            }
            succeed(completion);
        }

        #[unsafe(method(closeForWebExtensionContext:completionHandler:))]
        fn close(&self, _context: &WKWebExtensionContext, completion: &ErrorCompletion) {
            if let Some(shared) = self.ivars().shared.upgrade() {
                shared.emit(ExtensionEvent::CloseTab {
                    tab_id: self.ivars().id,
                });
            }
            succeed(completion);
        }
    }
);

/// The one window, as WebKit wants it.
fn window_of(shared: &Weak<Shared>) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
    let shared = shared.upgrade()?;
    Some(ProtocolObject::from_retained(shared.window.clone()))
}

impl ExtensionTab {
    fn title(&self) -> Option<Retained<NSString>> {
        let title = self.ivars().title.borrow();
        if title.is_empty() {
            // Before the browser has reported it, the page knows.
            // SAFETY: a plain read on our web view.
            return unsafe { self.ivars().webview.title() };
        }
        Some(NSString::from_str(&title))
    }

    fn url(&self) -> Option<Retained<NSURL>> {
        let url = self.ivars().url.borrow();
        if url.is_empty() {
            // Before the browser has reported it, the page knows.
            // SAFETY: a plain read on our web view.
            return unsafe { self.ivars().webview.URL() };
        }
        NSURL::URLWithString(&NSString::from_str(&url))
    }

    pub(super) fn new(
        mtm: MainThreadMarker,
        id: u64,
        webview: Retained<WKWebView>,
        shared: Weak<Shared>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TabIvars {
            id,
            webview,
            title: RefCell::default(),
            url: RefCell::default(),
            shared,
        });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

pub struct WindowIvars {
    shared: Weak<Shared>,
}

define_class!(
    /// The browser window, holding every tab.
    // SAFETY: as for `ExtensionTab`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VamprowserExtensionWindow"]
    #[ivars = WindowIvars]
    pub struct ExtensionWindow;

    unsafe impl NSObjectProtocol for ExtensionWindow {}

    unsafe impl WKWebExtensionWindow for ExtensionWindow {
        #[unsafe(method_id(tabsForWebExtensionContext:))]
        fn tabs(
            &self,
            _context: &WKWebExtensionContext,
        ) -> Retained<NSArray<ProtocolObject<dyn WKWebExtensionTab>>> {
            let tabs: Vec<Retained<ProtocolObject<dyn WKWebExtensionTab>>> = self
                .ivars()
                .shared
                .upgrade()
                .map(|shared| {
                    let state = shared.state.borrow();
                    state
                        .tabs
                        .iter()
                        .map(|tab| ProtocolObject::from_retained(tab.clone()))
                        .collect()
                })
                .unwrap_or_default();
            NSArray::from_retained_slice(&tabs)
        }

        #[unsafe(method_id(activeTabForWebExtensionContext:))]
        fn active_tab(
            &self,
            _context: &WKWebExtensionContext,
        ) -> Option<Retained<ProtocolObject<dyn WKWebExtensionTab>>> {
            self.ivars()
                .shared
                .upgrade()
                .and_then(|shared| shared.active_tab())
                .map(ProtocolObject::from_retained)
        }

        #[unsafe(method(windowTypeForWebExtensionContext:))]
        fn window_type(&self, _context: &WKWebExtensionContext) -> WKWebExtensionWindowType {
            WKWebExtensionWindowType::Normal
        }

        #[unsafe(method(windowStateForWebExtensionContext:))]
        fn window_state(&self, _context: &WKWebExtensionContext) -> WKWebExtensionWindowState {
            WKWebExtensionWindowState::Normal
        }

        #[unsafe(method(isPrivateForWebExtensionContext:))]
        fn is_private(&self, _context: &WKWebExtensionContext) -> bool {
            false
        }

        #[unsafe(method(frameForWebExtensionContext:))]
        fn frame(&self, _context: &WKWebExtensionContext) -> NSRect {
            self.ns_window_frame(false)
        }

        #[unsafe(method(screenFrameForWebExtensionContext:))]
        fn screen_frame(&self, _context: &WKWebExtensionContext) -> NSRect {
            self.ns_window_frame(true)
        }

        #[unsafe(method(focusForWebExtensionContext:completionHandler:))]
        fn focus(&self, _context: &WKWebExtensionContext, completion: &ErrorCompletion) {
            succeed(completion);
        }
    }
);

impl ExtensionWindow {
    pub(super) fn new(mtm: MainThreadMarker, shared: Weak<Shared>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(WindowIvars { shared });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// The real window's frame (or its screen's), found through any tab.
    fn ns_window_frame(&self, screen: bool) -> NSRect {
        let null = NSRect::new(
            NSPoint::new(f64::INFINITY, f64::INFINITY),
            NSSize::new(0.0, 0.0),
        );
        let Some(shared) = self.ivars().shared.upgrade() else {
            return null;
        };
        let webview = shared
            .state
            .borrow()
            .tabs
            .first()
            .map(|t| t.ivars().webview.clone());
        let Some(window) = webview.and_then(|w| w.window()) else {
            return null;
        };
        if screen {
            window.screen().map_or(null, |s| s.frame())
        } else {
            window.frame()
        }
    }
}

pub struct DelegateIvars {
    shared: Weak<Shared>,
}

define_class!(
    /// Does what extensions ask of the browser as a whole.
    // SAFETY: as for `ExtensionTab`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VamprowserExtensionControllerDelegate"]
    #[ivars = DelegateIvars]
    pub struct ControllerDelegate;

    unsafe impl NSObjectProtocol for ControllerDelegate {}

    unsafe impl WKWebExtensionControllerDelegate for ControllerDelegate {
        #[unsafe(method_id(webExtensionController:openWindowsForExtensionContext:))]
        fn open_windows(
            &self,
            _controller: &WKWebExtensionController,
            _context: &WKWebExtensionContext,
        ) -> Retained<NSArray<ProtocolObject<dyn WKWebExtensionWindow>>> {
            match self.ivars().shared.upgrade() {
                Some(shared) => NSArray::from_retained_slice(&[ProtocolObject::from_retained(
                    shared.window.clone(),
                )]),
                None => NSArray::new(),
            }
        }

        #[unsafe(method_id(webExtensionController:focusedWindowForExtensionContext:))]
        fn focused_window(
            &self,
            _controller: &WKWebExtensionController,
            _context: &WKWebExtensionContext,
        ) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
            window_of(&self.ivars().shared)
        }

        #[unsafe(method(webExtensionController:openNewTabUsingConfiguration:forExtensionContext:completionHandler:))]
        fn open_new_tab(
            &self,
            _controller: &WKWebExtensionController,
            configuration: &WKWebExtensionTabConfiguration,
            _context: &WKWebExtensionContext,
            completion: &block2::DynBlock<
                dyn Fn(*mut ProtocolObject<dyn WKWebExtensionTab>, *mut NSError),
            >,
        ) {
            let Some(shared) = self.ivars().shared.upgrade() else {
                completion.call((ptr::null_mut(), ptr::null_mut()));
                return;
            };
            // SAFETY: plain reads.
            let (url, active) = unsafe { (configuration.url(), configuration.shouldBeActive()) };
            let url = url
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string())
                .unwrap_or_default();
            // Answered with the tab once the browser has opened it: this
            // one, by number, not whichever tab happens to open next.
            let request = {
                let mut state = shared.state.borrow_mut();
                state.next_tab_request += 1;
                let request = state.next_tab_request;
                state.pending_tabs.insert(request, (Instant::now(), completion.copy()));
                request
            };
            shared.emit(ExtensionEvent::OpenTab {
                url,
                active,
                request: Some(request),
            });
        }

        #[unsafe(method(webExtensionController:openNewWindowUsingConfiguration:forExtensionContext:completionHandler:))]
        fn open_new_window(
            &self,
            _controller: &WKWebExtensionController,
            configuration: &WKWebExtensionWindowConfiguration,
            _context: &WKWebExtensionContext,
            completion: &block2::DynBlock<
                dyn Fn(*mut ProtocolObject<dyn WKWebExtensionWindow>, *mut NSError),
            >,
        ) {
            let Some(shared) = self.ivars().shared.upgrade() else {
                completion.call((ptr::null_mut(), ptr::null_mut()));
                return;
            };
            // There's one window, so a new window's tabs open in it.
            // SAFETY: a plain read.
            let urls = unsafe { configuration.tabURLs() };
            if urls.is_empty() {
                shared.emit(ExtensionEvent::OpenTab {
                    url: String::new(),
                    active: true,
                    request: None,
                });
            }
            for url in &urls {
                if let Some(url) = url.absoluteString() {
                    shared.emit(ExtensionEvent::OpenTab {
                        url: url.to_string(),
                        active: true,
                        request: None,
                    });
                }
            }
            let window = Retained::as_ptr(&shared.window).cast_mut().cast();
            completion.call((window, ptr::null_mut()));
        }

        #[unsafe(method(webExtensionController:promptForPermissions:inTab:forExtensionContext:completionHandler:))]
        fn prompt_for_permissions(
            &self,
            _controller: &WKWebExtensionController,
            permissions: &NSSet<WKWebExtensionPermission>,
            _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
            _context: &WKWebExtensionContext,
            completion: &block2::DynBlock<
                dyn Fn(ptr::NonNull<NSSet<WKWebExtensionPermission>>, *mut NSDate),
            >,
        ) {
            completion.call((ptr::NonNull::from(permissions), ptr::null_mut()));
        }

        #[unsafe(method(webExtensionController:promptForPermissionToAccessURLs:inTab:forExtensionContext:completionHandler:))]
        fn prompt_for_urls(
            &self,
            _controller: &WKWebExtensionController,
            urls: &NSSet<NSURL>,
            _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
            _context: &WKWebExtensionContext,
            completion: &block2::DynBlock<dyn Fn(ptr::NonNull<NSSet<NSURL>>, *mut NSDate)>,
        ) {
            completion.call((ptr::NonNull::from(urls), ptr::null_mut()));
        }

        #[unsafe(method(webExtensionController:promptForPermissionMatchPatterns:inTab:forExtensionContext:completionHandler:))]
        fn prompt_for_match_patterns(
            &self,
            _controller: &WKWebExtensionController,
            patterns: &NSSet<WKWebExtensionMatchPattern>,
            _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
            _context: &WKWebExtensionContext,
            completion: &block2::DynBlock<
                dyn Fn(ptr::NonNull<NSSet<WKWebExtensionMatchPattern>>, *mut NSDate),
            >,
        ) {
            completion.call((ptr::NonNull::from(patterns), ptr::null_mut()));
        }

        #[unsafe(method(webExtensionController:didUpdateAction:forExtensionContext:))]
        fn did_update_action(
            &self,
            _controller: &WKWebExtensionController,
            action: &WKWebExtensionAction,
            context: &WKWebExtensionContext,
        ) {
            let Some(shared) = self.ivars().shared.upgrade() else {
                return;
            };
            if let Some(id) = shared.id_of(context) {
                // SAFETY: plain reads on the action WebKit passes.
                let (icon, tab) = unsafe {
                    (
                        action.iconForSize(NSSize::new(ICON_POINTS, ICON_POINTS)),
                        action.associatedTab(),
                    )
                };
                let tab_id = tab.and_then(|tab| {
                    let tab = Retained::as_ptr(&tab).cast::<ExtensionTab>();
                    let state = shared.state.borrow();
                    state
                        .tabs
                        .iter()
                        .find(|t| ptr::eq(Retained::as_ptr(t), tab))
                        .map(|t| t.ivars().id)
                });
                shared.with_entry(&id, |entry| entry.action_updated(tab_id, icon.as_deref()));
            }
            shared.emit(ExtensionEvent::ActionChanged);
        }

        /// `runtime.sendNativeMessage`: there is no native host to run, but
        /// Vamprowser's own bridge scripts (uBlock Origin's reports its
        /// filter lists) talk to the browser this way.
        #[unsafe(method(webExtensionController:sendMessage:toApplicationWithIdentifier:forExtensionContext:replyHandler:))]
        fn send_native_message(
            &self,
            _controller: &WKWebExtensionController,
            message: &AnyObject,
            application: Option<&NSString>,
            context: &WKWebExtensionContext,
            reply: &block2::DynBlock<dyn Fn(*mut AnyObject, *mut NSError)>,
        ) {
            if let Some(shared) = self.ivars().shared.upgrade()
                && let Some(extension_id) = shared.id_of(context)
                && let Some(text) = message.downcast_ref::<NSString>()
            {
                shared.emit(ExtensionEvent::NativeMessage {
                    extension_id,
                    application: application.map(|a| a.to_string()).unwrap_or_default(),
                    message: text.to_string(),
                });
            }
            reply.call((ptr::null_mut(), ptr::null_mut()));
        }

        #[unsafe(method(webExtensionController:presentPopupForAction:forExtensionContext:completionHandler:))]
        fn present_popup(
            &self,
            _controller: &WKWebExtensionController,
            action: &WKWebExtensionAction,
            _context: &WKWebExtensionContext,
            completion: &ErrorCompletion,
        ) {
            if let Some(shared) = self.ivars().shared.upgrade() {
                present_popup(&shared, action);
            }
            succeed(completion);
        }
    }
);

impl ControllerDelegate {
    pub(super) fn new(mtm: MainThreadMarker, shared: Weak<Shared>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { shared });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Shows an action's popup in WebKit's ready-made popover, pointing at the
/// button that was clicked — or, when the extension opened it itself, at
/// the top right of the page.
fn present_popup(shared: &Rc<Shared>, action: &WKWebExtensionAction) {
    // SAFETY: a plain read; WebKit builds the popover on first use.
    let Some(popover) = (unsafe { action.popupPopover() }) else {
        return;
    };
    let anchor = shared.state.borrow_mut().popup_anchor.take();
    let (rect, view) = match anchor {
        Some(anchor) => anchor,
        None => {
            let Some(tab) = shared.active_tab() else {
                return;
            };
            let view: Retained<NSView> = Retained::into_super(tab.ivars().webview.clone());
            let width = view.bounds().size.width;
            let corner = NSRect::new(NSPoint::new(width - 48.0, 0.0), NSSize::new(32.0, 1.0));
            (corner, view)
        }
    };
    // Showing from a view outside any window raises an exception.
    if view.window().is_none() {
        return;
    }
    // The anchor is top-left-origin; AppKit views are usually bottom-left.
    let flipped = view.isFlipped();
    let rect = if flipped {
        rect
    } else {
        let height = view.bounds().size.height;
        NSRect::new(
            NSPoint::new(rect.origin.x, height - rect.origin.y - rect.size.height),
            rect.size,
        )
    };
    let below = if flipped {
        NSRectEdge::MaxY
    } else {
        NSRectEdge::MinY
    };
    popover.showRelativeToRect_ofView_preferredEdge(rect, &view, below);
    watch_popup(shared, action, &popover);
    // SAFETY: a plain read of the popover's page.
    if let Some(page) = unsafe { action.popupWebView() } {
        fit_popup(&popover, &page);
    }
}

/// Notes which extension's popup is showing, and when it closes, so its
/// toolbar button can toggle it.
fn watch_popup(shared: &Rc<Shared>, action: &WKWebExtensionAction, popover: &NSPopover) {
    // SAFETY: a plain read.
    let Some(id) = (unsafe { action.webExtensionContext() }).and_then(|c| shared.id_of(&c)) else {
        return;
    };
    let center = NSNotificationCenter::defaultCenter();
    let weak = Rc::downgrade(shared);
    let closed_id = id.clone();
    let on_close = RcBlock::new(move |_note: std::ptr::NonNull<NSNotification>| {
        let Some(shared) = weak.upgrade() else {
            return;
        };
        let observer = {
            let mut state = shared.state.borrow_mut();
            state.popup_open = None;
            state.popup_closed = Some((closed_id.clone(), Instant::now()));
            state.popup_observer.take()
        };
        // A popover closes once; WebKit makes a new one the next time.
        if let Some(observer) = observer {
            // SAFETY: a token the default centre returned; removing an
            // observer from inside its own block is allowed.
            unsafe { NSNotificationCenter::defaultCenter().removeObserver(observer.as_ref()) };
        }
    });
    // SAFETY: a name AppKit defines, our popover as the object, and a block
    // of the documented type, run on the posting (main) thread.
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSPopoverDidCloseNotification),
            Some(popover),
            None,
            &on_close,
        )
    };
    let mut state = shared.state.borrow_mut();
    if let Some(previous) = state.popup_observer.replace(observer) {
        // SAFETY: a token this centre returned.
        unsafe { center.removeObserver(previous.as_ref()) };
    }
    state.popup_open = Some(id);
}

/// Popup pages size themselves in CSS, but WebKit's popover measures the
/// page before its styles apply (Proton Pass's opens a sliver tall). Measure
/// the page a few times as it loads, until it holds still, sizing the
/// popover to it within the 800×600 that browsers allow popups.
fn fit_popup(popover: &NSPopover, page: &WKWebView) {
    const MEASURE: &str = "(() => { const d = document.documentElement, b = document.body; \
        const w = Math.max(d.scrollWidth, b ? b.scrollWidth : 0, d.offsetWidth); \
        const h = Math.max(d.scrollHeight, b ? b.scrollHeight : 0, d.offsetHeight); \
        return w + 'x' + h; })()";
    let popover = popover.retain();
    let page = page.retain();
    let measure = NSString::from_str(MEASURE);
    let remaining = std::cell::Cell::new(12u32);
    // The last measurement, and whether the next matched it after the page
    // finished loading: then it has settled, and measuring stops early.
    let last = Rc::new(RefCell::new(String::new()));
    let settled = Rc::new(std::cell::Cell::new(false));
    let tick = RcBlock::new(move |timer: std::ptr::NonNull<NSTimer>| {
        let left = remaining.get();
        remaining.set(left.saturating_sub(1));
        // SAFETY: the timer is live for the duration of its own callback.
        if left == 0 || settled.get() || !popover.isShown() {
            unsafe { timer.as_ref() }.invalidate();
            return;
        }
        let popover = popover.clone();
        let (last, settled) = (last.clone(), settled.clone());
        let loading_page = page.clone();
        let measured = RcBlock::new(move |result: *mut AnyObject, _error: *mut NSError| {
            // SAFETY: WebKit passes the script's value, or null.
            let Some(text) = (unsafe { result.as_ref() })
                .and_then(|value| value.downcast_ref::<NSString>())
                .map(|text| text.to_string())
            else {
                return;
            };
            // SAFETY: a plain read on the popup's web view.
            let loading = unsafe { loading_page.isLoading() };
            if *last.borrow() == text && !loading {
                settled.set(true);
            }
            last.replace(text.clone());
            let Some((w, h)) = text.split_once('x') else {
                return;
            };
            let (Ok(w), Ok(h)) = (w.parse::<f64>(), h.parse::<f64>()) else {
                return;
            };
            if w < 40.0 || h < 40.0 {
                return;
            }
            let size = NSSize::new(w.min(800.0), h.min(600.0));
            let current = popover.contentSize();
            if (current.width - size.width).abs() > 1.0
                || (current.height - size.height).abs() > 1.0
            {
                popover.setContentSize(size);
            }
        });
        // SAFETY: a script string and a block of the documented type.
        unsafe { page.evaluateJavaScript_completionHandler(&measure, Some(&measured)) };
    });
    // SAFETY: a repeating main-run-loop timer with a block of the
    // documented type; it invalidates itself when done.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.25, true, &tick);
    }
}
