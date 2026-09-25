#!/bin/zsh
# Builds a release Vamprowser.app and packs it into a drag-to-install disk
# image: dist/Vamprowser-<version>.dmg.
set -euo pipefail
cd "${0:A:h:h:h}"
packaging/macos/bundle.sh release
version=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" packaging/macos/Info.plist)
dmg="dist/Vamprowser-$version.dmg"
staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT
cp -R dist/Vamprowser.app "$staging/"
ln -s /Applications "$staging/Applications"
# The volume shows the app's own icon in Finder's sidebar and on the desktop.
cp packaging/macos/AppIcon.icns "$staging/.VolumeIcon.icns"
rm -f "$dmg"
rw=$(mktemp -u).dmg
hdiutil create -quiet -volname Vamprowser -srcfolder "$staging" -fs HFS+ -format UDRW "$rw"
mount=$(hdiutil attach -nobrowse -noautoopen "$rw" | awk -F'\t' '/\/Volumes\// {print $NF}')
if command -v SetFile >/dev/null; then
  SetFile -a C "$mount"
fi
hdiutil detach -quiet "$mount"
hdiutil convert -quiet "$rw" -format UDZO -imagekey zlib-level=9 -o "$dmg"
rm -f "$rw"
print "Built $dmg"
