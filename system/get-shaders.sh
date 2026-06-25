#!/usr/bin/env bash
# Downloads the AI upscaling shaders tvosd's Enhance profiles use:
#   Anime4K v4   (anime chains)        — MIT, github.com/bloc97/Anime4K
#   FSRCNNX      (live-action chains)  — github.com/igv/FSRCNN-TensorFlow
# Idempotent; safe to re-run. Profiles degrade gracefully if this never runs.
set -euo pipefail

DIR="${TVOS_SHADER_DIR:-$HOME/.local/share/tvos/shaders}"
mkdir -p "$DIR"

echo "==> FSRCNNX"
for f in FSRCNNX_x2_16-0-4-1.glsl FSRCNNX_x2_8-0-4-1.glsl; do
  [ -f "$DIR/$f" ] || curl -fsSL -o "$DIR/$f" \
    "https://github.com/igv/FSRCNN-TensorFlow/releases/download/1.1/$f"
done

echo "==> Anime4K v4"
if [ ! -f "$DIR/Anime4K_Upscale_CNN_x2_VL.glsl" ]; then
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  curl -fsSL -o "$TMP/anime4k.zip" \
    "https://github.com/bloc97/Anime4K/releases/download/v4.0.1/Anime4K_v4.0.zip"
  unzip -joq "$TMP/anime4k.zip" '*.glsl' -d "$DIR"
fi

echo "Shaders installed in $DIR:"
ls "$DIR" | head -8
echo "  … $(ls "$DIR" | wc -l | tr -d ' ') files total"
