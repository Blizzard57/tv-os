#!/usr/bin/env bash
# Removes the user-level TV OS app install. Keeps your config/library data in
# ~/.config/tvos (pass --purge to delete that too).
set -euo pipefail

rm -f  "$HOME/.local/bin/tvosd" "$HOME/.local/bin/tvos-app"
rm -rf "$HOME/.local/share/tvos"
rm -f  "$HOME/.local/share/applications/tvos.desktop"
rm -f  "$HOME/.local/share/icons/hicolor/scalable/apps/tvos.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true

if [ "${1:-}" = "--purge" ]; then
  rm -rf "$HOME/.config/tvos"
  echo "Removed TV OS and its config."
else
  echo "Removed TV OS. Config kept in ~/.config/tvos (use --purge to delete)."
fi
