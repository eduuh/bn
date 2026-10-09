#!/usr/bin/env bash
# claude-statusline.sh — Claude Code statusLine: model, repo:branch, bn open todos/blockers.
# Claude passes session JSON on stdin; the bn line is cheap and failure-tolerant.
set -u
model=""
if command -v jq >/dev/null 2>&1; then
  model=$(jq -r '.model.display_name // empty' 2>/dev/null)
else
  cat >/dev/null
fi
note=$(timeout 2 bn status 2>/dev/null | head -1)
printf '%s%s' "${model:+[$model] }" "${note:-$(basename "$PWD")}"
