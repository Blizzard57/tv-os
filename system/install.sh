#!/usr/bin/env bash
# Builds and installs TV OS phase 1 on this machine (Bazzite or any
# systemd + SDDM + gamescope distro).
#
#   user files:   ~/.local/bin/tvosd, ~/.local/share/tvos/ui
#   system files: /usr/local/bin/tvos-{session,shell},
#                 /usr/local/share/wayland-sessions/tvos.desktop,
#                 /etc/sddm.conf.d/tvos.conf            (these need sudo)
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> Building daemon (tvosd)"
(cd tvosd && cargo build --release)

echo "==> Building shell UI"
(cd shell && npm ci && npm run build)

echo "==> Installing user files"
install -Dm755 tvosd/target/release/tvosd "$HOME/.local/bin/tvosd"
rm -rf "$HOME/.local/share/tvos/ui"
mkdir -p "$HOME/.local/share/tvos"
cp -r shell/dist "$HOME/.local/share/tvos/ui"

echo "==> Fetching upscaling shaders (best effort)"
./system/get-shaders.sh || echo "    shader download failed — Enhance will use mpv's built-in scalers; re-run system/get-shaders.sh later"

echo "==> Installing session files (needs sudo)"
sudo install -Dm755 system/tvos-session /usr/local/bin/tvos-session
sudo install -Dm755 system/tvos-shell /usr/local/bin/tvos-shell
sudo install -Dm644 system/tvos.desktop /usr/local/share/wayland-sessions/tvos.desktop
sudo install -Dm644 system/sddm-tvos.conf /etc/sddm.conf.d/tvos.conf

echo
echo "Done. Log out and pick the 'TV OS' session on the login screen."
echo "To test inside your desktop first:  TVOS_WINDOWED=1 tvos-session"
