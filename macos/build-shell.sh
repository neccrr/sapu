#!/usr/bin/env bash
# Build the SwiftUI shell around sapu-core: dist/SapuShell.app
#
# Usage: ./macos/build-shell.sh
#   UNIVERSAL=1      Apple Silicon + Intel in one app
#   SIGN_IDENTITY=…  sign with a Developer ID instead of ad-hoc
#
# Needs Xcode (for swift, xcodebuild and lipo) and Rust.
set -euo pipefail
cd "$(dirname "$0")/.."

PKG=macos/SapuShell
BUILD=$PKG/build
APP=dist/SapuShell.app
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)

rm -rf "$BUILD"
mkdir -p "$BUILD/lib" "$BUILD/include" "$BUILD/gen"

if [ -n "${UNIVERSAL:-}" ]; then
  echo "▸ building sapu-ffi (universal)…"
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release -p sapu-ffi --lib --target "$t"
  done
  lipo -create -output "$BUILD/lib/libsapu_ffi.a" \
    target/aarch64-apple-darwin/release/libsapu_ffi.a \
    target/x86_64-apple-darwin/release/libsapu_ffi.a
  LIB_FOR_BINDINGS=target/aarch64-apple-darwin/release/libsapu_ffi.dylib
  SWIFT_ARCH=(--arch arm64 --arch x86_64)
else
  echo "▸ building sapu-ffi…"
  cargo build --release -p sapu-ffi --lib
  cp target/release/libsapu_ffi.a "$BUILD/lib/"
  LIB_FOR_BINDINGS=target/release/libsapu_ffi.dylib
  SWIFT_ARCH=()
fi

echo "▸ generating Swift bindings"
cargo run --quiet --release -p sapu-ffi --bin uniffi-bindgen -- \
  generate --library "$LIB_FOR_BINDINGS" --language swift --out-dir "$BUILD/gen"
cp "$BUILD/gen/sapu_ffi.swift" "$PKG/Sources/SapuFFI/sapu_ffi.swift"
cp "$BUILD/gen/sapu_ffiFFI.h" "$BUILD/include/"
cp "$BUILD/gen/sapu_ffiFFI.modulemap" "$BUILD/include/module.modulemap"

xcodebuild -create-xcframework \
  -library "$BUILD/lib/libsapu_ffi.a" -headers "$BUILD/include" \
  -output "$BUILD/sapu_ffiFFI.xcframework" >/dev/null

echo "▸ building the SwiftUI app"
(cd "$PKG" && swift build -c release ${SWIFT_ARCH[@]+"${SWIFT_ARCH[@]}"})
BIN_DIR=$(cd "$PKG" && swift build -c release ${SWIFT_ARCH[@]+"${SWIFT_ARCH[@]}"} --show-bin-path)

echo "▸ assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN_DIR/SapuShell" "$APP/Contents/MacOS/SapuShell"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>               <string>SapuShell</string>
  <key>CFBundleDisplayName</key>        <string>Sapu (SwiftUI preview)</string>
  <key>CFBundleIdentifier</key>         <string>io.github.neccrr.sapu.shell</string>
  <key>CFBundleExecutable</key>         <string>SapuShell</string>
  <key>CFBundlePackageType</key>        <string>APPL</string>
  <key>CFBundleVersion</key>            <string>${VERSION}</string>
  <key>CFBundleShortVersionString</key> <string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key>     <string>13.0</string>
  <key>NSHighResolutionCapable</key>    <true/>
  <key>LSApplicationCategoryType</key>  <string>public.app-category.utilities</string>
</dict>
</plist>
PLIST

if [ -n "${SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP"
else
  codesign --force --deep --sign - "$APP"
fi

echo "✓ $APP"
