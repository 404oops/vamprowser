#!/bin/zsh
# Regenerates AppIcon.icns and icon-512.png from icon.svg.
set -euo pipefail
cd "${0:A:h:h:h}"
iconset="target/AppIcon.iconset"
rm -rf "$iconset"
cargo run --quiet --example render_icon -- packaging/macos/icon.svg "$iconset"
iconutil --convert icns --output packaging/macos/AppIcon.icns "$iconset"
mv target/icon-512.png packaging/macos/icon-512.png
print "Wrote packaging/macos/AppIcon.icns and packaging/macos/icon-512.png"
