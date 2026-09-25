//! WebKit's per-page audio controls. These selectors are WebKit SPI: the
//! public WKWebView API exposes playback state, but not tab audio muting.

use std::{ffi::c_void, ptr};

use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, rc::Retained,
    runtime::AnyObject,
};
use objc2_foundation::{
    NSDictionary, NSKeyValueChangeKey, NSKeyValueChangeNewKey, NSKeyValueObservingOptions,
    NSNumber, NSObject, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSString,
};
use objc2_web_kit::WKWebView;

const AUDIO_MUTED: usize = 1;

pub(crate) struct AudioIvars {
    view: Retained<WKWebView>,
    key: Retained<NSString>,
    changed: Box<dyn Fn(bool)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. Drop unregisters
    // before either this observer or its web view can be released.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VamprowserAudioObserver"]
    #[ivars = AudioIvars]
    pub(crate) struct AudioObserver;

    impl AudioObserver {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            _key: Option<&NSString>,
            _object: Option<&AnyObject>,
            change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut c_void,
        ) {
            let playing = change
                .and_then(|change| change.objectForKey(unsafe { NSKeyValueChangeNewKey }))
                .and_then(|value| value.downcast::<NSNumber>().ok())
                .is_some_and(|value| value.boolValue());
            (self.ivars().changed)(playing);
        }
    }

    unsafe impl NSObjectProtocol for AudioObserver {}
);

impl AudioObserver {
    pub fn new(view: &WKWebView, changed: impl Fn(bool) + 'static) -> Retained<Self> {
        let key = NSString::from_str("_isPlayingAudio");
        let observer = Self::alloc(MainThreadMarker::new().expect("audio observer on main thread"))
            .set_ivars(AudioIvars {
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

impl Drop for AudioObserver {
    fn drop(&mut self) {
        // SAFETY: this is the same registration created in new, and the
        // retained web view is still alive during this destructor.
        unsafe {
            self.ivars()
                .view
                .removeObserver_forKeyPath(self, &self.ivars().key)
        };
    }
}

/// Mute only page audio, leaving camera, microphone and screen capture state.
pub fn set_muted(view: &WKWebView, muted: bool) {
    // SAFETY: these selectors take and return NS_OPTIONS(NSUInteger).
    unsafe {
        let state: usize = msg_send![view, _mediaMutedState];
        let next = if muted {
            state | AUDIO_MUTED
        } else {
            state & !AUDIO_MUTED
        };
        let _: () = msg_send![view, _setPageMuted: next];
    }
}

/// Hold a newly opened background page's media until the tab is selected.
pub fn set_suspended(view: &WKWebView, suspended: bool) {
    // SAFETY: public WKWebView API; the caller pairs suspension with resume
    // when the same view is first selected.
    unsafe {
        view.setAllMediaPlaybackSuspended_completionHandler(suspended, None);
    }
}
