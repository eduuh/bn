#!/usr/bin/env bash

# Shell productivity companions for the workflow layer — not required by bn itself, but they pay
# for themselves on top of it: zoxide for jumping between pool-slot worktrees by fragment, bat for
# reading `bn cat`/`bn plan` output with highlighting, eza for a quick tree of active worktrees, and
# delta as git's diff pager for `bn why` / review-gate diffs. Sourced by install.sh (opt-in via
# --shell-clis / BN_INSTALL_SHELL_CLIS=1) and by its regression check.
#
# Opt-in and non-fatal by design, same posture as install-tmux.sh's build step: a missing tool here
# never blocks the workflow layer, it just means those niceties stay off until installed by hand.
BN_SHELL_CLIS="zoxide bat eza delta"

# The apt/dnf/yum/pacman/brew package name for $1, when it differs from the command name.
# `delta` the command ships as `git-delta` on every package manager but brew (name collision with
# an unrelated `delta` package already in Debian/Ubuntu/Fedora/Arch).
bn_install_shell_cli_package() {
  local manager="$1" tool="$2"
  if [ "$tool" = "delta" ] && [ "$manager" != "brew" ]; then
    echo "git-delta"
  else
    echo "$tool"
  fi
}

# Install whichever of $BN_SHELL_CLIS are missing, via the detected package manager. Reuses
# dep_manager/run_as_root from install.sh (must already be sourced/defined by the caller, as
# install-tmux.sh's functions already assume). Each tool is independent: one missing/unavailable
# package is a warning, not an abort, so the rest still land.
bn_install_ensure_shell_clis() {
  local manager tool pkg missing=()
  for tool in $BN_SHELL_CLIS; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
  done
  if [ "${#missing[@]}" -eq 0 ]; then
    echo "Shell CLIs already present: $BN_SHELL_CLIS"
    return 0
  fi

  manager=$(dep_manager 2>/dev/null || true)
  if [ -z "$manager" ]; then
    echo "warning: missing shell CLIs:${missing[*]:+ ${missing[*]}} (no supported package manager found)." >&2
    return 0
  fi

  echo "Installing missing shell CLIs via $manager:${missing[*]:+ ${missing[*]}}"
  for tool in "${missing[@]}"; do
    pkg=$(bn_install_shell_cli_package "$manager" "$tool")
    case "$manager" in
      apt)    run_as_root apt-get install -y "$pkg" ;;
      dnf)    run_as_root dnf install -y "$pkg" ;;
      yum)    run_as_root yum install -y "$pkg" ;;
      pacman) run_as_root pacman -S --needed --noconfirm "$pkg" ;;
      brew)   brew install "$pkg" ;;
    esac || echo "warning: could not install '$tool' (package '$pkg' via $manager)." >&2
  done
  hash -r 2>/dev/null || true
}
