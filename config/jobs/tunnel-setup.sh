#!/bin/sh
# tunnel-setup -- put this machine on the tunnel mesh in one command:  bn run tunnel-setup
#
# tunnel-auto enrols a machine by itself once everything it needs is there; this installs what is
# missing and does the steps that need a person, so a new machine takes one run. Idempotent: every
# step checks first and says ok / done / NEEDS YOU, so re-running it is how you check a machine.
#
# Steps:
#   1. packages: sshd, tmux, nc, curl, ss and gh (apt-get; sudo asks for a password at a terminal);
#   2. the devtunnel CLI, downloaded into ~/bin;
#   3. the Dev Tunnels sign-in (device code at a terminal; same account on every machine);
#   4. a GitHub account whose SSH keys may log in (gh's account, or bn.tunnelGithubUser);
#   5. linger, so the timers run with nobody signed in;
#   6. bn's timers, when the tunnel-auto one is not registered yet;
#   7. on WSL: the Windows keeper (wsl-keepalive), and on a host the boot task that brings it back
#      after a reboot before anyone signs in (one UAC prompt, at a terminal only);
#   8. host this machine and connect it to every other host.
# A machine nobody needs to reach: `BN_TUNNEL_HOST=0 bn run tunnel-setup` makes it connect-only
# (BN_TUNNEL_HOST=1 undoes it), and sshd is not installed.
# Local network mode is opt-in: `BN_TUNNEL_LAN=1 bn run tunnel-setup` turns it on for this machine
# (BN_TUNNEL_LAN=0 turns it off) and, on WSL, checks the two
# things Windows needs for it: mirrored networking and the firewall rule for the port.
# It ends with the things left for a person, if any, and exits 1 when there are some.
#
# Unattended (no terminal) nothing prompts: a step that needs a password or a sign-in is listed
# under NEEDS YOU with the command to run.
set -u

dir=$(cd "$(dirname "$0")" && pwd -P)
ME=$(hostname -s 2>/dev/null || hostname)
export PATH="$HOME/.local/bin:$HOME/bin:$PATH" XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
. "$dir/lib/tunnel.sh"

ok()    { echo "  ok    $*"; }
done_() { echo "  done  $*"; }
needs() { echo "  NEEDS YOU  $*"; left="$left
  - $*"; }
left=""
# as_root <command...> -- sudo that asks only when someone is there to answer.
as_root() { if bn_tunnel_interactive; then sudo "$@"; else sudo -n "$@" < /dev/null; fi; }
echo "tunnel-setup: $ME"
[ -z "$(bn_tunnel_nick)" ] || echo "  nickname $(bn_tunnel_nick)"

case "${BN_TUNNEL_HOST:-}" in
  0) bn_tunnel_set tunnelHost off ;;
  1) bn_tunnel_set tunnelHost "" ;;
esac
case "${BN_TUNNEL_AUTO:-}" in
  0) bn_tunnel_set tunnelAuto off ;;
  1) bn_tunnel_set tunnelAuto "" ;;
esac
# A nickname for this machine: what bn tunnels, the tmux status bar and `ssh <nick>` use on every
# machine. The hostname stays the real name. BN_TUNNEL_NICK= (empty) clears it.
if [ -n "${BN_TUNNEL_NICK+set}" ]; then
  n=$BN_TUNNEL_NICK
  if [ -z "$n" ] || printf '%s' "$n" | grep -qE '^[A-Za-z0-9][A-Za-z0-9-]{0,19}$'; then
    bn_tunnel_set tunnelNick "$n"
    bn_tunnel_record_nick "$n" || true
  else
    needs "a nickname is 1-20 letters, digits or dashes: '$BN_TUNNEL_NICK' is not"
  fi
fi
host_off=0
case "$(bn_tunnel_flag tunnelHost)" in off|false|no|0) host_off=1 ;; esac

tmpd=$(mktemp -d); trap 'rm -rf "$tmpd"' EXIT

# --- 1. packages --------------------------------------------------------------------
has_sshd() { bn_tunnel_sshd >/dev/null; }
pkgs=""
[ "$host_off" = 1 ] || has_sshd || pkgs="$pkgs openssh-server"
command -v tmux >/dev/null 2>&1 || pkgs="$pkgs tmux"
command -v nc   >/dev/null 2>&1 || pkgs="$pkgs netcat-openbsd"
command -v curl >/dev/null 2>&1 || pkgs="$pkgs curl"
command -v ss   >/dev/null 2>&1 || pkgs="$pkgs iproute2"
command -v gh   >/dev/null 2>&1 || pkgs="$pkgs gh"
# devtunnel is a .NET program and needs libicu; without it bn wraps it to run without (lib/tunnel.sh).
command -v ldconfig >/dev/null 2>&1 && ! ldconfig -p 2>/dev/null | grep -q libicuuc && pkgs="$pkgs libicu-dev"
if [ -z "$pkgs" ]; then
  ok "packages (sshd tmux nc curl ss gh)"
elif ! command -v apt-get >/dev/null 2>&1; then
  needs "install with this system's package manager:$pkgs"
else
  # A machine fresh from an image has no package lists yet, so a failed install gets one update.
  # shellcheck disable=SC2086 -- $pkgs is a list, split on purpose
  if as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y $pkgs >/dev/null 2>&1 \
      || { as_root apt-get update >/dev/null 2>&1 && as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y $pkgs >/dev/null 2>&1; }; then
    done_ "installed$pkgs"
  else
    # sshd does not need root: without sudo, unpack bn's own copy and install the rest later.
    if [ "$host_off" = 0 ] && ! has_sshd && bn_tunnel_sshd_unpack; then
      done_ "sshd unpacked for this user ($BN_TUNNEL_SSHD_HOME), no root needed"
      pkgs=$(printf '%s\n' $pkgs | grep -vx openssh-server | tr '\n' ' ')
    fi
    [ -z "$(printf '%s' "$pkgs" | tr -d ' ')" ] || needs "sudo apt-get install -y $pkgs"
  fi
fi

# --- 2 + 3. devtunnel CLI and sign-in --------------------------------------------------
DT=""
if DT=$(bn_tunnel_devtunnel 2>"$tmpd/dt.err"); then
  ok "devtunnel CLI ($DT)"
  if bn_tunnel_signed_in "$DT"; then
    ok "Dev Tunnels sign-in ($("$DT" user show 2>/dev/null | head -n 1))"
  elif bn_tunnel_login "$DT" 2>/dev/null; then
    done_ "signed in to Dev Tunnels"
  else
    needs "$DT user login -g -d   (the same account as your other machines)"
  fi
else
  DT=""
  needs "the devtunnel CLI could not be installed into ~/bin: $(tail -n 1 "$tmpd/dt.err" 2>/dev/null)"
fi

# --- 4. whose keys may log in -----------------------------------------------------------
GH_USER=$(bn_tunnel_github_user)
if [ -n "$GH_USER" ]; then
  ok "SSH keys trusted from github.com/$GH_USER"
else
  needs "gh auth login   (or: git config --global bn.tunnelGithubUser <account>)"
fi

# --- 5. linger ----------------------------------------------------------------------
if command -v loginctl >/dev/null 2>&1; then
  if [ "$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null)" = yes ]; then
    ok "linger"
  elif loginctl enable-linger "$(id -un)" < /dev/null 2>/dev/null || as_root loginctl enable-linger "$(id -un)" 2>/dev/null; then
    done_ "linger enabled"
  else
    needs "sudo loginctl enable-linger $(id -un)"
  fi
fi

# --- 6. timers ----------------------------------------------------------------------
if systemctl --user list-timers --all 2>/dev/null | grep -q 'bn-tunnel-auto'; then
  ok "tunnel-auto timer"
elif command -v bn >/dev/null 2>&1 && bn cron register >/dev/null 2>&1; then
  done_ "bn timers registered"
else
  needs "bn cron register   (run it by hand to see why it failed)"
fi

# --- 7. WSL keeper ------------------------------------------------------------------
if grep -qi microsoft /proc/version 2>/dev/null; then
  if sh "$dir/wsl-keepalive.sh" install >/dev/null 2>&1; then
    ks=$(sh "$dir/wsl-keepalive.sh" status 2>/dev/null | sed 's/^wsl-keepalive: //')
    ok "WSL keeper ($ks)"
    # A host must also come back after a reboot nobody signs in after (Windows Update overnight).
    # That task needs admin rights once, so it is asked for only at a terminal.
    # A machine where Windows would not start the boot task (wsl-keepalive boot says why) is not
    # asked again: it needs a sign-in after a reboot, and re-running setup would only repeat the
    # UAC prompt (2026-10-01: the Entra-joined laptop and cloud-pc-AB12C both refused S4U).
    case "$ks" in *"boot task: not registered"*)
      if [ -f "$HOME/.config/bn/boot-task.unsupported" ]; then
        echo "  ...   no boot task: Windows will not start one for this account, so after a reboot this host waits for a sign-in"
      elif [ "$host_off" = 0 ] && has_sshd; then
        if bn_tunnel_interactive && sh "$dir/wsl-keepalive.sh" boot; then
          done_ "boot task: this host comes back after a reboot, before anyone signs in"
        else
          needs "sh $dir/wsl-keepalive.sh boot   (one UAC prompt; brings this host back after a reboot before sign-in)"
        fi
      fi ;;
    esac
  else
    needs "sh $dir/wsl-keepalive.sh install"
  fi
fi

# --- local network mode (opt-in) ----------------------------------------------------
case "${BN_TUNNEL_LAN:-}" in
  1) bn_tunnel_set tunnelLan on ;;
  0) bn_tunnel_set tunnelLan "" ;;
esac
lan=0
case "$(bn_tunnel_flag tunnelLan)" in on|true|yes|1) lan=1 ;; esac

# --- 8. host and connect ------------------------------------------------------------
out=$(mktemp)
if [ -z "$DT" ] || ! bn_tunnel_signed_in "$DT"; then
  echo "  ...   hosting and connecting wait for the sign-in; tunnel-auto does both within 5 minutes of it"
else
  if [ "$host_off" = 1 ]; then
    sh "$dir/tunnel-host.sh" < /dev/null > "$out" 2>&1
    ok "connect-only: not hosting (bn.tunnelHost off)"
  elif ! has_sshd; then
    echo "  ...   not hosting: no sshd, so other machines cannot connect back to this one"
  elif sh "$dir/tunnel-host.sh" < /dev/null > "$out" 2>&1; then
    ok "hosting ($(grep -o 'sshd [a-z ]*on [0-9.]*:[0-9]*' "$out" | head -n 1))"
  else
    needs "tunnel-host: $(tail -n 1 "$out")"
  fi
  if sh "$dir/tunnel-connect.sh" < /dev/null > "$out" 2>&1; then
    ok "connected ($(grep -c ': ssh ' "$out") other hosts on the account)"
  else
    needs "tunnel-connect: $(tail -n 1 "$out")"
  fi
fi
rm -f "$out"

if [ "$lan" = 1 ] && [ "$host_off" = 1 ]; then
  echo "  ...   local network mode is on but this machine does not host (bn.tunnelHost off)"
elif [ "$lan" = 1 ]; then
  port=$(awk '$1 == "Port" {print $2; exit}' "$HOME/.ssh/bn-sshd/sshd_config" 2>/dev/null)
  [ -n "$port" ] || port=$(bn_tunnel_port_for "$ME")
  PS=$(command -v powershell.exe 2>/dev/null || echo /mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe)
  if ! has_sshd; then
    echo "  ...   local network mode is on and starts once sshd is installed"
  elif ! grep -qi microsoft /proc/version 2>/dev/null; then
    ok "local network mode (port $port on $(bn_tunnel_lan_ip))"
  elif [ "$(wslinfo --networking-mode 2>/dev/null)" != mirrored ]; then
    needs "local network mode needs WSL mirrored networking: networkingMode=mirrored under [wsl2] in %UserProfile%\\.wslconfig, then wsl --shutdown"
  elif [ "$("$PS" -NoProfile -NonInteractive -Command "(Get-NetFirewallHyperVRule -Name 'bn-ssh-$port' -ErrorAction SilentlyContinue | Measure-Object).Count" 2>/dev/null | tr -dc '0-9')" = 1 ]; then
    ok "local network mode (port $port on $(bn_tunnel_lan_ip), allowed through Windows)"
  else
    needs "allow port $port through Windows, from an elevated PowerShell: New-NetFirewallHyperVRule -Name bn-ssh-$port -DisplayName 'bn ssh $port' -Direction Inbound -VMCreatorId '{40E0AC32-46A5-438A-A0B2-2B479E8F2E90}' -Protocol TCP -LocalPorts $port"
  fi
fi

if [ -z "$left" ]; then
  echo "tunnel-setup: $ME is set up; tunnel-auto keeps it that way"
else
  echo "tunnel-setup: $ME needs you for:$left"
  exit 1
fi
