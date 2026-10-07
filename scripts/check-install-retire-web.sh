#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/install-retire-web.sh
source "$repo/scripts/install-retire-web.sh"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/config" WEB_CLEANUP_LOG="$tmp/calls"
export PATH="$tmp/tools:$PATH"
mkdir -p "$tmp/tools" "$HOME/bin" "$HOME/bn-config" "$XDG_CONFIG_HOME/systemd/user" \
  "$HOME/Library/LaunchAgents"
cat >"$tmp/tools/systemctl" <<'SH'
#!/bin/sh
echo "systemctl $*" >>"$WEB_CLEANUP_LOG"
[ "${WEB_CLEANUP_FAIL:-0}" != 1 ]
SH
cat >"$tmp/tools/launchctl" <<'SH'
#!/bin/sh
echo "launchctl $*" >>"$WEB_CLEANUP_LOG"
exit 0
SH
chmod +x "$tmp/tools/systemctl" "$tmp/tools/launchctl"

# A fresh install does not contact a service manager.
bn_install_retire_web "$HOME/bin" "$HOME/bn-config"
[ ! -e "$WEB_CLEANUP_LOG" ]

unit="$XDG_CONFIG_HOME/systemd/user/bn-web.service"
plist="$HOME/Library/LaunchAgents/com.bn.web.plist"
touch "$unit" "$plist" "$HOME/bin/bn-web" "$HOME/bn-config/web-token" \
  "$XDG_CONFIG_HOME/systemd/user/bn-other.service"
bn_install_retire_web "$HOME/bin" "$HOME/bn-config"
[ ! -e "$unit" ] && [ ! -e "$plist" ]
[ ! -e "$HOME/bin/bn-web" ] && [ ! -e "$HOME/bn-config/web-token" ]
[ -e "$XDG_CONFIG_HOME/systemd/user/bn-other.service" ]
grep -qx 'systemctl --user disable --now bn-web.service' "$WEB_CLEANUP_LOG"
grep -qx 'systemctl --user daemon-reload' "$WEB_CLEANUP_LOG"
grep -qx "launchctl bootout gui/$(id -u)/com.bn.web" "$WEB_CLEANUP_LOG"

# A failed stop is explicit and leaves the unit and binary available for retry.
touch "$unit" "$HOME/bin/bn-web"
export WEB_CLEANUP_FAIL=1
if bn_install_retire_web "$HOME/bin" "$HOME/bn-config"; then
  echo "web retirement unexpectedly succeeded after a service-manager failure" >&2
  exit 1
fi
[ -e "$unit" ] && [ -e "$HOME/bin/bn-web" ]
echo "installer web retirement: PASS"
