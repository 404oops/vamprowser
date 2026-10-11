# Local Wry changes

Based on the crates.io source release of Wry 0.57.0 (Apache-2.0 / MIT).

- `src/wkwebview/class/wry_navigation_delegate.rs`: implement WebKit's
  `_webView:contextMenuDidCreateDownload:` callback outside the public
  `WKNavigationDelegate` protocol, and attach Wry's existing download delegate.
  Context-menu image, link and media downloads otherwise have no destination
  or completion callbacks.
- `src/wkwebview/mod.rs`: navigate local files with
  `loadFileURL:allowingReadAccessToURL:` so WebKit's content process can read
  PDFs and sibling resources in local documents.

The application's native page menu bridge lives in
`src/browser/page_menu.rs`. It draws proposed WebKit menus with Vampir and
retains WebKit's completion until an action has run or the menu is dismissed.
