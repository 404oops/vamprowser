//! WebKit's per-page audio controls. These selectors are WebKit SPI: the
//! public WKWebView API exposes playback state, but not tab audio muting.

use objc2::msg_send;
use objc2_web_kit::WKWebView;

const AUDIO_MUTED: usize = 1;

/// Whether WebKit reports that this page is playing audio.
pub fn playing(view: &WKWebView) -> bool {
    // SAFETY: `_isPlayingAudio` is a read-only WKWebView selector on macOS.
    unsafe { msg_send![view, _isPlayingAudio] }
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
