#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
export RUSTUP_TOOLCHAIN=1.95.0
version=0.2.0
tools="$root/.tools/appimage"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$root/dist" "$tools"

cargo build --release --locked
cargo install cargo-deb --version 3.8.0 --locked --root "$root/.tools/cargo-deb"
export PATH="$root/.tools/cargo-deb/bin:$PATH"
cargo deb --no-build --output "$root/dist/oxeye-$version-linux-amd64.deb"

curl --fail --location --retry 3 \
  https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage \
  --output "$tools/appimagetool"
curl --fail --location --retry 3 \
  https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 \
  --output "$tools/runtime-x86_64"
printf '%s  %s\n' \
  ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 "$tools/appimagetool" \
  2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d "$tools/runtime-x86_64" \
  | sha256sum --check --strict
chmod +x "$tools/appimagetool" "$tools/runtime-x86_64"

appdir="$stage/Oxeye.AppDir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/doc/oxeye" \
  "$appdir/usr/share/applications" \
  "$appdir/usr/share/icons/hicolor/256x256/apps" \
  "$appdir/usr/share/icons/hicolor/512x512/apps"
install -m 755 target/release/oxeye "$appdir/usr/bin/oxeye"
install -m 644 LICENSE "$appdir/usr/share/doc/oxeye/LICENSE"
install -m 644 THIRD_PARTY_NOTICES.md "$appdir/usr/share/doc/oxeye/THIRD_PARTY_NOTICES.md"
install -m 644 packaging/linux/oxeye.desktop "$appdir/oxeye.desktop"
install -m 644 packaging/linux/oxeye.desktop "$appdir/usr/share/applications/oxeye.desktop"
install -m 644 assets/icons/oxeye-256.png "$appdir/usr/share/icons/hicolor/256x256/apps/oxeye.png"
install -m 644 assets/icons/oxeye-512.png "$appdir/usr/share/icons/hicolor/512x512/apps/oxeye.png"
install -m 644 assets/icons/oxeye-512.png "$appdir/oxeye.png"
ln -s oxeye.png "$appdir/.DirIcon"
cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
set -eu
# Keep the caller's working directory so relative image paths still work.
appdir="${APPDIR:-$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)}"
exec "$appdir/usr/bin/oxeye" "$@"
APPRUN
chmod +x "$appdir/AppRun"
ARCH=x86_64 VERSION="$version" "$tools/appimagetool" --appimage-extract-and-run \
  --runtime-file "$tools/runtime-x86_64" "$appdir" \
  "$root/dist/oxeye-$version-linux-x86_64.AppImage"
