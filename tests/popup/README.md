# WebKit popup regression check

Serve this directory with `python3 -m http.server 18746 --bind 127.0.0.1 --directory tests/popup`, then open `http://127.0.0.1:18746/` in Vamprowser.

Click both ordinary links first: each must open a regular browser tab, including the `rel="noopener"` link. Close those tabs, then test both buttons. A compact popup should appear without Expand. Click **Send result and close** in it. The opener should show a non-null window, `opener: true`, `sharedCookie: true`, a matching message source, and `Popup closed: true`. The second button starts with `about:blank` and navigates through the returned window reference. Repeat in a private window to check its shared in-memory store.

The popup titlebar moves the window normally without merging it into other browser windows. In Settings → Tabs, Background tab should preserve the same opener and callback behavior; Block should return null and create no window.
