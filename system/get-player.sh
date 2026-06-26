#!/usr/bin/env bash
# Installs the bundled mpv player into TV OS's private mpv config dir (MPV_HOME):
#   uosc        — modern, minimal, Stremio-like on-screen UI (the default)
#   thumbfast   — hover/seek thumbnail previews
#   controller  — game controller support (TV OS)
#   upscaler    — realtime upscaler switching menu (TV OS)
#   input.conf  — remote/controller/keyboard bindings (TV OS)
#
# The player now ships *with* TV OS (tvosd/player/) instead of being downloaded,
# so this is offline and deterministic. The daemon also installs these on its
# first launch; this script is for the portable demo and for pre-warming.
set -euo pipefail

SRC="$(cd "$(dirname "$0")/../tvosd/player" && pwd)"
DIR="${TVOS_MPV_HOME:-$HOME/.local/share/tvos/mpv}"

mkdir -p "$DIR"
cp -r "$SRC"/. "$DIR"/

echo "Player installed in $DIR (uosc + thumbfast + controller + upscaler)"
