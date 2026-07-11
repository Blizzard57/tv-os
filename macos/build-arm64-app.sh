#!/usr/bin/env bash
# Build a native Apple Silicon macOS app bundle:
#
#   dist/mac/TV OS.app
#   dist/TV OS-<version>-macos-arm64.zip
#
# The app is a tiny Swift/AppKit/WebKit host that launches the bundled Rust
# daemon and loads the bundled shell over loopback. No Electron runtime.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This builder must run on macOS."
  exit 1
fi

VERSION="$(grep '^version' tvosd/Cargo.toml | head -1 | cut -d'"' -f2)"
APP_NAME="TV OS"
TARGET="aarch64-apple-darwin"
MACOS_TARGET="arm64-apple-macosx13.0"
APP_DIR="$ROOT/dist/mac/$APP_NAME.app"
CONTENTS="$APP_DIR/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"
ICONSET="$ROOT/dist/mac/TVOS.iconset"
BUILD_CACHE="$ROOT/dist/mac/build-cache"
ZIP="$ROOT/dist/$APP_NAME-$VERSION-macos-arm64.zip"

say() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }

command -v cargo >/dev/null 2>&1 || { echo "Missing cargo."; exit 1; }
command -v rustup >/dev/null 2>&1 || { echo "Missing rustup."; exit 1; }
command -v npm >/dev/null 2>&1 || { echo "Missing npm."; exit 1; }
command -v swiftc >/dev/null 2>&1 || { echo "Missing swiftc / Xcode command line tools."; exit 1; }
if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "Missing Rust target $TARGET. Install it with: rustup target add $TARGET"
  exit 1
fi

say "Building tvosd for macOS arm64"
cargo build --manifest-path tvosd/Cargo.toml --release --target "$TARGET"

say "Building shell UI"
(cd shell && npm ci --silent && npm run build --silent)

say "Assembling app bundle"
rm -rf "$APP_DIR" "$ICONSET" "$ZIP"
mkdir -p "$MACOS" "$RESOURCES" "$BUILD_CACHE"
export CLANG_MODULE_CACHE_PATH="$BUILD_CACHE/clang"
export SWIFT_MODULE_CACHE_PATH="$BUILD_CACHE/swift"

cp macos/TVOSMac/Info.plist "$CONTENTS/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$CONTENTS/Info.plist" >/dev/null
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" "$CONTENTS/Info.plist" >/dev/null

swift -module-cache-path "$SWIFT_MODULE_CACHE_PATH" macos/TVOSMac/make_icon.swift "$ICONSET" "$RESOURCES/TVOS.icns" >/dev/null

swiftc -O -target "$MACOS_TARGET" \
  -module-cache-path "$SWIFT_MODULE_CACHE_PATH" \
  -framework AppKit -framework WebKit \
  macos/TVOSMac/main.swift \
  macos/TVOSMac/AppDelegate.swift \
  -o "$MACOS/$APP_NAME"

install -m 755 "tvosd/target/$TARGET/release/tvosd" "$RESOURCES/tvosd"
cp -R shell/dist "$RESOURCES/ui"

if command -v codesign >/dev/null 2>&1; then
  say "Ad-hoc signing app bundle"
  codesign --force --deep --sign - "$APP_DIR" >/dev/null
fi

say "Creating zip"
ditto -c -k --sequesterRsrc --keepParent "$APP_DIR" "$ZIP"

say "Done"
echo "App: $APP_DIR"
echo "Zip: $ZIP"
