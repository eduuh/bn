#!/usr/bin/env bash
set -euo pipefail

fail() { printf 'Entire setup: %s\n' "$*" >&2; exit 1; }

case "${1:-}" in
  claude|claude-code) agent=claude-code ;;
  copilot|copilot-cli) agent=copilot-cli ;;
  *) fail "usage: bash setup-entire.sh <claude|copilot> [project-directory]" ;;
esac

project="${2:-.}"
root=$(git -C "$project" rev-parse --show-toplevel) ||
  fail "run setup from an existing Git worktree"

config="${XDG_CONFIG_HOME:-$HOME/.config}/bn/entire-checkpoint-remote"
remote="${BN_ENTIRE_CHECKPOINT_REMOTE:-}"
if [ -z "$remote" ] && [ -f "$config" ]; then
  remote=$(cat "$config")
fi
[[ "$remote" =~ ^github:[A-Za-z0-9-]+/[A-Za-z0-9_.-]+$ ]] ||
  fail "set an approved github:owner/repo in $config or BN_ENTIRE_CHECKPOINT_REMOTE"

command -v entire >/dev/null 2>&1 ||
  fail "Entire is not installed; follow https://docs.entire.com/installation, then rerun setup"
command -v gh >/dev/null 2>&1 ||
  fail "gh is required to verify the checkpoint repository is private"

private=$(GH_PROMPT_DISABLED=1 gh api --hostname github.com \
  "repos/${remote#github:}" --jq '.private') ||
  fail "could not verify the checkpoint repository; no capture was enabled"
[ "$private" = true ] ||
  fail "checkpoint repository must be private; no capture was enabled"

cd "$root"
entire enable --yes --no-init-repo --local --telemetry=false \
  --agent "$agent" --checkpoint-remote "$remote"
entire status
