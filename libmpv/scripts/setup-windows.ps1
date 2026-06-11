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

# Generate an MSVC import library (mpv.lib) for libmpv-2.dll.
# Some dev packages ship a .def; newer shinchiro/zhongfly ones ship only a MinGW
# libmpv.dll.a (which MSVC's linker can't consume), so synthesise a .def from the
# DLL's export table via dumpbin in that case. (Requires the MSVC env: lib.exe + dumpbin.)
$libOut = Join-Path $dest "mpv.lib"
$def = Join-Path $MpvDev "mpv.def"
if (-not (Test-Path $def)) { $def = Join-Path $MpvDev "libmpv-2.def" }
if (-not (Test-Path $def)) {
  Write-Host "No .def in package; generating one from libmpv-2.dll exports via dumpbin"
  $def = Join-Path $dest "libmpv-2.def"
  $dump = & dumpbin /nologo /exports (Join-Path $dest "libmpv-2.dll")
  if ($LASTEXITCODE -ne 0) { throw "dumpbin failed ($LASTEXITCODE)" }
  $names = $dump |
    Select-String -Pattern '^\s+\d+\s+[0-9A-Fa-f]+\s+[0-9A-Fa-f]+\s+(\S+)' |
    ForEach-Object { $_.Matches[0].Groups[1].Value }
  if (-not $names) { throw "no exports parsed from libmpv-2.dll" }
  "EXPORTS" | Set-Content -Encoding ascii $def
  $names | Add-Content -Encoding ascii $def
  Write-Host "Generated $def with $($names.Count) exports"
}
& lib "/def:$def" "/name:libmpv-2.dll" "/out:$libOut" /machine:x64
if ($LASTEXITCODE -ne 0) { throw "lib.exe failed to create mpv.lib ($LASTEXITCODE)" }
if (-not (Test-Path $libOut)) { throw "mpv.lib was not created" }

Write-Host "libmpv dev ready in $dest (include, mpv.lib, libmpv-2.dll)"
Write-Host "Next: cd native && npx node-gyp rebuild   (builds mpv.node for Windows)"
