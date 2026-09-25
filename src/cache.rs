//! Erase Cache and Reload: empties what WebKit has cached for the page's
//! site and every site it loaded something from, then loads the page again
//! from the network.

use block2::RcBlock;
use objc2::{
    rc::{Retained, Weak},
    runtime::AnyObject,
};
use objc2_foundation::{NSArray, NSError, NSSet, NSString};
use objc2_web_kit::{
    WKWebView, WKWebsiteDataRecord, WKWebsiteDataTypeDiskCache, WKWebsiteDataTypeFetchCache,
    WKWebsiteDataTypeMemoryCache, WKWebsiteDataTypeOfflineWebApplicationCache,
};

/// The page's host and those of everything it loaded, as a JSON array.
const HOSTS_SCRIPT: &str = r#"JSON.stringify([location.hostname].concat(
  performance.getEntriesByType('resource').map((entry) => {
    try { return new URL(entry.name).hostname; } catch (_) { return ''; }
  })))"#;

/// Whether a data record, named for a site (`example.com`), covers `host`.
fn covers(site: &str, host: &str) -> bool {
    let site = site.trim_start_matches('.').to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    !site.is_empty()
        && (host == site || host.strip_suffix(site.as_str()).is_some_and(|rest| rest.ends_with('.')))
}

fn hosts_of(result: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(result)
        .unwrap_or_default()
        .into_iter()
        .filter(|host| !host.is_empty())
        .collect()
}

pub fn erase_and_reload(webview: &WKWebView) {
    let weak = Weak::from(webview);
    let page_host = unsafe { webview.URL() }
        .and_then(|url| url.host())
        .map(|host| host.to_string());
    let answered = RcBlock::new(move |result: *mut AnyObject, _error: *mut NSError| {
        // SAFETY: WebKit passes the script's value, or null, for the
        // callback's duration.
        let text = unsafe { result.as_ref() }
            .and_then(|value| value.downcast_ref::<NSString>())
            .map(|text| text.to_string());
        let mut hosts = text.as_deref().map(hosts_of).unwrap_or_default();
        // Scripts off, or the page not answering: its own site, at least.
        if hosts.is_empty() {
            hosts.extend(page_host.clone());
        }
        if let Some(webview) = weak.load() {
            erase_for(&webview, hosts);
        }
    });
    // SAFETY: a valid script and a block of the documented type, called
    // once on the main thread.
    unsafe {
        webview.evaluateJavaScript_completionHandler(&NSString::from_str(HOSTS_SCRIPT), Some(&answered));
    }
}

fn erase_for(webview: &WKWebView, hosts: Vec<String>) {
    // SAFETY: WebKit's own constants, and plain property reads.
    let (types, store) = unsafe {
        let types: Retained<NSSet<NSString>> = NSSet::from_slice(&[
            WKWebsiteDataTypeDiskCache,
            WKWebsiteDataTypeMemoryCache,
            WKWebsiteDataTypeFetchCache,
            WKWebsiteDataTypeOfflineWebApplicationCache,
        ]);
        (types, webview.configuration().websiteDataStore())
    };
    let weak = Weak::from(webview);
    let (store_again, types_again) = (store.clone(), types.clone());
    let fetched = RcBlock::new(move |records: std::ptr::NonNull<NSArray<WKWebsiteDataRecord>>| {
        // SAFETY: WebKit passes a valid array for the callback's duration.
        let records = unsafe { records.as_ref() };
        let chosen: Vec<Retained<WKWebsiteDataRecord>> = records
            .iter()
            .filter(|record| {
                // SAFETY: a plain property read.
                let site = unsafe { record.displayName() }.to_string();
                hosts.iter().any(|host| covers(&site, host))
            })
            .collect();
        let weak = weak.clone();
        let reload = RcBlock::new(move || {
            if let Some(webview) = weak.load() {
                // SAFETY: a live web view, on the main thread.
                unsafe { webview.reloadFromOrigin() };
            }
        });
        if chosen.is_empty() {
            reload.call(());
            return;
        }
        // SAFETY: records this store just listed, and a block of the
        // documented type.
        unsafe {
            store_again.removeDataOfTypes_forDataRecords_completionHandler(
                &types_again,
                &NSArray::from_retained_slice(&chosen),
                &reload,
            );
        }
    });
    // SAFETY: a valid set of types and a block of the documented type.
    unsafe { store.fetchDataRecordsOfTypes_completionHandler(&types, &fetched) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_cover_their_site_and_its_subdomains_only() {
        assert!(covers("example.com", "example.com"));
        assert!(covers("example.com", "static.cdn.example.com"));
        assert!(!covers("example.com", "notexample.com"));
        assert!(!covers("", "example.com"));
    }

    #[test]
    fn hosts_come_from_the_page_script() {
        assert_eq!(hosts_of(r#"["a.org","","cdn.b.net"]"#), ["a.org", "cdn.b.net"]);
        assert!(hosts_of("not json").is_empty());
    }
}
