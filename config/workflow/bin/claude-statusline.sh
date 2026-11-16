#!/usr/bin/env bash
# claude-statusline.sh — Claude Code statusLine: model and Git worktree identity.
set -u
model=""
if command -v jq >/dev/null 2>&1; then
  model=$(jq -r '.model.display_name // empty' 2>/dev/null)
else
  cat >/dev/null
fi
repo=$(git rev-parse --show-toplevel 2>/dev/null || true)
branch=$(git branch --show-current 2>/dev/null || true)
printf '%s%s%s' "${model:+[$model] }" "$(basename "${repo:-$PWD}")" "${branch:+:$branch}"
