#!/usr/bin/env bash
# Updates an existing TV OS install in place: pulls the latest code (if this is
# a git clone), then rebuilds and reinstalls. Your settings, addons, Steam/TMDB
# keys and watch history in ~/.config/tvos are preserved.
#
# Safe to run while the app is open — the live daemon keeps running on its old
# binary until you relaunch.
set -euo pipefail
cd "$(dirname "$0")"

if [ -d .git ] && command -v git >/dev/null 2>&1; then
  echo "==> Pulling latest changes"
  git pull --ff-only || echo "  ! git pull skipped (local changes or no remote) — building what's here"
else
  echo "==> Not a git checkout; building the code in this folder as-is"
fi

exec ./install.sh
