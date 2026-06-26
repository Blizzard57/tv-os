#!/usr/bin/env bash
# Installs the mpv player UI into TV OS's private mpv config dir (MPV_HOME):
#   uosc       — a modern, Stremio-like on-screen UI (seekbar, menus, chapters)
#   thumbfast  — hover/seek thumbnail previews
# Idempotent; safe to re-run. The player still works without these (mpv falls
# back to its built-in OSC), so failures here are non-fatal.
set -euo pipefail

DIR="${TVOS_MPV_HOME:-$HOME/.local/share/tvos/mpv}"
mkdir -p "$DIR/scripts" "$DIR/fonts"

echo "==> uosc (player UI)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
curl -fsSL -o "$TMP/uosc.zip" \
  https://github.com/tomasklaen/uosc/releases/latest/download/uosc.zip
unzip -oq "$TMP/uosc.zip" -d "$DIR"

echo "==> thumbfast (thumbnails)"
curl -fsSL -o "$DIR/scripts/thumbfast.lua" \
  https://raw.githubusercontent.com/po5/thumbfast/master/thumbfast.lua

echo "Player UI installed in $DIR"
