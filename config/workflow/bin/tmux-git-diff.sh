#!/bin/sh
set -eu

pane=${1:-${TMUX_PANE:-}}
if [ -z "$pane" ]; then
  echo "tmux-git-diff: no tmux pane supplied" >&2
  exit 1
fi

path=$(tmux display-message -p -t "$pane" '#{pane_current_path}')
if git -C "$path" rev-parse --verify --quiet origin/main >/dev/null; then
  base_ref=origin/main
elif git -C "$path" rev-parse --verify --quiet main >/dev/null; then
  base_ref=main
else
  echo "tmux-git-diff: neither origin/main nor main exists" >&2
  exit 1
fi

base=$(git -C "$path" merge-base "$base_ref" HEAD)
if git -C "$path" diff --quiet "$base"; then
  tmux display-message -t "$pane" "No tracked changes against $base_ref"
  exit 0
else
  status=$?
  if [ "$status" -ne 1 ]; then
    exit "$status"
  fi
fi

exec tmux display-popup -E -t "$pane" -w 97% -h 97% -d "$path" \
  "git diff --no-ext-diff --color=never $base | nvim -R -c 'set filetype=diff' -"
