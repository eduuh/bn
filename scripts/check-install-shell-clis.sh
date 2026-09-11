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

echo "check-install-shell-clis: ok"
