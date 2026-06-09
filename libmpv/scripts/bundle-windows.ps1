# Collect the self-contained Windows runtime into vendor\:
#   - node.exe   (runs the mpv host; must match the ABI the addon was built for)
#   - libmpv-2.dll  (self-contained — ffmpeg is baked into the shinchiro libmpv build)
# electron-builder ships vendor\ as extraResources; at runtime the host runs under
# vendor\node.exe with vendor\ on PATH so libmpv-2.dll loads. No system mpv/Node needed.
#
# Run AFTER scripts/setup-windows.ps1 (which produced native\mpv-dev\libmpv-2.dll).
# Usage:  pwsh scripts/bundle-windows.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$vendor = Join-Path $root "vendor"

Remove-Item -Recurse -Force $vendor -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $vendor | Out-Null

# Node (the one the addon was built against — keep node-gyp + this consistent).
$node = (Get-Command node).Source
Copy-Item -Force $node (Join-Path $vendor "node.exe")

# Self-contained libmpv (next to node.exe so the addon finds it via PATH).
Copy-Item -Force (Join-Path $root "native\mpv-dev\libmpv-2.dll") (Join-Path $vendor "libmpv-2.dll")

Write-Host "vendor\ ready: node.exe + libmpv-2.dll"
Write-Host ("node: " + (& (Join-Path $vendor 'node.exe') -v))
