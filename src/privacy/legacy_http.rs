//! Opens pages from servers too old for Apple's networking: the web
//! interfaces of switches, routers and printers that answer some requests
//! with just the page, no status line or headers (HTTP/0.9). WebKit fails
//! those with "cannot parse response"; other browsers show the page.
//!
//! Once a host has answered like that, WebKit is told to reach it through
//! a proxy here, on this machine, for that host only. The proxy passes
//! everything through untouched, except that a reply with no status line
//! gets one, with a type guessed from the address asked for.
//!
//! WebKit takes a data store's proxies when the store is first used and
//! not after, so the hosts are remembered: the shared store is given them
//! at launch. A host found meanwhile is reached in a store of its own, made
//! for it (see [`fresh_store`]), until the next launch.

use std::{
    collections::{HashMap, HashSet},
    ffi::{CString, c_char},
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::Duration,
};

use objc2::{rc::Retained, runtime::AnyObject};
use objc2_foundation::{NSArray, NSObjectNSKeyValueCoding, NSObjectProtocol, NSString};
use objc2_web_kit::WKWebsiteDataStore;

/// Hosts that answered without a status line, reached through the proxy.
fn hosts() -> &'static Mutex<HashSet<String>> {
    static HOSTS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    HOSTS.get_or_init(Mutex::default)
}

/// Whether `host` is one of them.
pub fn is_legacy(host: &str) -> bool {
    hosts().lock().is_ok_and(|hosts| hosts.contains(&host.to_ascii_lowercase()))
}

fn saved_path() -> Option<std::path::PathBuf> {
    crate::state::data_path("legacy-hosts.json")
}

/// The hosts found in earlier runs. Call before WebKit starts.
pub fn load_saved() {
    let saved: Vec<String> = saved_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if let Ok(mut hosts) = hosts().lock() {
        hosts.extend(saved.into_iter().map(|h| h.to_ascii_lowercase()));
    }
}

/// Takes `host` through the proxy from now on, and at later launches.
/// Whether it's new.
pub fn add(host: &str) -> bool {
    let Ok(mut hosts) = hosts().lock() else {
        return false;
    };
    let added = hosts.insert(host.to_ascii_lowercase());
    if added && let Some(path) = saved_path() {
        let mut list: Vec<&String> = hosts.iter().collect();
        list.sort();
        if let Ok(bytes) = serde_json::to_vec(&list) {
            let _ = crate::state::write_atomic(&path, &bytes);
        }
    }
    added
}

/// Which hosts each store was set up to reach through the proxy, by the
/// store's address.
fn routed() -> &'static Mutex<HashMap<usize, HashSet<String>>> {
    static ROUTED: OnceLock<Mutex<HashMap<usize, HashSet<String>>>> = OnceLock::new();
    ROUTED.get_or_init(Mutex::default)
}

/// Whether `store` reaches `host` through the proxy.
pub fn reaches(store: &WKWebsiteDataStore, host: &str) -> bool {
    let key = store as *const WKWebsiteDataStore as usize;
    routed()
        .lock()
        .is_ok_and(|routed| routed.get(&key).is_some_and(|hosts| hosts.contains(&host.to_ascii_lowercase())))
}

/// A store, kept in memory, that reaches every host known so far through
/// the proxy: for a page on a host found since its tab's store was set up.
pub fn fresh_store(mtm: objc2::MainThreadMarker) -> Retained<WKWebsiteDataStore> {
    // SAFETY: a plain constructor, on the main thread.
    let store = unsafe { WKWebsiteDataStore::nonPersistentDataStore(mtm) };
    route(&store);
    store
}

/// The proxy's port on this machine, started the first time it's needed.
fn port() -> Option<u16> {
    static PORT: OnceLock<Option<u16>> = OnceLock::new();
    *PORT.get_or_init(|| {
        let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
        let port = listener.local_addr().ok()?.port();
        thread::spawn(move || {
            for client in listener.incoming().flatten() {
                thread::spawn(move || {
                    let _ = serve(client);
                });
            }
        });
        Some(port)
    })
}

#[link(name = "Network", kind = "framework")]
unsafe extern "C" {
    fn nw_endpoint_create_host(hostname: *const c_char, port: *const c_char) -> *mut AnyObject;
    fn nw_proxy_config_create_http_connect(
        endpoint: *mut AnyObject,
        tls_options: *mut AnyObject,
    ) -> *mut AnyObject;
    fn nw_proxy_config_add_match_domain(config: *mut AnyObject, domain: *const c_char);
}

/// Sends `store`'s requests to the old hosts through the proxy, and every
/// other request straight on, as before; only before the store is first
/// used (WebKit keeps what it had then). Needs macOS 14.
pub fn route(store: &WKWebsiteDataStore) {
    let hosts: Vec<String> = hosts().lock().map(|h| h.iter().cloned().collect()).unwrap_or_default();
    if hosts.is_empty() {
        return;
    }
    let selector = objc2::sel!(setProxyConfigurations:);
    if !store.respondsToSelector(selector) {
        return;
    }
    let Some(port) = port() else {
        return;
    };
    let (Ok(address), Ok(port)) = (CString::new("127.0.0.1"), CString::new(port.to_string())) else {
        return;
    };
    // SAFETY: Network's constructors, given valid C strings, return new
    // objects we own (+1), or null; the match domains are copied.
    let config = unsafe {
        let endpoint = nw_endpoint_create_host(address.as_ptr(), port.as_ptr());
        if endpoint.is_null() {
            return;
        }
        let config = nw_proxy_config_create_http_connect(endpoint, std::ptr::null_mut());
        drop(Retained::from_raw(endpoint));
        let Some(config) = Retained::from_raw(config) else {
            return;
        };
        for host in &hosts {
            if let Ok(host) = CString::new(host.as_str()) {
                nw_proxy_config_add_match_domain(Retained::as_ptr(&config) as *mut _, host.as_ptr());
            }
        }
        config
    };
    let configs = NSArray::from_retained_slice(&[config]);
    // SAFETY: WebKit's own property (macOS 14+, checked above), an array of
    // Network proxy configurations.
    unsafe {
        store.setValue_forKey(Some(&configs), &NSString::from_str("proxyConfigurations"));
    }
    if let Ok(mut routed) = routed().lock() {
        routed.insert(store as *const WKWebsiteDataStore as usize, hosts.into_iter().collect());
    }
}

/// One connection from WebKit: a CONNECT to an old host, then the bytes
/// both ways, a reply without a status line given one.
fn serve(mut client: TcpStream) -> io::Result<()> {
    let head = read_head(&mut client)?;
    let text = String::from_utf8_lossy(&head);
    let target = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("CONNECT "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_default()
        .to_owned();
    let host = target.rsplit_once(':').map_or(target.as_str(), |(host, _)| host);
    // Only the hosts it was set up for: not a proxy for anything else here.
    if !is_legacy(host.trim_matches(['[', ']'])) {
        client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")?;
        return Ok(());
    }
    let address = target
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::other("no address"))?;
    let mut server = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    // What was last asked for, to name the type of a bare reply by.
    let asked = Arc::new(Mutex::new(String::new()));
    let (mut from_client, mut to_server) = (client.try_clone()?, server.try_clone()?);
    let noted = asked.clone();
    let upstream = thread::spawn(move || {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let read = match from_client.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            if let Some(path) = request_path(&buffer[..read])
                && let Ok(mut asked) = noted.lock()
            {
                *asked = path;
            }
            if to_server.write_all(&buffer[..read]).is_err() {
                break;
            }
        }
        let _ = to_server.shutdown(Shutdown::Write);
    });
    let mut buffer = [0u8; 16 * 1024];
    let mut first = true;
    loop {
        let read = match server.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if std::mem::take(&mut first) && !buffer[..read].starts_with(b"HTTP/") {
            let path = asked.lock().map(|p| p.clone()).unwrap_or_default();
            let header = format!(
                "HTTP/1.0 200 OK\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                content_type(&path, &buffer[..read])
            );
            client.write_all(header.as_bytes())?;
        }
        client.write_all(&buffer[..read])?;
    }
    let _ = client.shutdown(Shutdown::Both);
    let _ = upstream.join();
    Ok(())
}

/// A request's head, up to its blank line.
fn read_head(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 || stream.read(&mut byte)? == 0 {
            return Err(io::Error::other("bad request"));
        }
        head.push(byte[0]);
    }
    Ok(head)
}

/// The path of a request starting in `bytes`, if one does.
fn request_path(bytes: &[u8]) -> Option<String> {
    let line = bytes.split(|&b| b == b'\r').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split(' ');
    let method = parts.next()?;
    if !matches!(method, "GET" | "POST" | "HEAD" | "PUT" | "DELETE" | "OPTIONS") {
        return None;
    }
    parts.next().map(str::to_owned)
}

/// The type of a reply that didn't say, from the path asked for or, for
/// no telling extension, what it starts with.
fn content_type(path: &str, start: &[u8]) -> &'static str {
    let path = path.split(['?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    let extension = path.rsplit_once('.').map_or("", |(_, e)| e);
    match extension {
        "css" => "text/css",
        "js" => "text/javascript",
        "gif" => "image/gif",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "xml" => "text/xml",
        "json" => "application/json",
        "txt" => "text/plain",
        _ if start.iter().skip_while(|b| b.is_ascii_whitespace()).next() == Some(&b'<') => "text/html",
        "" | "htm" | "html" | "asp" | "cgi" | "shtml" => "text/html",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_replies_are_typed_by_what_was_asked() {
        assert_eq!(content_type("/default.html", b"<html>"), "text/html");
        assert_eq!(content_type("/style.css?v=1", b"body{}"), "text/css");
        assert_eq!(content_type("/cgi-bin/status", b"  <table>"), "text/html");
        assert_eq!(content_type("/logo.gif", b"GIF89a"), "image/gif");
    }

    #[test]
    fn request_paths_come_from_request_lines() {
        assert_eq!(request_path(b"GET /a.html HTTP/1.1\r\nHost: x\r\n"), Some("/a.html".into()));
        assert_eq!(request_path(b"\x16\x03\x01"), None);
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Against a real old device: `VAMPROWSER_LEGACY_HOST=10.0.2.222 cargo
    /// test legacy -- --ignored`.
    #[test]
    #[ignore]
    fn a_bare_reply_comes_back_with_a_status_line() {
        let Ok(host) = std::env::var("VAMPROWSER_LEGACY_HOST") else {
            return;
        };
        add(&host);
        let port = port().unwrap();
        let mut proxy = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(proxy, "CONNECT {host}:80 HTTP/1.1\r\nHost: {host}:80\r\n\r\n").unwrap();
        let head = read_head(&mut proxy).unwrap();
        assert!(head.starts_with(b"HTTP/1.1 200"));
        write!(proxy, "GET /default.html HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
        let mut reply = Vec::new();
        proxy.read_to_end(&mut reply).unwrap();
        let text = String::from_utf8_lossy(&reply);
        println!("{}", &text[..text.len().min(200)]);
        assert!(text.starts_with("HTTP/"));
        assert!(text.contains("<html") || text.contains("<HTML"));
    }
}
