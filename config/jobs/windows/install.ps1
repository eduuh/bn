#!/usr/bin/env pwsh
# install.ps1 — register the ONE Windows Task Scheduler task bn needs on WSL: a keepalive.
#
# Why one task: systemd --user timers inside WSL only fire while WSL is running, and WSL shuts
# down when idle. This task boots WSL and runs `bn run ensure-tmux`, which starts a detached
# tmux server — and that server keeps WSL alive afterwards. With WSL held up, every other bn
# job (refresh-all, pr-observe, pr-queue, commit-notes) runs from systemd inside WSL
# (config/jobs/systemd/install.sh). So Windows schedules exactly one thing; systemd schedules the rest.
#
# Triggers: at logon + every 30 minutes, so a tmux/WSL death midday self-heals within half an
# hour instead of waiting for the next day. Idempotent: re-run to update (Register -Force).
#
# Usage (from PowerShell, substitute if your distro/user differ):
#   pwsh -File config/jobs/windows/install.ps1 -Distro Ubuntu-22.04 -WslUser you
[CmdletBinding()]
param(
    [string]$Distro  = $(if ($env:BN_WSL_DISTRO) { $env:BN_WSL_DISTRO } else { 'Ubuntu-22.04' }),
    [string]$WslUser = $(if ($env:BN_WSL_USER)   { $env:BN_WSL_USER }   else { $env:USERNAME }),
    [string]$TaskName = 'BN WSL Keepalive'
)
$ErrorActionPreference = 'Stop'

# Remove tasks this design supersedes, if a previous (hand-made) setup left them behind. Both
# are now driven differently: tmux by this keepalive, commit-notes by a systemd timer.
foreach ($old in 'BN Daily tmux', 'BN Commit Notes') {
    if (Get-ScheduledTask -TaskName $old -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $old -Confirm:$false
        Write-Host "Removed superseded task: $old"
    }
}

$bn     = "/home/$WslUser/.config/bn/bn"
$action = New-ScheduledTaskAction -Execute 'wsl.exe' `
            -Argument "-d $Distro -u $WslUser -- $bn run ensure-tmux"

# At this user's logon, then re-check every 30 min (indefinitely) so a dead tmux/WSL self-heals.
# An all-users AtLogOn trigger requires elevation even though the task itself runs as this user.
$atLogon = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$repeat  = New-ScheduledTaskTrigger -Once -At (Get-Date) `
            -RepetitionInterval (New-TimeSpan -Minutes 30)

$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable `
            -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
            -MultipleInstances IgnoreNew

Register-ScheduledTask -TaskName $TaskName -Action $action `
    -Trigger $atLogon, $repeat -Settings $settings -Force | Out-Null

Write-Host "Registered '$TaskName' -> wsl.exe -d $Distro -u $WslUser -- $bn run ensure-tmux"
Write-Host "Triggers: at logon + every 30 min. Next, inside WSL run: sh config/jobs/systemd/install.sh"
(Get-ScheduledTask -TaskName $TaskName | Get-ScheduledTaskInfo) |
    Format-List TaskName, State, NextRunTime, LastRunTime, LastTaskResult
