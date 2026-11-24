#!/usr/bin/env bash
# Build Sapu.app — a double-clickable macOS app.
# Usage: ./scripts/bundle-macos.sh        → dist/Sapu.app
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP="dist/Sapu.app"

if [ -z "${SKIP_BUILD:-}" ]; then
  echo "▸ building release…"
  cargo build --release -p sapu-gui
fi

echo "▸ assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/sapu-gui "$APP/Contents/MacOS/Sapu"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>               <string>Sapu</string>
  <key>CFBundleDisplayName</key>        <string>Sapu</string>
  <key>CFBundleIdentifier</key>         <string>io.github.neccrr.sapu</string>
  <key>CFBundleExecutable</key>         <string>Sapu</string>
  <key>CFBundlePackageType</key>        <string>APPL</string>
  <key>CFBundleVersion</key>            <string>${VERSION}</string>
  <key>CFBundleShortVersionString</key> <string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key>     <string>11.0</string>
  <key>NSHighResolutionCapable</key>    <true/>
  <key>LSApplicationCategoryType</key>  <string>public.app-category.utilities</string>
</dict>
</plist>
PLIST

# Ad-hoc signature: lets macOS run it locally and remember Full Disk Access.
codesign --force --deep --sign - "$APP"

echo "✓ $APP"
echo "  Drag it into /Applications, then add it under"
echo "  System Settings → Privacy & Security → Full Disk Access."
