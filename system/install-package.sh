#!/usr/bin/env bash
# Installs TV OS from this prebuilt package as a desktop app (no toolchains,
# no root). Mirrors the from-source ./install.sh but uses the bundled binary
# and UI. After this, "TV OS" is in your application menu; run with tvos-app.
#
# For the full boot-to-TV-box *session* instead, see "Install on the gaming
# PC" in README.md (system/install.sh + the gamescope session files).
set -euo pipefail
cd "$(dirname "$0")"

BIN="$HOME/.local/bin"
DATA="$HOME/.local/share/tvos"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"

echo "==> Installing application"
# Unlink first so re-running as an update can't hit "Text file busy" while the
# daemon is running. Config in ~/.config/tvos is untouched.
rm -f "$BIN/tvosd" "$BIN/tvos-app"
install -Dm755 tvosd          "$BIN/tvosd"
install -Dm755 system/tvos-app "$BIN/tvos-app"
rm -rf "$DATA/ui"; mkdir -p "$DATA"; cp -r ui "$DATA/ui"
install -Dm644 system/tvos.svg "$ICONS/tvos.svg"

echo "==> Creating menu entry"
mkdir -p "$APPS"
cat > "$APPS/tvos.desktop" <<EOF
[Desktop Entry]
Name=TV OS
GenericName=Media & Game Center
Comment=All your games, movies and shows in one couch interface
Exec=$BIN/tvos-app
Icon=tvos
Terminal=false
Type=Application
Categories=Game;AudioVideo;
Keywords=games;steam;emulator;retro;movies;tv;stremio;
StartupWMClass=tvos
EOF
update-desktop-database "$APPS" 2>/dev/null || true

echo "==> Fetching upscaling shaders (best effort)"
./system/get-shaders.sh || echo "    shader download failed — re-run system/get-shaders.sh later"

echo "==> Installing the mpv player UI (best effort)"
./system/get-player.sh || echo "    player UI download failed — mpv's built-in OSC will be used; re-run system/get-player.sh later"

echo
echo "Done — launch 'TV OS' from your application menu, or run: tvos-app"
echo "    Big-screen / controller mode:  tvos-app --tv"
