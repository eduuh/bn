#!/usr/bin/env bash
# The stow linker install.sh relies on, against a throwaway $HOME and the real config/stow tree:
# every tracked file links in, a real file survives without force and is backed up with it, a
# second run changes nothing, and the no-stow fallback gives the same result.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/install-stow.sh
source "$repo/scripts/install-stow.sh"

fail() { echo "check-install-stow: $*" >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
stow_dir="$repo/config/stow"

tracked=$(cd "$stow_dir" && find . -mindepth 3 -type f | sed -E 's|^\./[^/]+/||' | sort)
[ -n "$tracked" ] || fail "no files under config/stow"

linked() { # $1 = home: every tracked file is a symlink into the repo
  local f
  while IFS= read -r f; do
    [ -L "$1/$f" ] || fail "$f is not a symlink in $1"
    case "$(bn_install_stow_canon "$1/$f")" in */config/stow/*) ;; *) fail "$f does not resolve into config/stow" ;; esac
  done <<<"$tracked"
  # Folding off: a tool's state dir must stay a real directory.
  [ ! -L "$1/.copilot" ] || fail "~/.copilot became a symlink (folded)"
}

run_case() { # $1 = label, rest = PATH override for the case
  local label=$1 home="$tmp/$1"
  mkdir -p "$home/.copilot"
  echo "state" >"$home/.copilot/sessions.db"
  echo "mine" >"$home/.copilot/settings.json"

  bn_install_stow "$stow_dir" "$home" 0 >/dev/null 2>&1
  [ "$(cat "$home/.copilot/settings.json")" = "mine" ] || fail "$label: real file replaced without force"
  [ -f "$home/.copilot/sessions.db" ] || fail "$label: unrelated state file disturbed"

  bn_install_stow "$stow_dir" "$home" 1 >/dev/null 2>&1
  linked "$home"
  ls "$home/.copilot"/settings.json.bak-* >/dev/null 2>&1 || fail "$label: no backup kept"
  [ "$(cat "$home/.copilot/sessions.db")" = "state" ] || fail "$label: state file lost"

  local before after
  before=$(find "$home" -type l | sort | while IFS= read -r l; do echo "$l $(readlink "$l")"; done)
  bn_install_stow "$stow_dir" "$home" 0 >/dev/null 2>&1
  after=$(find "$home" -type l | sort | while IFS= read -r l; do echo "$l $(readlink "$l")"; done)
  [ "$before" = "$after" ] || fail "$label: second run changed links"
}

# One conflicting file must not stop the rest of its package from linking.
mix="$tmp/mix"
mkdir -p "$tmp/mixsrc/pkg/.d" "$mix/.d"
echo a >"$tmp/mixsrc/pkg/.d/a"; echo b >"$tmp/mixsrc/pkg/.d/b"; echo real >"$mix/.d/a"
bn_install_stow "$tmp/mixsrc" "$mix" 0 >/dev/null 2>&1
[ "$(cat "$mix/.d/a")" = "real" ] && [ ! -L "$mix/.d/a" ] || fail "conflicting file was replaced"
[ -L "$mix/.d/b" ] || fail "free file in a conflicting package was not linked"

run_case with-stow-if-present

# Same again with stow hidden from PATH, to exercise the ln fallback.
nostow=$(mktemp -d "$tmp/bin.XXXX")
for t in bash dirname basename find sed sort mkdir mv ln readlink date python3 cat ls rm mktemp; do
  p=$(command -v "$t" 2>/dev/null || true); [ -n "$p" ] && ln -s "$p" "$nostow/$t"
done
PATH="$nostow" run_case without-stow

echo "check-install-stow: ok"
