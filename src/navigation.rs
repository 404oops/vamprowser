//! When a response becomes a download. Wry downloads any response WebKit
//! says it can't show, frames inside a page included, and ignores what the
//! server asked for; a page could then arrive as a file (YouTube's did).
//! This puts a browser's rules in its place: attachments download, what
//! WebKit can show is shown, and the page itself downloads only when it
//! can't be shown.
//!
//! It also opens links clicked with ⌘ (or the middle button) in a new
//! tab, as WebKit leaves that to the browser and wry doesn't do it: the
//! page would otherwise just follow the link.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_char,
    sync::OnceLock,
};

use block2::{Block, RcBlock};
use objc2::{
    ffi, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};
use objc2_foundation::{NSHTTPURLResponse, NSString, NSURLResponse, NSURLAuthenticationChallenge, NSURLCredential, NSURLCredentialPersistence};
use objc2_web_kit::{WKNavigationResponse, WKNavigationResponsePolicy, WKWebView};

/// What to do with `response`.
fn policy(response: &WKNavigationResponse) -> WKNavigationResponsePolicy {
    // SAFETY: plain property reads on the response WebKit handed us.
    let (main_frame, showable) = unsafe { (response.isForMainFrame(), response.canShowMIMEType()) };
    // No response to speak of: nothing to save. WebKit hands these over for
    // YouTube's page before the real one, and wry saved them as files.
    let Some(answer) = url_response(response) else {
        return WKNavigationResponsePolicy::Allow;
    };
    // The server asked for it to be saved.
    let attachment = answer
        .downcast_ref::<NSHTTPURLResponse>()
        .and_then(|http| http.valueForHTTPHeaderField(&NSString::from_str("Content-Disposition")))
        .is_some_and(|value| value.to_string().trim_start().to_ascii_lowercase().starts_with("attachment"));
    if attachment {
        return WKNavigationResponsePolicy::Download;
    }
    if showable {
        return WKNavigationResponsePolicy::Allow;
    }
    // Something WebKit can't show: saved if it's the page itself, left alone
    // in a frame inside one.
    if main_frame {
        WKNavigationResponsePolicy::Download
    } else {
        WKNavigationResponsePolicy::Allow
    }
}

/// The response itself, which WebKit sometimes doesn't have (the binding
/// assumes it always does).
fn url_response(response: &WKNavigationResponse) -> Option<Retained<NSURLResponse>> {
    // SAFETY: `response` returns an NSURLResponse or nil.
    unsafe { msg_send![response, response] }
}

unsafe extern "C-unwind" fn decide(
    _this: &AnyObject,
    _cmd: Sel,
    _webview: &WKWebView,
    response: &WKNavigationResponse,
    handler: &Block<dyn Fn(WKNavigationResponsePolicy)>,
) {
    let chosen = policy(response);
    handler.call((chosen,));
}

/// What each web view does with a link clicked to open in a new tab: the
/// address, and whether the tab comes to the front (⇧ held too).
type Opener = Box<dyn Fn(String, bool)>;

/// A page that couldn't load: its address, and the error's domain, code
/// and description.
#[derive(Debug)]
pub struct LoadFailure {
    pub url: String,
    pub domain: String,
    pub code: isize,
    pub description: String,
}

type FailureHandler = Box<dyn Fn(LoadFailure)>;
type AuthHandler = Box<dyn Fn(AuthChallenge)>;
type AuthCompletion = RcBlock<dyn Fn(isize, *mut AnyObject)>;

/// A protection space in one browser session. Scope 0 is ordinary browsing;
/// private windows use their window serial and are forgotten on close.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AuthSpace {
    scope: u64,
    host: String,
    port: isize,
    scheme: String,
    realm: String,
    method: String,
}

struct AuthRegistration {
    scope: u64,
    prompt: AuthHandler,
}

struct PendingAuth {
    completion: AuthCompletion,
    space: AuthSpace,
}

#[derive(Debug)]
pub struct AuthChallenge {
    pub host: String,
    pub port: isize,
    pub scheme: String,
    pub realm: String,
    pub failed: bool,
}

thread_local! {
    /// By web view address.
    static OPENERS: RefCell<HashMap<usize, Opener>> = RefCell::default();
    /// By web view address.
    static FAILURES: RefCell<HashMap<usize, FailureHandler>> = RefCell::default();
    static AUTH_HANDLERS: RefCell<HashMap<usize, AuthRegistration>> = RefCell::default();
    static AUTH_PENDING: RefCell<HashMap<usize, PendingAuth>> = RefCell::default();
    /// Browser-owned credentials, never written to Keychain or session files.
    static AUTH_SESSION: RefCell<HashMap<AuthSpace, (String, String)>> = RefCell::default();
    /// Whether the navigation wry is asking its handler about is the
    /// page itself loading an address (not a frame in it, not a form),
    /// set around the call; see [`page_load`].
    static PAGE_LOAD: Cell<bool> = const { Cell::new(true) };
    static MAIN_FRAME: Cell<bool> = const { Cell::new(true) };
}

/// Whether the navigation being decided, from inside wry's navigation
/// handler, is the page itself going to an address with a plain GET: the
/// only kind worth loading again over HTTPS.
pub fn page_load() -> bool {
    PAGE_LOAD.with(Cell::get)
}

pub fn main_frame_load() -> bool {
    MAIN_FRAME.with(Cell::get)
}

pub enum HttpsAction {
    Allow,
    Upgrade(String),
    Block(&'static str),
}

/// The navigation policy for HTTPS-only mode. POSTs and frames cannot be
/// safely replayed as a new GET, so they are stopped.
pub fn https_only_action(url: &str, main_frame: bool, get: bool, last: &mut Option<String>) -> HttpsAction {
    let Ok(mut parsed) = url::Url::parse(url) else { return HttpsAction::Allow };
    if parsed.scheme() != "http" || crate::settings::is_local(&parsed) {
        return HttpsAction::Allow;
    }
    if !main_frame {
        return HttpsAction::Block("HTTPS-only blocked an HTTP frame.");
    }
    if !get {
        return HttpsAction::Block("HTTPS-only blocked an HTTP form submission.");
    }
    if parsed.set_scheme("https").is_err() {
        return HttpsAction::Block("HTTPS-only blocked an HTTP address.");
    }
    let secure: String = parsed.into();
    if last.as_deref() == Some(secure.as_str()) {
        HttpsAction::Block("HTTPS-only blocked a redirect back to HTTP.")
    } else {
        *last = Some(secure.clone());
        HttpsAction::Upgrade(secure)
    }
}

fn is_main_frame(action: &AnyObject) -> bool {
    unsafe {
        let frame: Option<Retained<AnyObject>> = msg_send![action, targetFrame];
        frame.is_some_and(|frame| msg_send![&*frame, isMainFrame])
    }
}

/// Whether `action` loads the page itself with a GET.
fn is_page_load(action: &AnyObject) -> bool {
    // SAFETY: plain property reads; the target frame is nil for a new
    // window, and the request's method may be nil.
    unsafe {
        let request: Option<Retained<AnyObject>> = msg_send![action, request];
        let method: Option<Retained<NSString>> = request.and_then(|r| msg_send![&*r, HTTPMethod]);
        is_main_frame(action) && method.is_none_or(|m| m.to_string().eq_ignore_ascii_case("GET"))
    }
}

/// Wry's own navigation-action method, which every other navigation still
/// goes through.
static WRY_DECIDE_ACTION: OnceLock<Imp> = OnceLock::new();

type DecideAction = unsafe extern "C-unwind" fn(&AnyObject, Sel, &WKWebView, &AnyObject, &Block<dyn Fn(isize)>);

/// Has `webview` hand links clicked for a new tab to `open`.
pub fn open_new_tabs_with(webview: &WKWebView, open: impl Fn(String, bool) + 'static) {
    let key = webview as *const WKWebView as usize;
    OPENERS.with(|openers| openers.borrow_mut().insert(key, Box::new(open)));
}

/// Has `webview` report pages that fail to load to `failed`: wry leaves
/// them blank, saying nothing.
pub fn on_failure(webview: &WKWebView, failed: impl Fn(LoadFailure) + 'static) {
    let key = webview as *const WKWebView as usize;
    FAILURES.with(|failures| failures.borrow_mut().insert(key, Box::new(failed)));
    // WebKit notes which of its delegate's methods exist when the delegate
    // is set; the first web view's was set before `install` added ours.
    // SAFETY: the delegate wry set, set again.
    unsafe {
        let delegate = webview.navigationDelegate();
        webview.setNavigationDelegate(delegate.as_deref());
    }
}

pub fn on_authentication(webview: &WKWebView, scope: u64, prompt: impl Fn(AuthChallenge) + 'static) {
    let key = webview as *const WKWebView as usize;
    AUTH_HANDLERS.with(|handlers| handlers.borrow_mut().insert(key, AuthRegistration {
        scope, prompt: Box::new(prompt),
    }));
    unsafe {
        let delegate = webview.navigationDelegate();
        // WebKit may cache optional delegate methods when assigning it. The
        // auth method was added after Wry created this view, so reassigning
        // the same object alone may leave the old capability cache in place.
        webview.setNavigationDelegate(None);
        webview.setNavigationDelegate(delegate.as_deref());
    }
}

fn session_credential(space: &AuthSpace, failed: bool) -> Option<(String, String)> {
    AUTH_SESSION.with(|session| {
        let mut session = session.borrow_mut();
        if failed {
            session.remove(space);
            None
        } else {
            session.get(space).cloned()
        }
    })
}

pub fn answer_auth(webview: &WKWebView, user: &str, password: &str) -> bool {
    let key = webview as *const WKWebView as usize;
    let pending = AUTH_PENDING.with(|pending| pending.borrow_mut().remove(&key));
    let Some(pending) = pending else { return false };
    AUTH_SESSION.with(|session| session.borrow_mut().insert(
        pending.space, (user.to_owned(), password.to_owned()),
    ));
    let credential = NSURLCredential::credentialWithUser_password_persistence(
        &NSString::from_str(user), &NSString::from_str(password), NSURLCredentialPersistence::None,
    );
    pending.completion.call((0, Retained::as_ptr(&credential) as *mut AnyObject));
    true
}

pub fn cancel_auth(webview: &WKWebView) {
    let key = webview as *const WKWebView as usize;
    if let Some(pending) = AUTH_PENDING.with(|pending| pending.borrow_mut().remove(&key)) {
        pending.completion.call((2, std::ptr::null_mut()));
    }
}

pub fn forget_private_auth(scope: u64) {
    AUTH_SESSION.with(|session| session.borrow_mut().retain(|space, _| space.scope != scope));
}

unsafe extern "C-unwind" fn did_receive_auth(
    _this: &AnyObject, _cmd: Sel, webview: &WKWebView,
    challenge: &NSURLAuthenticationChallenge,
    handler: &Block<dyn Fn(isize, *mut AnyObject)>,
) {
    let space = challenge.protectionSpace();
    let method = space.authenticationMethod().to_string();
    if space.isProxy() || (method != "NSURLAuthenticationMethodHTTPBasic" && method != "NSURLAuthenticationMethodHTTPDigest") {
        handler.call((1, std::ptr::null_mut()));
        return;
    }
    let key = webview as *const WKWebView as usize;
    let scope = AUTH_HANDLERS.with(|handlers| handlers.borrow().get(&key).map(|entry| entry.scope));
    let Some(scope) = scope else {
        // Never fall back to WebKit's default credential lookup for HTTP auth.
        handler.call((2, std::ptr::null_mut()));
        return;
    };
    cancel_auth(webview);
    let space_key = AuthSpace {
        scope,
        host: space.host().to_string().to_ascii_lowercase(),
        port: space.port(),
        scheme: space.protocol().map_or_else(|| "https".into(), |s| s.to_string().to_ascii_lowercase()),
        realm: space.realm().map_or_else(String::new, |s| s.to_string()),
        method,
    };
    let saved = session_credential(&space_key, challenge.previousFailureCount() > 0);
    if let Some((user, password)) = saved {
        let credential = NSURLCredential::credentialWithUser_password_persistence(
            &NSString::from_str(&user), &NSString::from_str(&password), NSURLCredentialPersistence::None,
        );
        handler.call((0, Retained::as_ptr(&credential) as *mut AnyObject));
        return;
    }
    AUTH_PENDING.with(|pending| pending.borrow_mut().insert(key, PendingAuth {
        completion: handler.copy(), space: space_key,
    }));
    let prompt = AuthChallenge {
        host: space.host().to_string(),
        port: space.port(),
        scheme: space.protocol().map_or_else(|| "https".into(), |s| s.to_string()),
        realm: space.realm().map_or_else(String::new, |s| s.to_string()),
        failed: challenge.previousFailureCount() > 0,
    };
    AUTH_HANDLERS.with(|handlers| {
        if let Some(entry) = handlers.borrow().get(&key) { (entry.prompt)(prompt); }
    });
}

/// Forgets a web view going away.
pub fn forget(webview: &WKWebView) {
    let key = webview as *const WKWebView as usize;
    OPENERS.with(|openers| openers.borrow_mut().remove(&key));
    FAILURES.with(|failures| failures.borrow_mut().remove(&key));
    AUTH_HANDLERS.with(|handlers| handlers.borrow_mut().remove(&key));
    cancel_auth(webview);
}

unsafe extern "C-unwind" fn did_fail(
    _this: &AnyObject,
    _cmd: Sel,
    webview: &WKWebView,
    _navigation: *mut AnyObject,
    error: &objc2_foundation::NSError,
) {
    let key = webview as *const WKWebView as usize;
    let url = {
        // SAFETY: the error's user info, and the failing URL WebKit files
        // in it (an NSURL), or nil.
        let url: Option<Retained<objc2_foundation::NSURL>> = unsafe {
            let info = error.userInfo();
            msg_send![&*info, objectForKey: &*NSString::from_str("NSErrorFailingURLKey")]
        };
        url.and_then(|url| url.absoluteString()).map(|s| s.to_string()).unwrap_or_default()
    };
    let failure = LoadFailure {
        url,
        domain: error.domain().to_string(),
        code: error.code(),
        description: error.localizedDescription().to_string(),
    };
    FAILURES.with(|failures| {
        if let Some(failed) = failures.borrow().get(&key) {
            failed(failure);
        }
    });
}

/// A link clicked for a new tab: ⌘ or the middle button, and whether ⇧
/// was held; `None` for any other navigation.
fn new_tab_click(action: &AnyObject) -> Option<(String, bool)> {
    use objc2_app_kit::NSEventModifierFlags;
    // SAFETY: plain property reads on the navigation action WebKit handed
    // us; `request` and its URL may be nil.
    unsafe {
        let kind: isize = msg_send![action, navigationType];
        // WKNavigationTypeLinkActivated.
        if kind != 0 {
            return None;
        }
        let flags: NSEventModifierFlags = msg_send![action, modifierFlags];
        let button: isize = msg_send![action, buttonNumber];
        // WebKit numbers the middle button 4; 2 on some versions.
        let middle = button == 4 || button == 2;
        if !flags.contains(NSEventModifierFlags::Command) && !middle {
            return None;
        }
        let request: Option<Retained<AnyObject>> = msg_send![action, request];
        let url: Option<Retained<objc2_foundation::NSURL>> = msg_send![&*request?, URL];
        let url = url?.absoluteString()?.to_string();
        Some((url, flags.contains(NSEventModifierFlags::Shift)))
    }
}

unsafe extern "C-unwind" fn decide_action(
    this: &AnyObject,
    cmd: Sel,
    webview: &WKWebView,
    action: &AnyObject,
    handler: &Block<dyn Fn(isize)>,
) {
    if let Some((url, front)) = new_tab_click(action) {
        let key = webview as *const WKWebView as usize;
        let opened = OPENERS.with(|openers| {
            openers.borrow().get(&key).map(|open| open(url, front)).is_some()
        });
        if opened {
            // WKNavigationActionPolicyCancel: the page stays where it is.
            handler.call((0,));
            return;
        }
    }
    match WRY_DECIDE_ACTION.get() {
        // SAFETY: wry's method, of this very signature.
        Some(imp) => unsafe {
            let original = std::mem::transmute::<Imp, DecideAction>(*imp);
            MAIN_FRAME.with(|flag| flag.set(is_main_frame(action)));
            PAGE_LOAD.with(|flag| flag.set(is_page_load(action)));
            original(this, cmd, webview, action, handler);
            PAGE_LOAD.with(|flag| flag.set(true));
            MAIN_FRAME.with(|flag| flag.set(true));
        },
        // WKNavigationActionPolicyAllow.
        None => handler.call((1,)),
    }
}

/// Puts [`policy`] in place of wry's for every web view. Wry defines its
/// delegate class when the first web view is made, so this runs after that;
/// later calls do nothing.
pub fn install() {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(class) = AnyClass::classes()
        .iter()
        .copied()
        .find(|class| class.name().to_str().is_ok_and(|name| name.contains("WryNavigationDelegate")))
    else {
        eprintln!("Couldn't find wry's navigation delegate; downloads keep wry's rules.");
        return;
    };
    let selector = sel!(webView:decidePolicyForNavigationResponse:decisionHandler:);
    let Some(method) = class.instance_method(selector) else {
        return;
    };
    // SAFETY: the replacement has the method's exact signature (an object,
    // the selector, a web view, a navigation response and a block), and the
    // type encoding is the original's.
    unsafe {
        let types: *const c_char = ffi::method_getTypeEncoding(method);
        let imp: Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(
                &AnyObject,
                Sel,
                &WKWebView,
                &WKNavigationResponse,
                &Block<dyn Fn(WKNavigationResponsePolicy)>,
            ),
            Imp,
        >(decide);
        ffi::class_replaceMethod(class as *const AnyClass as *mut AnyClass, selector, imp, types);
    }
    // Pages that fail to load, before and after they begin to arrive:
    // wry has no methods for them, so these are added.
    type DidFail = unsafe extern "C-unwind" fn(
        &AnyObject,
        Sel,
        &WKWebView,
        *mut AnyObject,
        &objc2_foundation::NSError,
    );
    for selector in [
        sel!(webView:didFailProvisionalNavigation:withError:),
        sel!(webView:didFailNavigation:withError:),
    ] {
        // SAFETY: the method takes an object, the selector, a web view, a
        // navigation and an error, and returns nothing: "v@:@@@".
        unsafe {
            let imp: Imp = std::mem::transmute::<DidFail, Imp>(did_fail);
            ffi::class_addMethod(class as *const AnyClass as *mut AnyClass, selector, imp, c"v@:@@@".as_ptr());
        }
    }
    let selector = sel!(webView:didReceiveAuthenticationChallenge:completionHandler:);
    unsafe {
        type DidReceiveAuth = unsafe extern "C-unwind" fn(&AnyObject, Sel, &WKWebView, &NSURLAuthenticationChallenge, &Block<dyn Fn(isize, *mut AnyObject)>);
        let imp: Imp = std::mem::transmute::<DidReceiveAuth, Imp>(did_receive_auth);
        ffi::class_addMethod(class as *const AnyClass as *mut AnyClass, selector, imp, c"v@:@@@".as_ptr());
    }
    let selector = sel!(webView:decidePolicyForNavigationAction:decisionHandler:);
    let Some(method) = class.instance_method(selector) else {
        return;
    };
    // SAFETY: as above; wry's method is kept to hand everything else to,
    // and the replacement has its signature.
    unsafe {
        let types: *const c_char = ffi::method_getTypeEncoding(method);
        let imp: Imp = std::mem::transmute::<DecideAction, Imp>(decide_action);
        let previous = ffi::class_replaceMethod(class as *const AnyClass as *mut AnyClass, selector, imp, types);
        if let Some(previous) = previous {
            let _ = WRY_DECIDE_ACTION.set(previous);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_credentials_stay_in_their_protection_space_and_expire_on_failure() {
        let space = AuthSpace {
            scope: 0, host: "example.org".into(), port: 443,
            scheme: "https".into(), realm: "staff".into(),
            method: "NSURLAuthenticationMethodHTTPBasic".into(),
        };
        AUTH_SESSION.with(|session| session.borrow_mut().insert(space.clone(), ("name".into(), "secret".into())));
        assert_eq!(session_credential(&space, false), Some(("name".into(), "secret".into())));
        assert_eq!(session_credential(&AuthSpace { realm: "other".into(), ..space.clone() }, false), None);
        assert_eq!(session_credential(&AuthSpace { scope: 4, ..space.clone() }, false), None);
        assert_eq!(session_credential(&space, true), None);
        assert_eq!(session_credential(&space, false), None);
        let private = AuthSpace { scope: 4, ..space };
        AUTH_SESSION.with(|session| session.borrow_mut().insert(private.clone(), ("private".into(), "secret".into())));
        forget_private_auth(4);
        assert_eq!(session_credential(&private, false), None);
    }

    #[test]
    fn https_only_upgrades_gets_and_blocks_unsafe_http_navigation() {
        let mut last = None;
        assert!(matches!(https_only_action("http://example.org/a", true, true, &mut last),
            HttpsAction::Upgrade(url) if url == "https://example.org/a"));
        assert!(matches!(https_only_action("http://example.org/a", true, true, &mut last),
            HttpsAction::Block(_)));
        assert!(matches!(https_only_action("http://example.org/form", true, false, &mut last),
            HttpsAction::Block(_)));
        assert!(matches!(https_only_action("http://example.org/frame", false, true, &mut last),
            HttpsAction::Block(_)));
        assert!(matches!(https_only_action("http://localhost/a", true, true, &mut last),
            HttpsAction::Allow));
        assert!(matches!(https_only_action("https://example.org/a", true, true, &mut last),
            HttpsAction::Allow));
    }
}
