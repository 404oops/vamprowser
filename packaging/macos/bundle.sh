#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h:h}"
mode=${1:-release}
if [[ "$mode" != release && "$mode" != debug ]]; then
  print -u2 "usage: packaging/macos/bundle.sh [release|debug]"
  exit 2
fi
if [[ "$mode" == release ]]; then
  cargo build --release --locked
else
  cargo build --locked
fi
app="dist/Vamprowser.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$mode/vamprowser" "$app/Contents/MacOS/vamprowser"
cp packaging/macos/Info.plist "$app/Contents/Info.plist"
cp packaging/macos/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
cp LICENSE "$app/Contents/Resources/LICENSE"

# A Developer ID build can be signed with a provisioning profile. Set
#   VAMPROWSER_SIGN_IDENTITY="Developer ID Application: … (TEAMID)"
#   VAMPROWSER_PROFILE=path/to/Vamprowser.provisionprofile
# to sign that way. Without them the bundle is ad-hoc signed.
if [[ -n "${VAMPROWSER_SIGN_IDENTITY:-}" && -n "${VAMPROWSER_PROFILE:-}" ]]; then
  cp "$VAMPROWSER_PROFILE" "$app/Contents/embedded.provisionprofile"
  profile_plist=$(mktemp)
  security cms -D -i "$VAMPROWSER_PROFILE" > "$profile_plist"
  entitlements=$(mktemp)
  cp packaging/macos/Vamprowser.entitlements "$entitlements"
  # The app and team identifiers the profile is for, which the signature
  # must name too.
  for key in com.apple.application-identifier com.apple.developer.team-identifier; do
    value=$(/usr/libexec/PlistBuddy -c "Print :Entitlements:$key" "$profile_plist")
    /usr/libexec/PlistBuddy -c "Add :$key string $value" "$entitlements"
  done
  codesign --force --deep --options runtime --timestamp \
    --entitlements "$entitlements" --sign "$VAMPROWSER_SIGN_IDENTITY" "$app"
  rm -f "$profile_plist" "$entitlements"
else
  codesign --force --deep --sign - "$app"
fi
print "Built $app"
