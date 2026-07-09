#!/usr/bin/env bash
# Builds and installs TV OS phase 1 on this machine (Bazzite or any
# systemd + SDDM + gamescope distro).
#
#   user files:   ~/.local/bin/tvosd, ~/.local/share/tvos/ui
#   system files: /usr/local/bin/tvos-{session,shell},
#                 /usr/local/share/wayland-sessions/tvos.desktop,
#                 /etc/sddm.conf.d/tvos.conf            (these need sudo)
#
# Options:
#   --autologin USER   also boot straight into the TV OS session as USER
#                      (writes /etc/sddm.conf.d/tvos-autologin.conf). Optional —
#                      without it you pick "TV OS" at the SDDM greeter.
set -euo pipefail
: "${HOME:?HOME must be set}"
cd "$(dirname "$0")/.."

AUTOLOGIN_USER=""
while [ $# -gt 0 ]; do
  case "$1" in
    --autologin) AUTOLOGIN_USER="${2:?--autologin needs a username}"; shift 2;;
    --autologin=*) AUTOLOGIN_USER="${1#*=}"; shift;;
    *) echo "unknown option: $1" >&2; exit 1;;
  esac
done

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

if [ -n "$AUTOLOGIN_USER" ]; then
  echo "==> Enabling autologin into the TV OS session as '$AUTOLOGIN_USER' (needs sudo)"
  sudo install -Dm644 /dev/stdin /etc/sddm.conf.d/tvos-autologin.conf <<EOF
# Boot straight into the TV OS session (PLAN phase 1: boot -> shell).
# Remove this file (or run uninstall.sh --system) to return to the greeter.
[Autologin]
User=$AUTOLOGIN_USER
Session=tvos
EOF
fi

echo
echo "Done. Log out and pick the 'TV OS' session on the login screen."
if [ -n "$AUTOLOGIN_USER" ]; then
  echo "Autologin enabled: next boot goes straight to TV OS as '$AUTOLOGIN_USER'."
else
  echo "Tip: re-run with '--autologin \$USER' to boot straight into TV OS."
fi
echo "To test inside your desktop first:  TVOS_WINDOWED=1 tvos-session"
