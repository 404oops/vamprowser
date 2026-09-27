# Vamprowser

A macOS browser built in Rust: Apple's WebKit (through Wry's `WKWebView`) for pages, and a native [GPUI](https://www.gpui.rs)/[Vampir](https://crates.io/crates/vampir) interface around them. The window takes its colour from the page you're looking at — its `theme-color`, the colour along its top, or its icon — and goes grey for sites with none.

- **Windows and tabs**: as many windows as you like, and private windows that keep nothing. Tabs run along the top or down the side (full sidebar or icon rail). They open and close with animation, and after a close their widths hold still so the next × lands under the pointer. Middle-click closes a tab; right-click offers everything else.
  - Titles use the whole tab. The × appears on hover, and a cut-off title glides along to show the rest, instead of a tooltip.
  - Tabs playing sound show a speaker. Click it to mute or unmute; the tab's right-click menu also has a mute option.
  - **Drag tabs** to reorder them, out of the strip to tear one off into its own window, or onto another window's tabs to move it there, live page and all. The window it will join lights up, with the tab's ghost where it will land.
  - ⌘-click or middle-click a link to open it in a new tab behind (⌘⇧-click, in front).
  - Windows come back only when you ask: ⌘⇧T, once a window's own closed tabs run out, reopens the windows you closed, and after a relaunch the ones open when you quit, where you left them (Settings → General can restore them at launch instead). Closing the last window leaves the app running, and the Dock icon opens a new one.
- **Minimal mode** (⌘⇧M) shows only the page, under a hairline. Point at the top of the window and the browser slides in over the page without it reflowing.
- **Address field**: suggestions as you type.
  - It completes the site you most likely mean.
  - The list shows matching history and bookmarks, most visited first, then your search engine's suggestions.
  - Remove a page from history with its × or ⇧⌫.
  - A progress bar runs along the field as a page loads.
- **Bookmarks** with folders, nested as deep as you like. Folders on the bookmarks bar open as menus. ⌘D saves the page and offers to file it in a folder.
  - The **bookmark manager** (⌥⌘B, `vamp://bookmarks`) shows the folder tree beside the chosen folder, with search across everything and drag and drop to reorder and file.
  - It imports from Safari, Chrome, Brave, Edge, Vivaldi and Arc, or any browser's HTML bookmarks file, and exports one.
- **Find in page** (⌘F): a bar above the page counts the matches; Enter and ⌘G go to the next, ⇧Enter and ⇧⌘G the previous, Esc closes it.
- **Reader Mode** (⌥⌘R, or View → Reader Mode) shows the article already loaded in the current tab in a clean reading layout. Use the shortcut again or Close to return to the page. It does not fetch restricted content.
- **Hints** (button names and shortcuts, and tab titles in the icon rail) are drawn by macOS above the page, so a page never cuts them off.
- **⌘K** switches to any tab and finds bookmarks, history and commands, over a still of the page.
- **The browser's own pages** are native, not web pages: the Start page, Settings, History and Downloads, each with its own `vamp://` address (`vamp://settings/privacy`, `vamp://downloads`, …). Downloads also appear on a shelf along the bottom, old-Chrome style.
- **Settings** cover:
  - appearance, startup and tabs;
  - every search engine worth having: Kagi, Brave, Startpage, Mullvad Leta, Marginalia, SearXNG/Whoogle/4get/LibreY with your own instance, a custom URL, and `@keyword` one-off searches;
  - privacy: tracking protection, third-party cookie blocking, HTTPS-only, Global Privacy Control, search suggestions, camera/microphone/screen permissions, clearing data (it asks first), and exporting or importing everything;
  - history, downloads, a customisable toolbar, extensions and more.
- **Extensions**: Firefox add-ons from addons.mozilla.org (by link or name), or a `.xpi`, `.zip` or unpacked folder, run by WebKit's WebExtension support (macOS 15.4+).
  - HTTP Basic and Digest sign-ins open a browser form with standard fields for password managers such as Proton Pass. Credentials are reused for the current app session and are never saved to Keychain; private windows keep separate credentials until they close.
  - The app does not enable Apple's browser passkeys. Client certificate and proxy authentication are disabled rather than handed to Keychain.
  - A compatibility layer papers over where WebKit differs from Firefox.
  - Add-ons update themselves from addons.mozilla.org daily.
  - uBlock Origin's filter lists are compiled into WebKit content rules, since WebKit won't let extensions block requests. They follow what you set in uBlock Origin — its filter lists (including lists imported by address), your own filters, your dynamic rules, per-site switches (no pop-ups, no scripting, no remote fonts) and trusted sites — within seconds of a change, and refresh on their own.
- **A real macOS browser**:
  - It can be the default browser.
  - It opens links and HTML/PDF/image/text files handed over by other apps.
  - Handoff offers the page in front to your other devices; this needs a Developer ID-signed build.
  - It has a Dock menu.
- **Media downloads**: the toolbar's media button asks `yt-dlp` what the current page offers, then lists video and audio formats, subtitles, a description, and a folder bundle with all available parts. Individual choices save files directly in Downloads; only the bundle creates a folder. Install `yt-dlp` separately (`brew install yt-dlp`); some formats also need `ffmpeg` (`brew install ffmpeg`).

Your data lives in `~/Library/Application Support/Vamprowser/`: settings, session, bookmarks, history, downloads, extensions, and `Site Data` — cookies and logins, site storage and extensions' storage. WebKit would keep site data under the app's identifier, which a development build and the installed app don't share; here both use the same, and it stays put across reinstalls. Site icons and filter lists are cached in `~/Library/Caches/Vamprowser/`.

**Settings → Privacy → Your data** exports all of it to one `.zip`, to bring back after reinstalling or on another Mac with Import (which keeps what it replaces in `Vamprowser (before import)` beside it, then relaunches). Don’t run the dev build and the installed app at the same time: they share the site data.

## Run

Requires macOS, Xcode, and a recent Rust toolchain.

```sh
cargo run
```

## License

Vamprowser is licensed under the GNU General Public License version 3. See [LICENSE](LICENSE) for the full terms.

## Source layout

`src/main.rs` assembles the browser. Its module declarations point to these feature directories while keeping the existing Rust module names:

| Directory | Code |
| --- | --- |
| `bookmarks/` | Bookmark storage, menus, and manager |
| `browser/` | Page navigation, WebView features, media, and downloads |
| `data/` | Settings, history, cache, site data, and saved state |
| `extensions/` | WebExtension integration and compatibility |
| `interface/` | Native pages, menus, commands, and visual controls |
| `platform/` | macOS integration, authentication, and updates |
| `privacy/` | Content rules, filtering, and HTTPS handling |

## Package

```sh
packaging/macos/bundle.sh          # dist/Vamprowser.app (release)
packaging/macos/bundle.sh debug    # faster local build
packaging/macos/install.sh          # build, replace /Applications/Vamprowser.app, launch
packaging/macos/install.sh debug    # same with a debug build
packaging/macos/dmg.sh             # dist/Vamprowser-<version>-macos-<architecture>.dmg
packaging/macos/icon.sh            # regenerate AppIcon.icns from icon.svg
```

The bundle is ad-hoc signed. On another Mac, open it the first time with right-click → Open (or System Settings → Privacy & Security → Open Anyway). Setting it as the default browser only works from the app bundle, and Handoff needs it signed with your Developer ID.

## Shortcuts

| Keys | Action |
| --- | --- |
| ⌘N / ⌘⇧N | New window / new private window |
| ⌘T | New tab |
| ⌘W / ⌘⇧W | Close tab / close window |
| ⌘⇧T | Reopen closed tab, then closed windows (and last session's, after a relaunch) |
| ⌘K | Switch tabs, find anything |
| ⌘L | Focus address field (↑↓ walk suggestions, esc dismisses) |
| ⌘R / ⌘. | Reload / stop |
| ⌘⇧R | Erase cache and reload (also on the reload button's right-click menu) |
| ⌥⌘R | Toggle Reader Mode |
| ⌘F / ⌘G / ⇧⌘G | Find in page / next / previous |
| ⌘[ / ⌘] | Back / forward |
| ⌘⇧H | Home |
| ⌘D | Bookmark page (and choose its folder) |
| ⌥⌘B | Bookmark manager |
| ⌘⇧B | Show/hide bookmarks bar |
| ⌘⇧L | Vertical tabs |
| ⌘⇧M | Minimal mode |
| ⌘1–⌘8, ⌘9 | Go to tab, last tab |
| ⌃Tab / ⌃⇧Tab, ⌘⇧] / ⌘⇧[ | Next / previous tab |
| ⌘+ / ⌘− / ⌘0 | Zoom in / out / actual size |
| ⌘, | Settings |
| ⌘Y | History |
| ⌥⌘L | Downloads |
| ⌘P | Print |
| ⌘M | Minimize |
| ⌥⌘I | Web Inspector |

In the address field, `@k rust` searches Kagi once, `@w` Wikipedia, `@g` Google, and so on; Settings → Search lists them all.
