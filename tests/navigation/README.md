# Native history input check

Serve with `python3 -m http.server 18747 --bind 127.0.0.1 --directory tests/navigation`. Open `http://127.0.0.1:18747/` in Vamprowser and follow the link.

With the page focused, Command–Left / Command–[ should go back; Command–Right / Command–] should go forward. Repeat with mouse side buttons over both the page and toolbar. Inside the input, Command–Left/Right should move the caret, while Command–[/] still navigate.

For hardware input diagnostics, launch the executable with `VAMPROWSER_INPUT_TRACE=1`. It reports only navigation key/button codes, event window association, and whether the window monitor accepted the event. It does not record typed text.
