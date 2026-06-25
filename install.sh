#!/usr/bin/env bash
# Install TV OS as a desktop application (user-level — no root needed).
# Tested for CachyOS / Arch; works on any Linux with the build tools below.
#
# After this, "TV OS" appears in your application menu — launch it like any
# app. From a terminal:  tvos-app   (or  tvos-app --tv  for big-screen mode).
set -euo pipefail
cd "$(dirname "$0")"

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m  ! %s\033[0m\n' "$*"; }

# --- build tools (hard requirements) ---
need=()
command -v cargo >/dev/null 2>&1 || need+=(rust)
command -v npm   >/dev/null 2>&1 || need+=(npm)
command -v curl  >/dev/null 2>&1 || need+=(curl)
if [ ${#need[@]} -gt 0 ]; then
  echo "Missing build tools: ${need[*]}"
  echo "On CachyOS:  sudo pacman -S --needed rust nodejs npm curl"
  exit 1
fi

# --- runtime tools (optional; features degrade gracefully if absent) ---
for t in mpv ffprobe gamescope unzip; do
  command -v "$t" >/dev/null 2>&1 || warn "optional '$t' missing (install: sudo pacman -S $t)"
done
have_browser=0
for b in chromium chromium-browser google-chrome-stable brave vivaldi-stable microsoft-edge-stable; do
  command -v "$b" >/dev/null 2>&1 && have_browser=1
done
[ "$have_browser" = 1 ] || warn "no Chromium-family browser — install one: sudo pacman -S chromium"

BIN="$HOME/.local/bin"
DATA="$HOME/.local/share/tvos"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"

say "Building daemon (release)"
( cd tvosd && cargo build --release )

say "Building shell UI"
( cd shell && npm ci && npm run build )

say "Installing application"
install -Dm755 tvosd/target/release/tvosd "$BIN/tvosd"
install -Dm755 system/tvos-app           "$BIN/tvos-app"
rm -rf "$DATA/ui"; mkdir -p "$DATA"; cp -r shell/dist "$DATA/ui"
install -Dm644 system/tvos.svg "$ICONS/tvos.svg"

say "Creating menu entry"
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
gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

say "Fetching upscaling shaders (best effort)"
./system/get-shaders.sh || warn "shader download failed — re-run system/get-shaders.sh later"

case ":$PATH:" in
  *":$BIN:"*) ;;
  *) warn "add $BIN to your PATH to run 'tvos-app' from a terminal";;
esac

echo
say "Done — launch 'TV OS' from your application menu."
echo "    Terminal:        tvos-app"
echo "    Big-screen mode: tvos-app --tv"
echo "    Uninstall:       ./uninstall.sh"
