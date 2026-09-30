//! Tracking protection and cookie blocking, as WebKit content rule lists:
//! compiled once by WebKit, then enforced inside its network stack for every
//! tab, with no per-request work here.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use block2::RcBlock;
use objc2::{
    MainThreadMarker, Message,
    rc::{Retained, Weak},
};
use objc2_foundation::{NSError, NSString};
use objc2_web_kit::{
    WKContentRuleList, WKContentRuleListStore, WKUserContentController, WKWebView,
};
use serde_json::{Value, json};

use crate::{
    settings::{Protection, Settings},
    site_controls::SiteControls,
};

/// Analytics, session recording and cross-site tracking. Blocked when a
/// page loads them from another site, so the services' own sites still work.
const TRACKERS: &[&str] = &[
    "google-analytics.com",
    "googletagmanager.com",
    "googletagservices.com",
    "doubleclick.net",
    "connect.facebook.net",
    "scorecardresearch.com",
    "quantserve.com",
    "quantcount.com",
    "hotjar.com",
    "hotjar.io",
    "mouseflow.com",
    "fullstory.com",
    "crazyegg.com",
    "clarity.ms",
    "mixpanel.com",
    "amplitude.com",
    "segment.io",
    "cdn.segment.com",
    "heapanalytics.com",
    "heap.io",
    "nr-data.net",
    "optimizely.com",
    "chartbeat.com",
    "chartbeat.net",
    "parsely.com",
    "parse.ly",
    "comscore.com",
    "krxd.net",
    "bluekai.com",
    "demdex.net",
    "omtrdc.net",
    "everesttech.net",
    "rlcdn.com",
    "agkn.com",
    "tapad.com",
    "mathtag.com",
    "bat.bing.com",
    "analytics.twitter.com",
    "ads-twitter.com",
    "px.ads.linkedin.com",
    "snap.licdn.com",
    "analytics.tiktok.com",
    "sc-static.net",
    "analytics.yahoo.com",
    "mc.yandex.ru",
    "mc.yandex.com",
    "top-fwz1.mail.ru",
    "kissmetrics.com",
    "woopra.com",
    "statcounter.com",
    "histats.com",
    "hs-analytics.net",
    "hsadspixel.net",
    "track.hubspot.com",
    "pardot.com",
    "munchkin.marketo.net",
    "newrelic.com",
    "matomo.cloud",
    "branch.io",
    "app-measurement.com",
    "adjust.com",
    "appsflyer.com",
    "kochava.com",
    "braze.com",
    "iterable.com",
    "exponea.com",
    "pendo.io",
    "logrocket.io",
    "lr-ingest.io",
    "smartlook.com",
    "inspectlet.com",
    "luckyorange.com",
    "contentsquare.net",
    "quantummetric.com",
    "glassboxdigital.io",
    "decibelinsight.net",
];

/// Ad networks and exchanges, added in strict mode.
const ADVERTISING: &[&str] = &[
    "googlesyndication.com",
    "googleadservices.com",
    "adservice.google.com",
    "2mdn.net",
    "amazon-adsystem.com",
    "adsrvr.org",
    "adnxs.com",
    "criteo.com",
    "criteo.net",
    "taboola.com",
    "outbrain.com",
    "rubiconproject.com",
    "pubmatic.com",
    "openx.net",
    "casalemedia.com",
    "moatads.com",
    "doubleverify.com",
    "adsafeprotected.com",
    "ads.linkedin.com",
    "ads.tiktok.com",
    "turn.com",
    "yieldmo.com",
    "sharethrough.com",
    "teads.tv",
    "smartadserver.com",
    "33across.com",
    "indexww.com",
    "lijit.com",
    "sovrn.com",
    "contextweb.com",
    "gumgum.com",
    "media.net",
    "zemanta.com",
    "revcontent.com",
    "mgid.com",
    "adform.net",
    "adroll.com",
    "advertising.com",
    "bidswitch.net",
    "smaato.net",
    "inmobi.com",
    "unityads.unity3d.com",
    "applovin.com",
    "adcolony.com",
    "vungle.com",
    "yieldlove.com",
    "triplelift.com",
    "spotxchange.com",
    "springserve.com",
    "undertone.com",
    "districtm.io",
    "emxdgt.com",
    "rhythmone.com",
    "improvedigital.com",
    "adition.com",
    "yandexadexchange.net",
    "popads.net",
    "propellerads.com",
    "exoclick.com",
    "juicyads.com",
];

/// The rule list for these settings as WebKit's JSON, or `None` when there
/// is nothing to enforce.
#[cfg(test)]
pub fn rules(settings: &Settings) -> Option<String> {
    rules_for(settings, &SiteControls::default())
}

/// Builds one list for all pages. Top-URL conditions make individual site
/// choices effective without changing the rules on unrelated tabs.
pub fn rules_for(settings: &Settings, controls: &SiteControls) -> Option<String> {
    let mut rules: Vec<Value> = Vec::new();
    let mut block = |domains: &[&str], base: bool, enabled: &[String], disabled: &[String]| {
        if !base && enabled.is_empty() {
            return;
        }
        for domain in domains {
            // WebKit's rule regexes have no alternation, so one rule per
            // domain; the prefix matches the domain and its subdomains.
            let escaped = regex_escape(domain);
            let filter = format!("^https?://([^/:]*\\.)?{escaped}[/:?]");
            if base {
                let mut trigger = json!({ "url-filter": filter, "load-type": ["third-party"] });
                if !disabled.is_empty() {
                    trigger["unless-top-url"] = json!(disabled);
                }
                rules.push(json!({ "trigger": trigger, "action": { "type": "block" } }));
            }
            if !enabled.is_empty() {
                rules.push(json!({ "trigger": {
                    "url-filter": filter, "load-type": ["third-party"], "if-top-url": enabled
                }, "action": { "type": "block" } }));
            }
        }
    };
    let mut tracker_on = Vec::new();
    let mut tracker_off = Vec::new();
    let mut ads_on = Vec::new();
    let mut ads_off = Vec::new();
    let mut cookies_on = Vec::new();
    let mut cookies_off = Vec::new();
    let base_trackers = settings.protection != Protection::Off;
    let base_ads = settings.protection == Protection::Strict;
    let base_cookies = settings.block_third_party_cookies || base_ads;
    // Stable order keeps semantically identical settings from recompiling
    // when unrelated site permissions change the HashMap's iteration order.
    let mut sites: Vec<_> = controls.0.iter().collect();
    sites.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    for (host, site) in sites {
        let top = format!("^https?://{}[/:?]", regex_escape(host));
        let protection = site.protection.unwrap_or(settings.protection);
        let trackers = protection != Protection::Off;
        let ads = protection == Protection::Strict;
        let cookies = site
            .third_party_cookies
            .unwrap_or(settings.block_third_party_cookies || ads);
        for (actual, base, on, off) in [
            (trackers, base_trackers, &mut tracker_on, &mut tracker_off),
            (ads, base_ads, &mut ads_on, &mut ads_off),
            (cookies, base_cookies, &mut cookies_on, &mut cookies_off),
        ] {
            if actual != base {
                if actual {
                    on.push(top.clone());
                } else {
                    off.push(top.clone());
                }
            }
        }
    }
    block(TRACKERS, base_trackers, &tracker_on, &tracker_off);
    block(ADVERTISING, base_ads, &ads_on, &ads_off);
    drop(block);
    // Facebook's pixel is served from its own domain, so it needs a path.
    if base_trackers {
        let mut trigger = json!({ "url-filter": "^https?://([^/:]*\\.)?facebook\\.com/tr", "load-type": ["third-party"] });
        if !tracker_off.is_empty() {
            trigger["unless-top-url"] = json!(tracker_off);
        }
        rules.push(json!({ "trigger": trigger, "action": { "type": "block" } }));
    }
    if !tracker_on.is_empty() {
        rules.push(json!({ "trigger": {
            "url-filter": "^https?://([^/:]*\\.)?facebook\\.com/tr", "load-type": ["third-party"],
            "if-top-url": tracker_on
        }, "action": { "type": "block" } }));
    }
    if base_cookies {
        let mut trigger = json!({ "url-filter": ".*", "load-type": ["third-party"] });
        if !cookies_off.is_empty() {
            trigger["unless-top-url"] = json!(cookies_off);
        }
        rules.push(json!({ "trigger": trigger, "action": { "type": "block-cookies" } }));
    }
    if !cookies_on.is_empty() {
        rules.push(json!({ "trigger": {
            "url-filter": ".*", "load-type": ["third-party"], "if-top-url": cookies_on
        }, "action": { "type": "block-cookies" } }));
    }
    (!rules.is_empty()).then(|| Value::Array(rules).to_string())
}

fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if ".*+?^${}()|[]\\/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The compiled list shared by every tab, recompiled when the settings that
/// shape it change.
#[derive(Default)]
pub struct ContentRules {
    current: Rc<RefCell<Option<Retained<WKContentRuleList>>>>,
    /// The JSON the current list was compiled from, to skip no-op rebuilds.
    source: Option<String>,
    /// uBlock Origin's network filters, compiled (see `crate::filters`).
    ublock: Rc<RefCell<Vec<Retained<WKContentRuleList>>>>,
    /// Fingerprints of the lists `ublock` was built from.
    ublock_sources: Vec<String>,
    /// Which compile of each is the latest. WebKit finishes compiles in its
    /// own time, so an older one can finish after a newer one; only the
    /// latest may take effect.
    generation: Rc<Cell<u64>>,
    ublock_generation: Rc<Cell<u64>>,
    /// Only the lists currently installed in each controller. A weak
    /// controller does not keep closed tabs alive; retired lists are released
    /// when replaced instead of accumulating for the entire browser session.
    applied: RefCell<HashMap<usize, AppliedRules>>,
    /// Changes when a completed update actually replaces the active lists.
    revision: Rc<Cell<u64>>,
    /// Lists still being compiled or looked up since launch; pages wait
    /// for them before loading (see [`ContentRules::ready`]).
    outstanding: Rc<Cell<u32>>,
    /// The first list has been asked for.
    primed: bool,
    /// Pages stopped waiting, ready or not.
    gave_up: bool,
}

struct AppliedRules {
    controller: Weak<WKUserContentController>,
    revision: u64,
    lists: Vec<Retained<WKContentRuleList>>,
}

impl ContentRules {
    /// Compiles the list for `settings` if it differs from the current one,
    /// then calls `ready` on the main thread. `ready` should re-apply rules
    /// to every open tab.
    pub fn update(
        &mut self,
        settings: &Settings,
        controls: &SiteControls,
        ready: impl Fn() + 'static,
    ) {
        let source = rules_for(settings, controls);
        self.primed = true;
        if source == self.source {
            return;
        }
        self.source = source.clone();
        let current = self.current.clone();
        let revision = self.revision.clone();
        let generation = self.generation.clone();
        generation.set(generation.get() + 1);
        let mine = generation.get();
        let Some(source) = source else {
            *current.borrow_mut() = None;
            revision.set(revision.get().wrapping_add(1));
            ready();
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // SAFETY: called on the main thread, which the store requires.
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else {
            return;
        };
        let outstanding = self.outstanding.clone();
        outstanding.set(outstanding.get() + 1);
        let handler = RcBlock::new(move |list: *mut WKContentRuleList, error: *mut NSError| {
            outstanding.set(outstanding.get().saturating_sub(1));
            // SAFETY: WebKit passes either a valid list or null, retained
            // for the duration of the callback; we take our own reference.
            let list = unsafe { Retained::retain(list) };
            if list.is_none() && !error.is_null() {
                // SAFETY: non-null error pointer from WebKit.
                let error = unsafe { &*error };
                eprintln!(
                    "Could not compile content rules: {}",
                    error.localizedDescription()
                );
            }
            // Settings changed again while this compiled: the newer
            // compile's list is the one to use.
            if generation.get() != mine {
                return;
            }
            *current.borrow_mut() = list;
            revision.set(revision.get().wrapping_add(1));
            ready();
        });
        // SAFETY: arguments are valid NSStrings and a block with the
        // signature WebKit documents; it calls back on the main thread.
        unsafe {
            store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                Some(&NSString::from_str("vamprowser-protection")),
                Some(&NSString::from_str(&source)),
                Some(&handler),
            );
        }
    }

    /// Whether pages can load with every list in place: the first
    /// compile, and uBlock Origin's lists from last time, are done (or
    /// pages have waited long enough).
    pub fn ready(&self) -> bool {
        self.gave_up || (self.primed && self.outstanding.get() == 0)
    }

    /// Pages stop waiting for the lists.
    pub fn give_up_waiting(&mut self) {
        self.gave_up = true;
    }

    /// Brings back uBlock Origin's lists enforced last time, already
    /// compiled in WebKit's store, so pages are filtered from the first
    /// one at launch rather than once uBlock Origin reports in, seconds
    /// later. Calls `ready` once they're in place (or couldn't all be
    /// found: its report compiles them again).
    pub fn restore_ublock(&mut self, fingerprints: Vec<String>, ready: impl Fn() + 'static) {
        if fingerprints.is_empty() {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // SAFETY: called on the main thread, which the store requires.
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else {
            return;
        };
        // `ublock_sources` stays empty: uBlock Origin's first report looks
        // its lists up again (quick, as they're compiled) and takes over.
        let generation = self.ublock_generation.clone();
        generation.set(generation.get() + 1);
        let mine = generation.get();
        let outstanding = self.outstanding.clone();
        outstanding.set(outstanding.get() + 1);
        let found: Rc<RefCell<Vec<Option<Retained<WKContentRuleList>>>>> =
            Rc::new(RefCell::new(vec![None; fingerprints.len()]));
        let remaining = Rc::new(Cell::new(fingerprints.len()));
        let ready = Rc::new(ready);
        for (index, fingerprint) in fingerprints.iter().enumerate() {
            let (found, remaining, ready, outstanding, generation) = (
                found.clone(),
                remaining.clone(),
                ready.clone(),
                outstanding.clone(),
                generation.clone(),
            );
            let target = self.ublock.clone();
            let revision = self.revision.clone();
            let looked_up =
                RcBlock::new(move |list: *mut WKContentRuleList, _error: *mut NSError| {
                    // SAFETY: WebKit passes a valid list or null.
                    found.borrow_mut()[index] = unsafe { Retained::retain(list) };
                    remaining.set(remaining.get() - 1);
                    if remaining.get() > 0 {
                        return;
                    }
                    outstanding.set(outstanding.get().saturating_sub(1));
                    // Anything newer asked for meanwhile wins.
                    if generation.get() == mine {
                        let lists: Vec<_> = found.borrow_mut().drain(..).collect();
                        // Only if they all are: part of the filters could let
                        // through what the rest were written around.
                        if lists.iter().all(Option::is_some) {
                            *target.borrow_mut() = lists.into_iter().flatten().collect();
                            revision.set(revision.get().wrapping_add(1));
                        }
                    }
                    ready();
                });
            let identifier = NSString::from_str(&format!("{UBLOCK_PREFIX}{fingerprint}"));
            // SAFETY: a valid identifier and a block of the documented type.
            unsafe {
                store.lookUpContentRuleListForIdentifier_completionHandler(
                    Some(&identifier),
                    Some(&looked_up),
                );
            }
        }
    }

    /// Replaces a tab's rule lists with the current ones.
    pub fn apply(&self, webview: &WKWebView) {
        // SAFETY: plain property access and list mutation on a live web view,
        // on the main thread where all WebKit calls here are made.
        unsafe {
            let controller = webview.configuration().userContentController();
            let mut applied = self.applied.borrow_mut();
            let key = std::ptr::from_ref(&*controller) as usize;
            let revision = self.revision.get();
            if applied
                .get(&key)
                .is_some_and(|old| old.revision == revision && old.controller.load().is_some())
            {
                return;
            }
            // Run cleanup when a controller or rule revision changes, not
            // for every no-op application to an existing page.
            applied.retain(|_, old| old.controller.load().is_some());
            let previous = applied.remove(&key);
            for list in previous.iter().flat_map(|old| &old.lists) {
                controller.removeContentRuleList(list);
            }
            let current = self.current.borrow();
            let ublock = self.ublock.borrow();
            let mut lists = Vec::with_capacity(usize::from(current.is_some()) + ublock.len());
            for list in current.iter().chain(ublock.iter()) {
                controller.addContentRuleList(list);
                lists.push(list.clone());
            }
            applied.insert(
                key,
                AppliedRules {
                    controller: Weak::from_retained(&controller),
                    revision,
                    lists,
                },
            );
        }
    }

    /// Enforces uBlock Origin's network filters, given as WebKit rule lists
    /// (empty to stop). Lists already compiled — this launch or an earlier
    /// one — are reused by content, so a relaunch doesn't recompile tens of
    /// thousands of rules. Calls `ready` once they're in place.
    /// `fingerprints` are the chunks' [`crate::filters::fingerprint`]s,
    /// worked out off the main thread: the chunks run to megabytes.
    pub fn set_ublock(
        &mut self,
        chunks: Vec<String>,
        fingerprints: Vec<String>,
        ready: impl Fn() + 'static,
    ) {
        if fingerprints == self.ublock_sources {
            return;
        }
        self.ublock_sources = fingerprints.clone();
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // SAFETY: called on the main thread, which the store requires.
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else {
            return;
        };
        forget_stale_lists(&store, &fingerprints);
        let target = self.ublock.clone();
        let revision = self.revision.clone();
        let generation = self.ublock_generation.clone();
        generation.set(generation.get() + 1);
        let mine = generation.get();
        if chunks.is_empty() {
            target.borrow_mut().clear();
            revision.set(revision.get().wrapping_add(1));
            crate::filters::save_enforced(&[]);
            ready();
            return;
        }
        let ready = Rc::new(ready);
        let compiled: Rc<RefCell<Vec<Option<Retained<WKContentRuleList>>>>> =
            Rc::new(RefCell::new(vec![None; chunks.len()]));
        let remaining = Rc::new(std::cell::Cell::new(chunks.len()));
        let fingerprints_kept = Rc::new(fingerprints.clone());
        for (index, (chunk, fingerprint)) in chunks.into_iter().zip(fingerprints).enumerate() {
            let identifier = NSString::from_str(&format!("{UBLOCK_PREFIX}{fingerprint}"));
            let finish = {
                let compiled = compiled.clone();
                let remaining = remaining.clone();
                let target = target.clone();
                let ready = ready.clone();
                let generation = generation.clone();
                let fingerprints_kept = fingerprints_kept.clone();
                let revision = revision.clone();
                move |list: Option<Retained<WKContentRuleList>>| {
                    compiled.borrow_mut()[index] = list;
                    remaining.set(remaining.get() - 1);
                    // Newer lists were asked for meanwhile: those win.
                    if remaining.get() == 0 && generation.get() == mine {
                        let lists: Vec<_> = compiled.borrow_mut().drain(..).collect();
                        // Remembered for the next launch, if all of them
                        // are in the store to come back from.
                        if lists.iter().all(Option::is_some) {
                            crate::filters::save_enforced(&fingerprints_kept);
                        }
                        *target.borrow_mut() = lists.into_iter().flatten().collect();
                        revision.set(revision.get().wrapping_add(1));
                        ready();
                    }
                }
            };
            let store_again = store.clone();
            let identifier_again = identifier.clone();
            let looked_up = RcBlock::new(
                move |list: *mut WKContentRuleList, _error: *mut NSError| {
                    // SAFETY: WebKit passes a valid list or null.
                    if let Some(list) = unsafe { Retained::retain(list) } {
                        finish(Some(list));
                        return;
                    }
                    let finish = finish.clone();
                    let compiled_block =
                        RcBlock::new(move |list: *mut WKContentRuleList, error: *mut NSError| {
                            // SAFETY: as above; the error, if any, is valid.
                            let list = unsafe { Retained::retain(list) };
                            if list.is_none() && !error.is_null() {
                                let error = unsafe { &*error };
                                eprintln!(
                                    "Could not compile uBlock Origin's filters: {}",
                                    error.localizedDescription()
                                );
                            }
                            finish(list);
                        });
                    // SAFETY: valid strings and a block of the documented type.
                    unsafe {
                        store_again
                        .compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                            Some(&identifier_again),
                            Some(&NSString::from_str(&chunk)),
                            Some(&compiled_block),
                        );
                    }
                },
            );
            // SAFETY: a valid identifier and a block of the documented type.
            unsafe {
                store.lookUpContentRuleListForIdentifier_completionHandler(
                    Some(&identifier),
                    Some(&looked_up),
                );
            }
        }
    }
}

const UBLOCK_PREFIX: &str = "vamprowser-ublock-";

/// Deletes compiled uBlock Origin lists no longer in use, so old versions
/// of the filter lists don't pile up on disk.
fn forget_stale_lists(store: &WKContentRuleListStore, keep: &[String]) {
    let keep: Vec<String> = keep.iter().map(|f| format!("{UBLOCK_PREFIX}{f}")).collect();
    let store_again = store.retain();
    let handler = RcBlock::new(
        move |identifiers: *mut objc2_foundation::NSArray<NSString>| {
            // SAFETY: WebKit passes a valid array or null.
            let Some(identifiers) = (unsafe { identifiers.as_ref() }) else {
                return;
            };
            for identifier in identifiers.iter() {
                let name = identifier.to_string();
                if name.starts_with(UBLOCK_PREFIX) && !keep.contains(&name) {
                    let done = RcBlock::new(|_error: *mut NSError| {});
                    // SAFETY: an identifier the store just listed.
                    unsafe {
                        store_again.removeContentRuleListForIdentifier_completionHandler(
                            Some(&identifier),
                            Some(&done),
                        );
                    }
                }
            }
        },
    );
    // SAFETY: a block of the documented type.
    unsafe { store.getAvailableContentRuleListIdentifiers(Some(&handler)) };
}

/// Script run in every page before its own: announces Global Privacy
/// Control, which sites in several jurisdictions must honour.
pub const GPC_SCRIPT: &str = "Object.defineProperty(Navigator.prototype, 'globalPrivacyControl', \
    { get: () => true, configurable: true, enumerable: true });";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_build_bigger_lists() {
        let off = Settings {
            protection: Protection::Off,
            block_third_party_cookies: false,
            ..Settings::default()
        };
        assert_eq!(rules(&off), None);
        let count = |s: &Settings| {
            serde_json::from_str::<Vec<Value>>(&rules(s).unwrap())
                .unwrap()
                .len()
        };
        let standard = Settings::default();
        let strict = Settings {
            protection: Protection::Strict,
            ..Settings::default()
        };
        assert!(count(&strict) > count(&standard));
        let cookies_only = Settings {
            protection: Protection::Off,
            ..Settings::default()
        };
        assert_eq!(count(&cookies_only), 1);
    }

    #[test]
    fn domains_are_escaped_into_anchored_filters() {
        let list = rules(&Settings::default()).unwrap();
        assert!(list.contains(r#"^https?://([^/:]*\\.)?doubleclick\\.net[/:?]"#));
    }

    #[test]
    fn site_rule_source_is_stable_across_insertion_orders() {
        let mut forward = SiteControls::default();
        let mut backward = SiteControls::default();
        let hosts = ["a.example", "b.example", "c.example", "d.example"];
        for host in hosts {
            forward.change(host, |site| site.protection = Some(Protection::Off));
        }
        for host in hosts.into_iter().rev() {
            backward.change(host, |site| site.protection = Some(Protection::Off));
        }
        let settings = Settings::default();
        let source = rules_for(&settings, &forward).unwrap();
        assert_eq!(Some(source.clone()), rules_for(&settings, &backward));
        backward.change("unrelated.example", |site| {
            site.camera = Some(crate::settings::SitePermission::Allow);
        });
        assert_eq!(Some(source), rules_for(&settings, &backward));
    }

    #[test]
    fn site_overrides_scope_tracking_and_cookie_rules_to_top_url() {
        let mut controls = SiteControls::default();
        controls.change("teams.microsoft.com", |site| {
            site.protection = Some(Protection::Off);
            site.third_party_cookies = Some(false);
        });
        let list: Vec<Value> = serde_json::from_str(&rules_for(&Settings::default(), &controls).unwrap()).unwrap();
        let top = "^https?://teams\\.microsoft\\.com[/:?]";
        assert!(list.iter().any(|rule| rule["action"]["type"] == "block"
            && rule["trigger"]["unless-top-url"] == json!([top])));
        assert!(list.iter().any(|rule| rule["action"]["type"] == "block-cookies"
            && rule["trigger"]["unless-top-url"] == json!([top])));
    }
}
