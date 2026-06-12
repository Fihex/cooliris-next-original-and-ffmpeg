@echo off
rem Launch Cooliris Next (libmpv) with native embedded video — the two-window mode where
rem mpv renders hardware-decoded video directly into the window at full fps. Just double-click
rem this file. Running "Cooliris Next.exe" directly (without this) starts WITHOUT embed mode.
set COOLIRIS_MPV_EMBED=1
start "" "%~dp0Cooliris Next.exe"
