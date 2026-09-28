#!/bin/sh
# wsl-keepalive -- keep this WSL distro, and a tmux server with the tunnels, running on Windows.
#
# Windows shuts a WSL distro down once no Windows-side `wsl.exe` process holds it (closing the last
# terminal, or a Cloud PC session nobody is looking at), which takes tmux, the tunnels and every
# build with it. This registers a Windows scheduled task, `bn-wsl-keepalive`, that runs at sign-in
# and starts a hidden `wsl.exe` that never exits. Inside WSL it runs tunnel-auto (tunnels and the
# `bn-tunnels` tmux session come up), makes sure a `main` tmux session exists, then sleeps forever:
# that one process is what keeps the distro alive.
#
# Run it once per machine, from inside WSL:  bn run wsl-keepalive   (or: sh wsl-keepalive.sh)
# Idempotent: re-running updates the task and starts it if the keeper is not already running.
# `sh wsl-keepalive.sh status` reports the task and whether the keeper is running;
# `sh wsl-keepalive.sh remove` unregisters the task (the running keeper stays until WSL stops).
#
# User-level only: a per-user AtLogOn task needs no admin rights. It runs while you are signed in
# to Windows, and a Cloud PC keeps that session through disconnects; after a reboot it starts at the
# next sign-in. Env: BN_WSL_DISTRO (default: $WSL_DISTRO_NAME, else the default distro).
set -u

ACTION="${1:-${BN_WSL:-install}}"
say() { echo "wsl-keepalive: $*"; }
die() { echo "wsl-keepalive: $*" >&2; exit 1; }

grep -qi microsoft /proc/version 2>/dev/null || die "not running inside WSL"
# Over ssh (and under systemd) WSL does not append the Windows PATH, so name PowerShell directly.
PS=$(command -v powershell.exe 2>/dev/null || echo /mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe)
[ -x "$PS" ] || die "powershell.exe not found (WSL interop off?)"
ps() { "$PS" -NoProfile -NonInteractive -Command "$1" 2>&1 | tr -d '\r'; }

DISTRO="${BN_WSL_DISTRO:-${WSL_DISTRO_NAME:-}}"
if [ -z "$DISTRO" ]; then
  # `wsl.exe -l` prints UTF-16; the default distro is the line marked with `*` in `-l -v`.
  DISTRO=$(ps 'wsl.exe -l -v' | tr -d '\000' | awk '$1 == "*" {print $2; exit}')
fi
[ -n "$DISTRO" ] || die "could not tell which WSL distro this is; set BN_WSL_DISTRO"
case "$DISTRO" in *[!A-Za-z0-9._-]*) die "unexpected distro name '$DISTRO'" ;; esac

TASK=bn-wsl-keepalive
# The keeper, as it runs inside WSL. A login shell so bn and nvm are on PATH; `exec sleep` keeps
# exactly one long-lived process holding the distro.
# `main` first, then a pause for tmux-continuum's restore, then the tunnels: whichever session
# exists first is the one the restore reuses, and a `bn-tunnels` made first collected the restored
# `bn` agent windows (2026-09-27).
KEEPER="tmux has-session -t main 2>/dev/null || tmux new-session -d -s main; sleep 60; sh \$HOME/.config/bn/repo/config/jobs/tunnel-auto.sh; exec sleep infinity"

keeper_running() {
  ps "Get-CimInstance Win32_Process -Filter \"Name='wsl.exe'\" | Where-Object { \$_.CommandLine -like '*tunnel-auto.sh*sleep infinity*' } | Measure-Object | Select-Object -ExpandProperty Count" | tail -n 1 | tr -dc '0-9'
}

case "$ACTION" in
  install)
    # `conhost.exe --headless` runs wsl.exe without a console window on the desktop. The whole
    # argument is one single-quoted PowerShell string ($HOME and the quotes reach wsl.exe as-is;
    # KEEPER holds no single quote).
    out=$(ps "
      \$a = New-ScheduledTaskAction -Execute 'conhost.exe' -Argument '--headless wsl.exe -d $DISTRO --exec /bin/sh -lc \"$KEEPER\"'
      \$t = New-ScheduledTaskTrigger -AtLogOn -User (\$env:USERDOMAIN + '\\' + \$env:USERNAME)
      \$s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit 0 -MultipleInstances IgnoreNew
      Register-ScheduledTask -TaskName '$TASK' -Action \$a -Trigger \$t -Settings \$s -Description 'bn: keep WSL ($DISTRO) and its tmux/tunnels running' -Force | Out-Null
      'registered'")
    printf '%s\n' "$out" | grep -q registered || die "could not register the task: $out"
    say "task $TASK registered (at sign-in, distro $DISTRO)"
    if [ "$(keeper_running)" -gt 0 ] 2>/dev/null; then
      say "keeper already running"
    else
      ps "Start-ScheduledTask -TaskName '$TASK'" >/dev/null
      i=0; until [ "$(keeper_running)" -gt 0 ] 2>/dev/null || [ "$i" -ge 20 ]; do sleep 1; i=$((i + 1)); done
      [ "$(keeper_running)" -gt 0 ] 2>/dev/null && say "keeper started" || die "task ran but no keeper wsl.exe is running"
    fi
    ;;
  status)
    st=$(ps "(Get-ScheduledTask -TaskName '$TASK' -ErrorAction SilentlyContinue).State" | tail -n 1)
    say "task: ${st:-not registered}; keeper processes: $(keeper_running)"
    ;;
  remove)
    ps "Unregister-ScheduledTask -TaskName '$TASK' -Confirm:\$false" >/dev/null && say "task removed"
    ;;
  *) die "usage: wsl-keepalive.sh [install|status|remove]" ;;
esac
