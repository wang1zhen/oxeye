#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
export RUSTUP_TOOLCHAIN=1.95.0
export MACOSX_DEPLOYMENT_TARGET=11.0
version=0.3.0
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$root/dist"

rustup target add --toolchain "$RUSTUP_TOOLCHAIN" aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --locked --target aarch64-apple-darwin
cargo build --release --locked --target x86_64-apple-darwin

app="$stage/dmg/Oxeye.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$stage/oxeye.iconset"
lipo -create target/aarch64-apple-darwin/release/oxeye \
  target/x86_64-apple-darwin/release/oxeye -output "$app/Contents/MacOS/oxeye"
lipo "$app/Contents/MacOS/oxeye" -verify_arch arm64 x86_64
cp packaging/macos/Info.plist "$app/Contents/Info.plist"
cp LICENSE "$app/Contents/Resources/LICENSE"
cp THIRD_PARTY_NOTICES.md "$app/Contents/Resources/THIRD_PARTY_NOTICES.md"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" assets/icons/oxeye-1024.png \
    --out "$stage/oxeye.iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" assets/icons/oxeye-1024.png \
    --out "$stage/oxeye.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$stage/oxeye.iconset" -o "$app/Contents/Resources/oxeye.icns"
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - "$app"
codesign --verify --deep --strict --verbose=2 "$app"
ln -s /Applications "$stage/dmg/Applications"
hdiutil create -volname Oxeye -srcfolder "$stage/dmg" -ov -format UDZO \
  "$root/dist/oxeye-$version-macos-universal.dmg"
hdiutil verify "$root/dist/oxeye-$version-macos-universal.dmg"
