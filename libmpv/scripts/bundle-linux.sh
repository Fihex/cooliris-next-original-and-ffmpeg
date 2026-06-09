#!/usr/bin/env bash
# Collect a self-contained runtime into vendor/ so the packaged app needs neither system
# Node nor system mpv: a Node binary (to run the mpv host) + libmpv and its shared-lib
# deps. electron-builder ships vendor/ as extraResources; at runtime the host runs under
# vendor/node with LD_LIBRARY_PATH=vendor/lib. Core system libs (glibc, GL/driver) are
# left to the host OS on purpose — bundling those causes more trouble than it solves.
set -euo pipefail
cd "$(dirname "$0")/.."

VENDOR="vendor"
rm -rf "$VENDOR"
mkdir -p "$VENDOR/lib"

# 1) Node (must match the ABI the addon was built for).
NODE_BIN="$(command -v node)"
cp -L "$NODE_BIN" "$VENDOR/node"
echo "node: $NODE_BIN ($("$VENDOR/node" -v))"

# 2) libmpv + transitive deps the addon pulls in.
EXCLUDE='^(ld-linux|libc|libm|libdl|libpthread|librt|libresolv|libgcc_s|libstdc\+\+|libGL|libGLX|libGLdispatch|libEGL|libOpenGL|libdrm|libgbm|libX11|libxcb|libwayland|libGLU|libgallium)'

copy_deps() {
  ldd "$1" 2>/dev/null | awk '/=>/{print $3}' | while read -r lib; do
    [ -f "$lib" ] || continue
    base="$(basename "$lib")"
    echo "$base" | grep -Eq "$EXCLUDE" && continue
    [ -f "$VENDOR/lib/$base" ] && continue
    cp -L "$lib" "$VENDOR/lib/"
  done
}

MPV_NODE="native/build/Release/mpv.node"
copy_deps "$MPV_NODE"
# resolve deps-of-deps a few levels (libav* → libass, etc.)
for _ in 1 2 3; do
  for f in "$VENDOR"/lib/*.so*; do copy_deps "$f"; done
done

echo "bundled $(ls "$VENDOR/lib" | wc -l) libs into $VENDOR/lib"
ls "$VENDOR/lib" | sort
