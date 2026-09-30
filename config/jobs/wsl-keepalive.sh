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
#
# Install also stops Windows sleeping or hibernating on AC power: a sleeping machine drops every
# tunnel whatever holds the distro. Battery is left alone, so an unplugged laptop still sleeps.
# BN_WSL_AWAKE=0 leaves the power plan as it is.
#
# `sh wsl-keepalive.sh boot` adds a second task, `bn-wsl-keepalive-boot`, that starts the same
# keeper when Windows starts, before anyone signs in, so a host that rebooted overnight (Windows
# Update) is back without a sign-in. It runs as you without a stored password (S4U) and needs
# admin rights once to register: it asks through a UAC prompt, then starts the task straight away
# to prove it works, removing it again if Windows will not start it (S4U needs a domain controller
# to vouch for the account; on an Entra-joined laptop with a domain account it never ran).
# Both keepers may run at once; they do the same thing.
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

BOOT_TASK=bn-wsl-keepalive-boot

keeper_running() {
  ps "Get-CimInstance Win32_Process -Filter \"Name='wsl.exe'\" | Where-Object { \$_.CommandLine -like '*tunnel-auto.sh*sleep infinity*' } | Measure-Object | Select-Object -ExpandProperty Count" | tail -n 1 | tr -dc '0-9'
}

# ac_sleep -- idle seconds before Windows sleeps on AC power (0 = never); empty when it cannot be
# read. The AC value is the second to last 0x line of the query, whatever the display language.
ac_sleep() {
  _hex=$(ps 'powercfg /query SCHEME_CURRENT SUB_SLEEP STANDBYIDLE' | awk '/0x[0-9a-fA-F]+$/ {a = b; b = $NF} END {print a}')
  [ -n "$_hex" ] && printf '%d\n' "$_hex"
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
    if [ "${BN_WSL_AWAKE:-1}" = 1 ] && [ "$(ac_sleep)" != 0 ]; then
      ps 'powercfg /change standby-timeout-ac 0; powercfg /change hibernate-timeout-ac 0' >/dev/null
      if [ "$(ac_sleep)" = 0 ]; then
        say "Windows no longer sleeps on AC power (undo: powercfg.exe /change standby-timeout-ac 30)"
      else
        say "could not turn off sleep on AC power; set it in the Windows power settings"
      fi
    fi
    ;;
  boot)
    # The elevated half is a script in the Windows temp folder; it writes its result to a file,
    # since an elevated window's output does not come back to this terminal.
    wtemp=$(ps '[IO.Path]::GetTempPath()' | tail -n 1)
    [ -n "$wtemp" ] || die "could not find the Windows temp folder"
    utemp=$(wslpath -u "$wtemp")
    user=$(ps '$env:USERDOMAIN + "\" + $env:USERNAME' | tail -n 1)
    rm -f "$utemp/bn-boot.result"
    cat > "$utemp/bn-boot.ps1" <<EOF
\$out = '${wtemp}bn-boot.result'
try {
  \$a = New-ScheduledTaskAction -Execute 'wsl.exe' -Argument '-d $DISTRO --exec /bin/sh -lc "$KEEPER"'
  \$t = New-ScheduledTaskTrigger -AtStartup
  \$p = New-ScheduledTaskPrincipal -UserId '$user' -LogonType S4U -RunLevel Limited
  \$s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit 0 -MultipleInstances IgnoreNew
  Register-ScheduledTask -TaskName '$BOOT_TASK' -Action \$a -Trigger \$t -Principal \$p -Settings \$s -Description 'bn: start WSL ($DISTRO) and its tmux/tunnels at boot, before sign-in' -Force -ErrorAction Stop | Out-Null
  Start-ScheduledTask -TaskName '$BOOT_TASK' -ErrorAction Stop
  Start-Sleep -Seconds 15
  \$i = Get-ScheduledTaskInfo -TaskName '$BOOT_TASK'
  \$st = (Get-ScheduledTask -TaskName '$BOOT_TASK').State
  \$res = "\$st 0x{0:x}" -f \$i.LastTaskResult
  # A task Windows will not start (S4U needs a domain controller to vouch for the account, which an
  # Entra-joined laptop signed in with a domain account does not have) is removed again rather
  # than left looking registered.
  if (\$res -ne 'Running 0x41301') { Unregister-ScheduledTask -TaskName '$BOOT_TASK' -Confirm:\$false }
  \$res | Set-Content \$out
} catch { 'error: ' + \$_.Exception.Message | Set-Content \$out }
EOF
    say "asking Windows for admin rights (a UAC prompt) to register $BOOT_TASK ..."
    ps "Start-Process powershell.exe -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','${wtemp}bn-boot.ps1'" >/dev/null \
      || die "no admin rights (UAC declined or not an administrator); nothing was changed"
    res=$(tr -d '\r' < "$utemp/bn-boot.result" 2>/dev/null)
    rm -f "$utemp/bn-boot.ps1" "$utemp/bn-boot.result"
    # 0x41301 is "the task is running": the keeper started under S4U and is holding WSL.
    case "$res" in
      "Running 0x41301") say "task $BOOT_TASK registered and its keeper is running; it starts at every boot" ;;
      '') die "the elevated step left no result" ;;
      *) die "Windows did not start $BOOT_TASK ($res; 0x41303 means it never ran), so it was removed again. On this machine a host still needs a sign-in after a reboot" ;;
    esac
    ;;
  status)
    st=$(ps "(Get-ScheduledTask -TaskName '$TASK' -ErrorAction SilentlyContinue).State" | tail -n 1)
    bt=$(ps "\$t = Get-ScheduledTask -TaskName '$BOOT_TASK' -ErrorAction SilentlyContinue; if (\$t) { \$t.State.ToString() + \$(if ((Get-ScheduledTaskInfo -TaskName '$BOOT_TASK').LastTaskResult -eq 0x41303) { ' (never ran)' }) }" | tail -n 1)
    sl=$(ac_sleep)
    case "$sl" in '') sl=unknown ;; 0) sl=never ;; *) sl="after ${sl}s idle" ;; esac
    say "task: ${st:-not registered}; boot task: ${bt:-not registered (sh $0 boot)}; keeper processes: $(keeper_running); sleep on AC power: $sl"
    ;;
  remove)
    ps "Unregister-ScheduledTask -TaskName '$TASK' -Confirm:\$false" >/dev/null && say "task removed"
    ;;
  *) die "usage: wsl-keepalive.sh [install|boot|status|remove]" ;;
esac
