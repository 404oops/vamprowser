#!/bin/zsh
# Build, replace the copy in /Applications, and launch it.
set -euo pipefail
cd "${0:A:h:h:h}"

mode=${1:-release}
if [[ "$mode" != release && "$mode" != debug ]]; then
  print -u2 "usage: packaging/macos/install.sh [release|debug]"
  exit 2
fi

# Keep the installed app intact if building or signing fails.
packaging/macos/bundle.sh "$mode"

installed=/Applications/Vamprowser.app
executable="$installed/Contents/MacOS/vamprowser"
staging=$(mktemp -d /Applications/.Vamprowser-update.XXXXXX)
cleanup() {
  if [[ -e "$staging/previous.app" && ! -e "$installed" ]]; then
    mv "$staging/previous.app" "$installed" || {
      print -u2 "Could not restore the previous app; it remains at $staging/previous.app"
      return
    }
  fi
  rm -rf "$staging"
}
trap cleanup EXIT
ditto dist/Vamprowser.app "$staging/Vamprowser.app"

if pgrep -f "$executable" >/dev/null; then
  print "Quitting Vamprowser…"
  osascript -e 'tell application id "dev.oops404.vamprowser" to quit'
  for ((attempt = 0; attempt < 50; attempt++)); do
    if ! pgrep -f "$executable" >/dev/null; then
      break
    fi
    sleep 0.2
  done
  if pgrep -f "$executable" >/dev/null; then
    print -u2 "Vamprowser did not quit; the installed app was not replaced."
    exit 1
  fi
fi

if [[ -e "$installed" ]]; then
  mv "$installed" "$staging/previous.app"
fi
if ! mv "$staging/Vamprowser.app" "$installed"; then
  exit 1
fi

print "Installed $installed"
if ! open "$installed"; then
  print -u2 "Could not launch the new app; restoring the previous copy."
  if [[ -e "$staging/previous.app" ]]; then
    rm -rf "$installed"
    mv "$staging/previous.app" "$installed"
    open "$installed" || true
  fi
  exit 1
fi
