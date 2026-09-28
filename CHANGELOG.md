# Changelog

## v0.4.7 — 2026-09-28

- Right-click menus were redesigned. Each menu is as wide as its longest label (184–340 pt) instead of a fixed width, with rounded corners, a hairline border, a native shadow, larger 13 pt text, and more room around rows and separators. Menus only reserve space for icons, checkmarks, or submenu chevrons when they have some; checkmarks now sit on the right in the accent colour.
- Menus sweep in. Opening, the panel unrolls from the pointer (upward for shelf menus) while its rows settle in one after another; entering a submenu sweeps its rows in from the right, and going back sweeps them in from the left. The stagger stops after eight rows so long menus are ready at once, and `VAMPIR_SLOW_MOTION` slows it like the other animations.
- Menu rows now have icons: tab, tab strip, toolbar, reload button, address bar, bookmarks bar, bookmark and folder, bookmark star, bookmark import, site controls, media download, start page and history link, download, and extension menus. The row under the pointer is highlighted and its icon takes the accent colour. Submenus are headed by the row that opened them (“‹ Tracking protection”) instead of “Back”.
- 25 new icons were added in the toolbar's style, including checkmark, trash, pencil, undo, tabs, clipboard, shield, cookie, camera, microphone, screen, database, and globe.

## v0.4.6 — 2026-09-28

- The signed macOS app now allows WebKit to load sites outside App Transport Security's app defaults. Explicitly trusted self-signed certificates work in the installed browser; sites without a saved trust choice still show a certificate warning. The exception applies to web content, not the app's other network requests.

## v0.4.5 — 2026-09-28

- Each window now shows only its own new downloads on the download shelf. Clearing a shelf leaves other windows' shelves alone; the full Downloads page still shows the shared download history.
- Download rows and shelf items use Finder's file icons. Shelf menus open upward, with an upward chevron to match.
- Trackpad pinch now uses WebKit's native magnification, keeping the page from reflowing during the gesture. Resetting zoom also resets pinch magnification.
- Certificate trust choices now work across threads, including private-window session choices.

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
