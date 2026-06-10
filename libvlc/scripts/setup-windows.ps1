# Prepare the libVLC SDK so the native addon can be built on Windows.
# Input: the folder extracted from the OFFICIAL VLC Windows zip
#   (vlc-3.x.x-win64.zip from https://get.videolan.org/vlc/ → win64/)
#   which contains: sdk\include, sdk\lib (import libs!), libvlc.dll, libvlccore.dll, plugins\
# Output: native\vlc-sdk\{include, lib}  (used by binding.gyp)
#
# Unlike libmpv, no import-library generation is needed — VLC ships sdk\lib\libvlc.lib.
#
# Usage:  pwsh scripts/setup-windows.ps1 -VlcDir C:\path\to\vlc-3.0.21
param(
  [Parameter(Mandatory = $true)] [string]$VlcDir
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "native\vlc-sdk"

if (-not (Test-Path (Join-Path $VlcDir "sdk\include\vlc\vlc.h"))) {
  throw "No sdk\include\vlc\vlc.h under '$VlcDir' — extract the full official VLC win64 zip."
}

New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item -Recurse -Force (Join-Path $VlcDir "sdk\include") (Join-Path $dest "include")
Copy-Item -Recurse -Force (Join-Path $VlcDir "sdk\lib") (Join-Path $dest "lib")

Write-Host "VLC SDK ready in $dest (include, lib)"
Write-Host "Next: cd native && npx node-gyp rebuild   (builds vlc.node for Windows)"
Write-Host "Then: pwsh scripts/bundle-windows.ps1 -VlcDir $VlcDir"
