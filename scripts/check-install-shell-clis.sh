#!/usr/bin/env bash
# The shell-CLI package-name mapping install.sh relies on, without a network or a package manager:
# `delta` maps to the `git-delta` package everywhere except brew; every other tool passes through.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/install-shell-clis.sh
source "$repo/scripts/install-shell-clis.sh"

fail() { echo "check-install-shell-clis: $*" >&2; exit 1; }

for manager in apt dnf yum pacman; do
  [ "$(bn_install_shell_cli_package "$manager" delta)" = "git-delta" ] ||
    fail "delta must map to git-delta on $manager"
done
[ "$(bn_install_shell_cli_package brew delta)" = "delta" ] || fail "delta must stay 'delta' on brew"

for manager in apt dnf yum pacman brew; do
  for tool in zoxide bat eza; do
    [ "$(bn_install_shell_cli_package "$manager" "$tool")" = "$tool" ] ||
      fail "$tool must pass through unchanged on $manager"
  done
done

# The tool list itself: catches an accidental typo/removal in BN_SHELL_CLIS going unnoticed.
[ "$BN_SHELL_CLIS" = "zoxide bat eza delta" ] || fail "BN_SHELL_CLIS changed unexpectedly: $BN_SHELL_CLIS"

# Check the installer's defaults and opt-outs without running its deployment steps.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed -n '/^mode="full"/,/^# bn home/{ /^# bn home/d; p; }' "$repo/install.sh" > "$tmp/settings.sh"
sed -n '/^# --- shell CLIs/,/^preflight_report "\$mode"/p' "$repo/install.sh" > "$tmp/phase.sh"

check_shell_cli_mode() (
  local expected="$1" setting="$2" deps="$3"
  shift 3
  unset BN_INSTALL_SHELL_CLIS BN_INSTALL_DEPS
  [ "$setting" = default ] || export BN_INSTALL_SHELL_CLIS="$setting"
  [ "$deps" = default ] || export BN_INSTALL_DEPS="$deps"
  local calls=0
  bn_install_ensure_shell_clis() { calls=$((calls + 1)); }
  preflight_report() { :; }
  source "$tmp/settings.sh"
  source "$tmp/phase.sh"
  [ "$calls" = "$expected" ] ||
    fail "expected $expected shell CLI installs for setting=$setting deps=$deps args=$*, got $calls"
)

check_shell_cli_mode 1 default default
check_shell_cli_mode 0 0 default
check_shell_cli_mode 0 default 0
check_shell_cli_mode 0 default default --core
check_shell_cli_mode 1 default default --core --shell-clis
check_shell_cli_mode 1 1 default --core
check_shell_cli_mode 1 0 default --shell-clis
check_shell_cli_mode 0 1 0 --shell-clis

echo "check-install-shell-clis: ok"
