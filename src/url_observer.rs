//! Report changes to WebKit's own URL, including History API navigation.

use std::{ffi::c_void, ptr};

use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, rc::Retained,
    runtime::AnyObject,
};
use objc2_foundation::{
    NSDictionary, NSKeyValueChangeKey, NSKeyValueObservingOptions, NSObject,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSString,
};
use objc2_web_kit::WKWebView;

pub(crate) struct UrlIvars {
    view: Retained<WKWebView>,
    key: Retained<NSString>,
    changed: Box<dyn Fn()>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. Drop unregisters
    // before the retained web view can be released.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VamprowserUrlObserver"]
    #[ivars = UrlIvars]
    pub(crate) struct UrlObserver;

    impl UrlObserver {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            _key: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut c_void,
        ) {
            (self.ivars().changed)();
        }
    }

    unsafe impl NSObjectProtocol for UrlObserver {}
);

impl UrlObserver {
    pub fn new(view: &WKWebView, changed: impl Fn() + 'static) -> Retained<Self> {
        let key = NSString::from_str("URL");
        let observer = Self::alloc(MainThreadMarker::new().expect("URL observer on main thread"))
            .set_ivars(UrlIvars {
                view: view.retain(),
                key,
                changed: Box::new(changed),
            });
        let observer: Retained<Self> = unsafe { msg_send![super(observer), init] };
        // SAFETY: the observer owns the web view and unregisters in Drop.
        unsafe {
            observer
                .ivars()
                .view
                .addObserver_forKeyPath_options_context(
                    &observer,
                    &observer.ivars().key,
                    NSKeyValueObservingOptions::New,
                    ptr::null_mut(),
                );
        }
        observer
    }
}

impl Drop for UrlObserver {
    fn drop(&mut self) {
        // SAFETY: this removes the registration created in new while the
        // retained web view is still alive.
        unsafe {
            self.ivars()
                .view
                .removeObserver_forKeyPath(self, &self.ivars().key)
        };
    }
}
