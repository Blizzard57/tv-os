#!/usr/bin/env bash
# Downloads the AI upscaling shaders tvosd's Enhance profiles use:
#   Anime4K v4   (anime chains)        — MIT, github.com/bloc97/Anime4K
#   FSRCNNX      (live-action chains)  — github.com/igv/FSRCNN-TensorFlow
# tvosd now fetches these itself at startup (src/shaders.rs); this script is
# the manual/offline alternative. Idempotent; safe to re-run.
set -euo pipefail

DIR="${TVOS_SHADER_DIR:-$HOME/.local/share/tvos/shaders}"
mkdir -p "$DIR"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# TODO: pin known-good SHA256 for each artifact and verify with `sha256sum -c`
# below. Upstream releases are unsigned and unhashed; until the hashes are
# pinned we rely on HTTPS + the temp-download-then-atomic-mv below so an
# interrupted transfer never leaves a truncated file the `[ -f ]` guard would
# treat as a complete install.

# The full set Enhance's profiles expect; a partial install must NOT count.
FSRCNNX_FILES=(FSRCNNX_x2_16-0-4-1.glsl FSRCNNX_x2_8-0-4-1.glsl)
ANIME4K_MARKER="Anime4K_Upscale_CNN_x2_VL.glsl"

echo "==> FSRCNNX"
for f in "${FSRCNNX_FILES[@]}"; do
  if [ ! -f "$DIR/$f" ]; then
    curl -fsSL -o "$TMP/$f" \
      "https://github.com/igv/FSRCNN-TensorFlow/releases/download/1.1/$f"
    mv -f "$TMP/$f" "$DIR/$f"   # atomic: only lands on a complete download
  fi
done

echo "==> Anime4K v4"
if [ ! -f "$DIR/$ANIME4K_MARKER" ]; then
  curl -fsSL -o "$TMP/anime4k.zip" \
    "https://github.com/bloc97/Anime4K/releases/download/v4.0.1/Anime4K_v4.0.zip"
  # Extract into a staging dir first; only publish once extraction succeeded.
  mkdir -p "$TMP/a4k"
  unzip -joq "$TMP/anime4k.zip" '*.glsl' -d "$TMP/a4k"
  [ -f "$TMP/a4k/$ANIME4K_MARKER" ] || {
    echo "get-shaders: Anime4K zip missing $ANIME4K_MARKER — aborting" >&2
    exit 1
  }
  mv -f "$TMP/a4k/"*.glsl "$DIR/"
fi

# Verify the full expected set actually landed.
missing=()
for f in "${FSRCNNX_FILES[@]}" "$ANIME4K_MARKER"; do
  [ -f "$DIR/$f" ] || missing+=("$f")
done
if [ ${#missing[@]} -gt 0 ]; then
  echo "get-shaders: incomplete install, missing: ${missing[*]}" >&2
  exit 1
fi

echo "Shaders installed in $DIR:"
ls "$DIR" | head -8
echo "  … $(ls "$DIR" | wc -l | tr -d ' ') files total"
