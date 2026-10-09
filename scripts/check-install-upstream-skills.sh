#!/usr/bin/env bash
# Exercise only the upstream-skill phase, with no network or changes to the user's home.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export HOME="$tmp/home"
mkdir -p "$HOME"
sed -n '/^install_upstream_skills() {/,/^}/p' "$repo/install.sh" > "$tmp/upstream.sh"
source "$tmp/upstream.sh"

fail() { echo "check-install-upstream-skills: $*" >&2; exit 1; }

npx() {
  local source="$4" skill="$6"
  printf '%s\n' "$*" >> "$tmp/calls"
  [ "$*" = "-y skills add $source --skill $skill -g -a claude-code -a github-copilot -y" ] ||
    return 1
  if [ "$skill" = "resolving-merge-conflicts" ]; then
    [ "$source" = "https://github.com/mattpocock/skills/tree/153fc1b93de6584562765cdce299324e1ff9e661/skills/engineering/resolving-merge-conflicts" ] ||
      return 1
  fi
  [ "$skill" != "${FAILED_SKILL:-}" ] || return 1
  mkdir -p "$HOME/.agents/skills/$skill"
}

BN_SKIP_UPSTREAM_SKILLS=0 install_upstream_skills > "$tmp/output" 2>&1
! grep -q 'did not install' "$tmp/output" ||
  fail "all configured skills must install, including the removed upstream skill"
[ "$(wc -l < "$tmp/calls" | tr -d ' ')" = 6 ] || fail "expected six installs"

# Skip must not invoke npx.
BN_SKIP_UPSTREAM_SKILLS=1 install_upstream_skills > "$tmp/output" 2>&1
[ "$(wc -l < "$tmp/calls" | tr -d ' ')" = 6 ] || fail "skip invoked npx"
grep -q 'Skipped upstream agent skills' "$tmp/output" || fail "skip was not reported"

# Best-effort failures remain visible and do not stop later installs.
: > "$tmp/calls"
FAILED_SKILL=resolving-merge-conflicts BN_SKIP_UPSTREAM_SKILLS=0 \
  install_upstream_skills > "$tmp/output" 2>&1
grep -q 'did not install: resolving-merge-conflicts' "$tmp/output" ||
  fail "failed skill was not reported"
[ "$(wc -l < "$tmp/calls" | tr -d ' ')" = 6 ] || fail "failure stopped later installs"

echo "check-install-upstream-skills: ok"
