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
  # Nothing against main (e.g. already merged): review the latest commit instead.
  rev="HEAD~1"
else
  rev="$base_ref"
fi

# Detach so an agent calling this script is not blocked until the popup closes.
tmux run-shell -b -t "$pane" "tmux display-popup -E -t '$pane' -w 97% -h 97% -d '$path' \"nvim -c 'DiffviewOpen $rev'\""
