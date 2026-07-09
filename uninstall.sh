#!/usr/bin/env bash
# Removes the user-level TV OS app install. Keeps your config/library data in
# ~/.config/tvos (pass --purge to delete that too).
#
# Flags:
#   --purge    also delete your config/library data in ~/.config/tvos
#   --system   also remove the system session files written by
#              system/install.sh (needs sudo): /usr/local/bin/tvos-{session,shell},
#              the wayland-session .desktop, and /etc/sddm.conf.d/tvos*.conf
set -euo pipefail
: "${HOME:?HOME must be set}"

purge=0
system=0
for arg in "$@"; do
  case "$arg" in
    --purge)  purge=1;;
    --system) system=1;;
    *) echo "unknown option: $arg" >&2; exit 1;;
  esac
done

rm -f  "$HOME/.local/bin/tvosd" "$HOME/.local/bin/tvos-app"
rm -rf "$HOME/.local/share/tvos"
rm -f  "$HOME/.local/share/applications/tvos.desktop"
rm -f  "$HOME/.local/share/icons/hicolor/scalable/apps/tvos.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true

if [ "$system" = 1 ]; then
  echo "Removing system session files (needs sudo)"
  sudo rm -f /usr/local/bin/tvos-session /usr/local/bin/tvos-shell
  sudo rm -f /usr/local/share/wayland-sessions/tvos.desktop
  sudo rm -f /etc/sddm.conf.d/tvos.conf /etc/sddm.conf.d/tvos-autologin.conf
fi

if [ "$purge" = 1 ]; then
  rm -rf "$HOME/.config/tvos"
  echo "Removed TV OS and its config."
else
  echo "Removed TV OS. Config kept in ~/.config/tvos (use --purge to delete)."
fi
