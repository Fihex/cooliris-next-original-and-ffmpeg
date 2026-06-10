#!/usr/bin/env bash
# Collect a self-contained runtime into vendor/ so the packaged app needs neither system
# Node nor system VLC:
#   vendor/node         — Node binary (runs the vlc host; must match the addon's ABI)
#   vendor/lib          — libvlc + libvlccore + their shared-lib deps
#   vendor/vlc-plugins  — VLC's plugin tree (the actual demuxers/decoders/outputs;
#                         libvlc loads them at runtime via VLC_PLUGIN_PATH)
# electron-builder ships vendor/ as extraResources. Core system libs (glibc, GL/driver,
# X/wayland, audio servers) are left to the host OS on purpose.
set -euo pipefail
cd "$(dirname "$0")/.."

VENDOR="vendor"
rm -rf "$VENDOR"
mkdir -p "$VENDOR/lib"

# 1) Node (must match the ABI the addon was built for).
NODE_BIN="$(command -v node)"
cp -L "$NODE_BIN" "$VENDOR/node"
echo "node: $NODE_BIN ($("$VENDOR/node" -v))"

# 2) VLC plugin tree.
PLUGINS=""
for p in /usr/lib/vlc/plugins /usr/lib64/vlc/plugins /usr/lib/x86_64-linux-gnu/vlc/plugins; do
  [ -d "$p" ] && PLUGINS="$p" && break
done
[ -n "$PLUGINS" ] || { echo "ERROR: VLC plugins dir not found (is vlc installed?)"; exit 1; }
cp -a "$PLUGINS" "$VENDOR/vlc-plugins"
echo "plugins: $PLUGINS ($(find "$VENDOR/vlc-plugins" -name '*.so' | wc -l) modules)"

# 3) libvlc/libvlccore + transitive deps of the addon AND of every plugin.
EXCLUDE='^(ld-linux|libc|libm|libdl|libpthread|librt|libresolv|libgcc_s|libstdc\+\+|libGL|libGLX|libGLdispatch|libEGL|libOpenGL|libdrm|libgbm|libX11|libxcb|libwayland|libGLU|libgallium|libasound|libpulse|libpipewire|libjack)'

copy_deps() {
  ldd "$1" 2>/dev/null | awk '/=>/{print $3}' | while read -r lib; do
    [ -f "$lib" ] || continue
    base="$(basename "$lib")"
    echo "$base" | grep -Eq "$EXCLUDE" && continue
    [ -f "$VENDOR/lib/$base" ] && continue
    cp -L "$lib" "$VENDOR/lib/"
  done
}

copy_deps "native/build/Release/vlc.node"
find "$VENDOR/vlc-plugins" -name '*.so' | while read -r so; do copy_deps "$so"; done
# resolve deps-of-deps a few levels
for _ in 1 2 3; do
  for f in "$VENDOR"/lib/*.so*; do copy_deps "$f"; done
done

echo "bundled $(ls "$VENDOR/lib" | wc -l) libs into $VENDOR/lib"
du -sh "$VENDOR"
