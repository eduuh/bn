#!/usr/bin/env pwsh
# register-bn-tasks.ps1 — idempotent, repeatable registration of bn's scheduled jobs as
# Windows Task Scheduler tasks that drive WSL via wsl.exe. Re-run any time, on any machine,
# to converge to the same state (Register-ScheduledTask -Force overwrites in place).
#
# A fixed target timezone (a constant UTC offset, no DST — set via -TzOffsetHours) is the
# target. Triggers are written in UTC (the target time minus the offset) and stored with an
# explicit offset, so the boundary is an absolute instant, not a naive local "07:00" that
# would fire at 07:00 in the host's own timezone. On a host whose clock matches the target
# offset this is exact. CAVEAT: on a DST-observing host Windows recurs a calendar trigger by
# local time-of-day, so the fire time can drift by 1h across that host's DST switch. If you
# run this on such a host and need minute-exact target times year-round, gate inside the job
# instead (wake often; act only when `TZ=<your-tz> date +%H:%M` is in a target slot).
#
# The WSL side — the `bn run <job>` scripts under jobs/ — is installed by install.sh; this
# script only wires the Windows scheduler to them. Distro and WSL user auto-detect; override
# with -Distro / -WslUser or $env:BN_WSL_DISTRO / $env:BN_WSL_USER.
[CmdletBinding()]
param(
  [string]$Distro        = $env:BN_WSL_DISTRO,
  [string]$WslUser       = $env:BN_WSL_USER,
  [int]   $TzOffsetHours  = 3
)
$ErrorActionPreference = 'Stop'

# wsl.exe emits UTF-16 with stray NULs when captured; strip them.
function Clean([string]$s) { ($s -replace "`0", "").Trim() }

if (-not $Distro) {
  $Distro = wsl.exe -l -q |
    ForEach-Object { Clean $_ } |
    Where-Object { $_ -and $_ -ne 'docker-desktop' } |
    Select-Object -First 1
}
if (-not $Distro) { throw "Could not detect a WSL distro; pass -Distro." }

if (-not $WslUser) { $WslUser = Clean (wsl.exe -d $Distro -- whoami) }
if (-not $WslUser) { throw "Could not detect the WSL user; pass -WslUser." }

$bn = "/home/$WslUser/.bin/bn"
Write-Host "Distro=$Distro  WslUser=$WslUser  bn=$bn"

# A target-timezone "HH:mm" -> a UTC StartBoundary string (trailing 'Z' = UTC; the date part
# is just "not before" and is irrelevant to a daily recurrence).
function Local-ToUtcBoundary([string]$hhmm) {
  $t = [datetime]::ParseExact("2024-01-01 $hhmm", "yyyy-MM-dd HH:mm", $null).AddHours(-$TzOffsetHours)
  $t.ToString("yyyy-MM-ddTHH:mm:ss") + "Z"
}

function Register-BnTask([string]$Name, [string]$Desc, [string]$Job, [string[]]$LocalTimes) {
  $triggers = ($LocalTimes | ForEach-Object {
    $b = Local-ToUtcBoundary $_
    "    <CalendarTrigger>`n      <StartBoundary>$b</StartBoundary>`n      <Enabled>true</Enabled>`n      <ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay>`n    </CalendarTrigger>"
  }) -join "`n"

  $taskArgs = "-d $Distro -u $WslUser -- $bn run $Job"
  $xml = @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>$Desc</Description>
  </RegistrationInfo>
  <Triggers>
$triggers
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <StartWhenAvailable>true</StartWhenAvailable>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT10M</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>wsl.exe</Command>
      <Arguments>$taskArgs</Arguments>
    </Exec>
  </Actions>
</Task>
"@
  Register-ScheduledTask -TaskName $Name -Xml $xml -Force | Out-Null
  Write-Host "Registered '$Name'  ($($LocalTimes -join ', ') @ UTC+$TzOffsetHours)"
}

Register-BnTask -Name 'BN Daily tmux' `
  -Desc 'Wake WSL and ensure a repository tmux session is up (bn run ensure-tmux).' `
  -Job  'ensure-tmux' -LocalTimes @('07:00')

Register-BnTask -Name 'BN Commit Notes' `
  -Desc 'Commit and push the branch-notes repos (bn run commit-notes).' `
  -Job  'commit-notes' -LocalTimes @('07:00', '12:00', '13:00', '16:00', '18:00')

Write-Host "Done. Triggers are in UTC, so they fire at the target-timezone (UTC+$TzOffsetHours) times on any host timezone."
