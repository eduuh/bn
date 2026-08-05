# Link this repo into ~/.bin/tmux-workflow so psmux can find the helpers.
# Uses a directory JUNCTION, which (unlike a symlink) needs no admin or
# Developer Mode on Windows.
$ErrorActionPreference = 'Stop'
$repo   = $PSScriptRoot
$binDir = Join-Path $HOME '.bin'
$link   = Join-Path $binDir 'tmux-workflow'

New-Item -ItemType Directory -Force -Path $binDir | Out-Null
if (Test-Path $link) { Remove-Item $link -Recurse -Force }
New-Item -ItemType Junction -Path $link -Target $repo | Out-Null
Write-Host "Linked $link -> $repo"
