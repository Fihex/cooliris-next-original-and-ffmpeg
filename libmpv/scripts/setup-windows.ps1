# Prepare the libmpv dev files so the native addon can be built on Windows.
# Input: the folder you extracted from a libmpv *dev* package
#   (mpv-dev-x86_64-*.7z from https://sourceforge.net/projects/mpv-player-windows/files/libmpv/)
#   which contains: include\mpv\*.h, libmpv-2.dll, mpv.def
# Output: native\mpv-dev\{include, mpv.lib, libmpv-2.dll}  (used by binding.gyp + bundling)
#
# Requires Visual Studio Build Tools (for lib.exe) on PATH — run from a
# "x64 Native Tools Command Prompt" / Developer PowerShell so lib.exe resolves.
#
# Usage:  pwsh scripts/setup-windows.ps1 -MpvDev C:\path\to\mpv-dev-x86_64-vXXXX
param(
  [Parameter(Mandatory = $true)] [string]$MpvDev
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "native\mpv-dev"

New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item -Recurse -Force (Join-Path $MpvDev "include") (Join-Path $dest "include")
Copy-Item -Force (Join-Path $MpvDev "libmpv-2.dll") (Join-Path $dest "libmpv-2.dll")

# Generate the import library (mpv.lib) from the shipped .def.
$def = Join-Path $MpvDev "mpv.def"
if (-not (Test-Path $def)) { $def = Join-Path $MpvDev "libmpv-2.def" }
& lib "/def:$def" "/name:libmpv-2.dll" "/out:$(Join-Path $dest 'mpv.lib')" /machine:x64

Write-Host "libmpv dev ready in $dest (include, mpv.lib, libmpv-2.dll)"
Write-Host "Next: cd native && npx node-gyp rebuild   (builds mpv.node for Windows)"
