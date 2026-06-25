#!/usr/bin/env bash
# Self-contained TV OS demo — runs entirely from this folder, in a sandbox.
#
# Nothing on your system is touched: config, ROMs and shaders live in a
# temporary directory (set TVOS_DEMO_DIR to keep state between runs). Works
# on any Linux or macOS machine; mpv/Steam/RetroArch are only needed to
# actually play things, the interface itself runs without them.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"

SANDBOX="${TVOS_DEMO_DIR:-$(mktemp -d -t tvos-demo)}"
export TVOS_CONFIG_DIR="$SANDBOX/config"
export TVOS_ROM_DIR="$SANDBOX/roms"
export TVOS_SHADER_DIR="$SANDBOX/shaders"
export TVOS_UI_DIR="$ROOT/ui"

URL="http://127.0.0.1:8484"
PIDS=()
cleanup() { kill "${PIDS[@]}" 2>/dev/null || true; }
trap cleanup EXIT

"$ROOT/tvosd" &
PIDS+=($!)

for _ in $(seq 1 50); do
  curl -fsS "$URL/api/version" >/dev/null 2>&1 && break
  sleep 0.2
done

# Bundle the sample stream addon so the demo has something to play.
if command -v python3 >/dev/null 2>&1; then
  python3 "$ROOT/tools/sample-addon.py" &
  PIDS+=($!)
  sleep 0.5
  curl -fsS -X POST "$URL/api/addons" -H 'Content-Type: application/json' \
    -d '{"url": "http://127.0.0.1:7100/manifest.json"}' >/dev/null 2>&1 || true
fi

echo
echo "TV OS demo running:  $URL"
echo "Sandbox state in:    $SANDBOX"
echo "Open the URL in a browser; arrows + Enter (or a gamepad) to navigate."
echo "Ctrl-C stops everything."
wait
