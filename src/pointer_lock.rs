//! Let pages capture the pointer for mouse-driven games. WebKit denies
//! pointer lock when its UI delegate does not answer the request, and wry's
//! macOS delegate currently has no handler for it.

use std::sync::OnceLock;

use block2::Block;
use objc2::{
    ffi,
    runtime::{AnyClass, AnyObject, Bool, Imp, Sel},
    sel,
};
use objc2_web_kit::WKWebView;

unsafe extern "C-unwind" fn allow(
    _delegate: &AnyObject,
    _cmd: Sel,
    _webview: &WKWebView,
    completion: &Block<dyn Fn(Bool)>,
) {
    // WebKit checks that the request came from an eligible page gesture.
    completion.call((Bool::YES,));
}

/// Add WebKit's pointer-lock callback to wry's UI delegate. The class exists
/// only after wry has built the first web view. Reassigning the delegate is
/// necessary because WebKit caches which optional callbacks it implements.
pub fn enable(webview: &WKWebView) {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    let installed = *INSTALLED.get_or_init(|| {
        let Some(class) = AnyClass::classes().iter().copied().find(|class| {
            class
                .name()
                .to_str()
                .is_ok_and(|name| name.contains("WryWebViewUIDelegate"))
        }) else {
            eprintln!("Couldn't find wry's UI delegate; pointer lock is unavailable.");
            return false;
        };
        let selector = sel!(_webViewDidRequestPointerLock:completionHandler:);
        type Allow = unsafe extern "C-unwind" fn(&AnyObject, Sel, &WKWebView, &Block<dyn Fn(Bool)>);
        // SAFETY: the method receives a web view and a BOOL completion block.
        // The Objective-C encoding is void, self, selector, object, object.
        unsafe {
            let imp: Imp = std::mem::transmute::<Allow, Imp>(allow);
            ffi::class_addMethod(
                class as *const AnyClass as *mut AnyClass,
                selector,
                imp,
                c"v@:@@".as_ptr(),
            )
            .as_bool()
        }
    });
    if installed {
        // SAFETY: wry retains the delegate; passing the same live instance
        // back to WebKit updates its optional-method cache.
        unsafe {
            let delegate = webview.UIDelegate();
            webview.setUIDelegate(delegate.as_deref());
        }
    }
}
