//! WebKit's per-page audio controls and activity observation. The audio
//! selectors are WebKit SPI; capture state and playback state are public.

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    ptr,
};

use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_foundation::{
    NSActivityOptions, NSDictionary, NSKeyValueChangeKey, NSKeyValueChangeNewKey,
    NSKeyValueObservingOptions, NSNumber, NSObject, NSObjectNSKeyValueObserverRegistration,
    NSObjectProtocol, NSProcessInfo, NSString,
};
use objc2_web_kit::{WKMediaCaptureState, WKWebView};

const AUDIO_MUTED: usize = 1;

/// A muted capture can still be part of an ongoing call.
pub fn is_capturing(view: &WKWebView) -> bool {
    // SAFETY: public WebKit state reads on the main thread.
    unsafe {
        view.cameraCaptureState() != WKMediaCaptureState::None
            || view.microphoneCaptureState() != WKMediaCaptureState::None
    }
}

pub(crate) struct AudioIvars {
    view: Retained<WKWebView>,
    audio_key: Retained<NSString>,
    camera_key: Retained<NSString>,
    microphone_key: Retained<NSString>,
    playing: Cell<bool>,
    activity: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>>,
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
            key: Option<&NSString>,
            _object: Option<&AnyObject>,
            change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut c_void,
        ) {
            if key.is_some_and(|key| key == &*self.ivars().audio_key) {
                let playing = change
                    .and_then(|change| change.objectForKey(unsafe { NSKeyValueChangeNewKey }))
                    .and_then(|value| value.downcast::<NSNumber>().ok())
                    .is_some_and(|value| value.boolValue());
                self.ivars().playing.set(playing);
                (self.ivars().changed)(playing);
            }
            self.refresh_activity();
        }
    }

    unsafe impl NSObjectProtocol for AudioObserver {}
);

impl AudioObserver {
    /// Keep the display and computer awake only while this page has live
    /// media. A muted capture still belongs to an ongoing call.
    fn refresh_activity(&self) {
        let ivars = self.ivars();
        let active = ivars.playing.get() || is_capturing(&ivars.view);
        let mut activity = ivars.activity.borrow_mut();
        if active && activity.is_none() {
            *activity = Some(
                NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                    NSActivityOptions::UserInitiated | NSActivityOptions::IdleDisplaySleepDisabled,
                    &NSString::from_str("Vamprowser page media or call in progress"),
                ),
            );
        } else if !active && let Some(token) = activity.take() {
            // SAFETY: `token` came from beginActivity on this process.
            unsafe { NSProcessInfo::processInfo().endActivity(&token) };
        }
    }

    pub fn new(view: &WKWebView, changed: impl Fn(bool) + 'static) -> Retained<Self> {
        let observer = Self::alloc(MainThreadMarker::new().expect("audio observer on main thread"))
            .set_ivars(AudioIvars {
                view: view.retain(),
                audio_key: NSString::from_str("_isPlayingAudio"),
                camera_key: NSString::from_str("cameraCaptureState"),
                microphone_key: NSString::from_str("microphoneCaptureState"),
                playing: Cell::new(false),
                activity: RefCell::new(None),
                changed: Box::new(changed),
            });
        let observer: Retained<Self> = unsafe { msg_send![super(observer), init] };
        // SAFETY: WebKit documents KVO on both capture properties. The
        // observer owns the web view and unregisters in Drop.
        unsafe {
            for key in [
                &observer.ivars().audio_key,
                &observer.ivars().camera_key,
                &observer.ivars().microphone_key,
            ] {
                observer
                    .ivars()
                    .view
                    .addObserver_forKeyPath_options_context(
                        &observer,
                        key,
                        NSKeyValueObservingOptions::New,
                        ptr::null_mut(),
                    );
            }
        }
        observer.refresh_activity();
        observer
    }
}

impl Drop for AudioObserver {
    fn drop(&mut self) {
        // SAFETY: these are the registrations created in new, and the
        // retained web view is still alive during this destructor.
        unsafe {
            for key in [
                &self.ivars().audio_key,
                &self.ivars().camera_key,
                &self.ivars().microphone_key,
            ] {
                self.ivars().view.removeObserver_forKeyPath(self, key);
            }
        };
        if let Some(token) = self.ivars().activity.borrow_mut().take() {
            // SAFETY: `token` came from beginActivity on this process.
            unsafe { NSProcessInfo::processInfo().endActivity(&token) };
        }
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
