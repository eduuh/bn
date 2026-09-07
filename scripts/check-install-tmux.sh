#!/usr/bin/env bash
# The tmux floor logic install.sh relies on, without a network or a compiler: version parsing and
# the floor comparison must treat letter suffixes as patch levels and a missing tmux as too old.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/install-tmux.sh
source "$repo/scripts/install-tmux.sh"

fail() { echo "check-install-tmux: $*" >&2; exit 1; }

[ "$(bn_install_tmux_version_of 'tmux 3.7c')" = "3.7c" ] || fail "3.7c did not parse"
[ "$(bn_install_tmux_version_of 'tmux 3.6a')" = "3.6a" ] || fail "3.6a did not parse"
[ "$(bn_install_tmux_version_of 'tmux next-3.8')" = "3.8" ] || fail "next-3.8 did not parse"

# Meets the floor.
bn_install_tmux_satisfies 3.7 3.7   || fail "3.7 must satisfy 3.7"
bn_install_tmux_satisfies 3.7c 3.7  || fail "3.7c must satisfy 3.7"
bn_install_tmux_satisfies 3.8 3.7   || fail "3.8 must satisfy 3.7"
bn_install_tmux_satisfies 3.10 3.7  || fail "3.10 must satisfy 3.7 (numeric, not lexical)"

# Below the floor — 3.6a is the palette-leak version this floor exists to retire.
! bn_install_tmux_satisfies 3.6a 3.7 || fail "3.6a must not satisfy 3.7"
! bn_install_tmux_satisfies 3.6b 3.7 || fail "3.6b must not satisfy 3.7"
! bn_install_tmux_satisfies 3.5a 3.7 || fail "3.5a must not satisfy 3.7"
! bn_install_tmux_satisfies ""   3.7 || fail "no tmux must not satisfy 3.7"

# The pin itself meets the floor, so a bump of one without the other cannot go unnoticed.
bn_install_tmux_satisfies "$BN_TMUX_VERSION" "$BN_TMUX_MIN_VERSION" ||
  fail "pinned BN_TMUX_VERSION=$BN_TMUX_VERSION is below BN_TMUX_MIN_VERSION=$BN_TMUX_MIN_VERSION"

echo "check-install-tmux: ok"
