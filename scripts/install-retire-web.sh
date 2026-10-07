#!/usr/bin/env bash
# Upgrade cleanup only. Never probe ports or kill an unrelated listener.
bn_install_retire_web() {
  local bindir="$1" cfgdir="$2"
  local unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/bn-web.service"
  local plist="$HOME/Library/LaunchAgents/com.bn.web.plist" domain

  if [ -e "$unit" ] || [ -L "$unit" ]; then
    if ! systemctl --user disable --now bn-web.service; then
      echo "error: could not retire bn-web.service; stop it before retrying the install." >&2
      return 1
    fi
    rm -f "$unit" || return 1
    systemctl --user daemon-reload || return 1
    echo "Removed retired bn-web.service."
  fi

  if [ -e "$plist" ] || [ -L "$plist" ]; then
    if ! command -v launchctl >/dev/null 2>&1; then
      echo "error: could not retire com.bn.web; launchctl is unavailable." >&2
      return 1
    fi
    domain="gui/$(id -u)"
    if launchctl print "$domain/com.bn.web" >/dev/null 2>&1; then
      if ! launchctl bootout "$domain/com.bn.web"; then
        echo "error: could not retire com.bn.web; stop it before retrying the install." >&2
        return 1
      fi
    fi
    rm -f "$plist" || return 1
    echo "Removed retired com.bn.web agent."
  fi

  rm -f "$bindir/bn-web" "$cfgdir/web-token"
}
