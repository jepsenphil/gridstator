#!/bin/sh
set -eu
base=/Users/philjepsen/gridstator-validation.I50ONpwm/docker-desktop
image="$base/Docker.dmg"
mount="$base/mount"
printf '%s\n' '0ede7f9917b55c396a17b03c069cadf457a7d21904bc264c6d398fe132da7fb9  /Users/philjepsen/gridstator-validation.I50ONpwm/docker-desktop/Docker.dmg' | shasum -a 256 -c -
if test -e /Applications/Docker.app; then
  echo 'Docker.app already exists; refusing to overwrite.'
  exit 1
fi
mkdir -p "$mount"
hdiutil attach -readonly -nobrowse -mountpoint "$mount" "$image"
trap 'hdiutil detach "$mount" >/dev/null 2>&1 || true' EXIT
codesign --verify --deep --strict --verbose=2 "$mount/Docker.app"
codesign --display --verbose=4 "$mount/Docker.app" 2>&1
spctl --assess --type execute --verbose=2 "$mount/Docker.app"
ditto "$mount/Docker.app" /Applications/Docker.app
codesign --verify --deep --strict --verbose=2 /Applications/Docker.app
/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' /Applications/Docker.app/Contents/Info.plist
printf '%s\n' 'Verified application installed; no first-launch agreement accepted.'
