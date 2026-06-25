#!/usr/bin/env bash
# Builds a self-contained, independently testable TV OS package:
#
#   dist/tvos-<version>-<os>-<arch>.tar.gz
#     ├── tvosd            release daemon binary (for THIS machine's os/arch;
#     │                    run this script on the target machine or cross-compile)
#     ├── ui/              built shell
#     ├── run-demo.sh      sandboxed demo — test the whole OS from this folder
#     ├── install.sh       full install (sessions, shaders) for the gaming PC
#     ├── system/, tools/  session files, shader fetcher, sample addon
#     └── README.md, PLAN.md
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="$(grep '^version' tvosd/Cargo.toml | head -1 | cut -d'"' -f2)"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
PKG="tvos-$VERSION-$OS-$ARCH"
OUT="dist/$PKG"

echo "==> Building tvosd (release)"
(cd tvosd && cargo build --release)

echo "==> Building shell"
(cd shell && npm ci --silent && npm run build --silent)

echo "==> Assembling $OUT"
rm -rf "$OUT"
mkdir -p "$OUT"
cp tvosd/target/release/tvosd "$OUT/"
cp -r shell/dist "$OUT/ui"
cp -r system tools "$OUT/"
cp system/run-demo.sh "$OUT/run-demo.sh"
cp system/install-package.sh "$OUT/install.sh"
cp uninstall.sh "$OUT/uninstall.sh"
cp README.md PLAN.md "$OUT/"

tar -czf "dist/$PKG.tar.gz" -C dist "$PKG"
echo
echo "Package: dist/$PKG.tar.gz"
echo "Test it anywhere:  tar xzf $PKG.tar.gz && cd $PKG && ./run-demo.sh"
