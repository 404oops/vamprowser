# Changelog

## v0.4.4 — 2026-09-28

- Links opened from other apps now appear in compact page windows. Drag the title into another browser window's tabs, click **Expand**, or open a second tab to get a full browser window. Compact windows no longer enter normal session restore or closed-window history until expanded.
- The site icon in the address bar now opens controls for that host: tracking protection, third-party cookies, camera, microphone, screen capture, and clearing its cookies, storage, or all site data. Host choices are saved for normal browsing; private-tab permission choices remain temporary.
- Trackpad pinch gestures now adjust the current tab's page zoom. The address field distinguishes the scheme and path from the host more clearly.
- Minimal mode's revealed toolbar and sidebar now overlay the page while its native WebKit view is clipped, avoiding page reflow. Menu placement and dismissal were refined.
- HTTP authentication now presents an in-page sign-in form and retries the challenged URL after credentials are entered. Certificate error pages now show certificate details and explain how long trust choices last.
- Active audio playback or camera/microphone capture now keeps the display and computer awake. Unused tabs are put to sleep only after checking playback and capture state.
- The media button now shows inspection activity and results. Bundled media downloads include supplied subtitles and avoid requesting every automatic or translated caption, which could hit site rate limits; automatic captions remain available individually.
- Download rows and shelf items now support dragging files out of the browser. Drag previews and page coverage were refined so dragging over a webpage works reliably; the shelf also animates active and newly added downloads.
- Window startup, session persistence, and tab selection handling were tightened, including restored windows and tabs moved between windows.
