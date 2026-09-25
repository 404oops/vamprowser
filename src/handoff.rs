//! Handoff: the page in front is offered to your other Apple devices, which
//! can pick it up in their browser. Private tabs and the browser's own
//! pages are never offered.

use objc2::{AllocAnyThread, rc::Retained};
use objc2_foundation::{NSString, NSURL, NSUserActivity};

/// Apple's activity type for a web page open in a browser.
const BROWSING_WEB: &str = "NSUserActivityTypeBrowsingWeb";

#[derive(Default)]
pub(crate) struct Handoff {
    activity: Option<Retained<NSUserActivity>>,
    offered: String,
    title: String,
}

impl Handoff {
    /// Offers `url` (titled `title`), replacing whatever was offered.
    pub(crate) fn offer(&mut self, url: &str, title: &str) {
        // Called on every page update, which mostly changes neither.
        if self.activity.is_some() && self.offered == url && self.title == title {
            return;
        }
        let web = url.starts_with("https://") || url.starts_with("http://");
        if !web {
            self.withdraw();
            return;
        }
        let address = if self.offered == url {
            None
        } else {
            let Some(address) = NSURL::URLWithString(&NSString::from_str(url)) else {
                self.withdraw();
                return;
            };
            Some(address)
        };
        let activity = self.activity.get_or_insert_with(|| {
            let activity = NSUserActivity::initWithActivityType(
                NSUserActivity::alloc(),
                &NSString::from_str(BROWSING_WEB),
            );
            activity.setEligibleForHandoff(true);
            activity
        });
        if let Some(address) = address {
            activity.setWebpageURL(Some(&address));
            self.offered = url.to_owned();
        }
        activity.setTitle(Some(&NSString::from_str(title)));
        self.title = title.to_owned();
        activity.becomeCurrent();
    }

    /// Stops offering anything, as for a private tab.
    pub(crate) fn withdraw(&mut self) {
        if let Some(activity) = self.activity.take() {
            activity.invalidate();
        }
        self.offered.clear();
        self.title.clear();
    }
}
