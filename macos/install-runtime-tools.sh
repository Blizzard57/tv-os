#!/usr/bin/env bash
# Install optional macOS runtime tools used by the native app:
# - webtorrent-cli, kept in the TV OS profile dir so it does not bloat the app
# - mpv, the native video player backend used for direct streams and torrents
set -euo pipefail

PROFILE_DIR="${TVOS_PROFILE_DIR:-$HOME/Library/Application Support/TV OS}"

say() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }

command -v npm >/dev/null 2>&1 || { echo "Missing npm. Install Node.js first."; exit 1; }
command -v brew >/dev/null 2>&1 || { echo "Missing Homebrew. Install Homebrew first."; exit 1; }

say "Installing webtorrent-cli into $PROFILE_DIR"
npm install --prefix "$PROFILE_DIR" webtorrent-cli

if command -v mpv >/dev/null 2>&1; then
  say "mpv already installed: $(command -v mpv)"
else
  say "Installing mpv with Homebrew"
  brew install mpv
fi

say "Runtime tools ready"
"$PROFILE_DIR/node_modules/.bin/webtorrent" --version
mpv --version | sed -n '1p'
