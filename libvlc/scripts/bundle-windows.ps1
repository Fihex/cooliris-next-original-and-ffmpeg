# Collect the self-contained Windows runtime into vendor\:
#   node.exe                       — runs the vlc host (must match the addon's ABI)
#   libvlc.dll + libvlccore.dll    — from the official VLC zip
#   plugins\                       — VLC's plugin tree (demuxers/decoders/outputs);
#                                    found at runtime via VLC_PLUGIN_PATH=vendor\plugins
# electron-builder ships vendor\ as extraResources. No system VLC/Node needed at runtime.
#
# Usage:  pwsh scripts/bundle-windows.ps1 -VlcDir C:\path\to\vlc-3.0.21
param(
  [Parameter(Mandatory = $true)] [string]$VlcDir
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$vendor = Join-Path $root "vendor"

Remove-Item -Recurse -Force $vendor -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $vendor | Out-Null

# Node (the one the addon was built against).
$node = (Get-Command node).Source
Copy-Item -Force $node (Join-Path $vendor "node.exe")

Copy-Item -Force (Join-Path $VlcDir "libvlc.dll") $vendor
Copy-Item -Force (Join-Path $VlcDir "libvlccore.dll") $vendor
Copy-Item -Recurse -Force (Join-Path $VlcDir "plugins") (Join-Path $vendor "plugins")

# vlc-cache-gen builds plugins.dat (the plugin index) so libVLC doesn't rescan every DLL
# on launch (~30s on Windows). The electron-builder afterPack hook runs it against the
# final packed vendor\plugins; at runtime vlc.ts copies the plugins to a writable per-user
# dir preserving timestamps, which keeps that cache valid → fast first open.
$cacheGen = Join-Path $VlcDir "vlc-cache-gen.exe"
if (Test-Path $cacheGen) {
  Copy-Item -Force $cacheGen $vendor
  Write-Host "bundled vlc-cache-gen.exe (afterPack will build plugins.dat)"
} else {
  Write-Warning "vlc-cache-gen.exe not found in $VlcDir — first open will be slow (no plugin cache)"
}

Write-Host "vendor\ ready: node.exe + libvlc.dll + libvlccore.dll + plugins\ + vlc-cache-gen.exe"
Write-Host ("node: " + (& (Join-Path $vendor 'node.exe') -v))
