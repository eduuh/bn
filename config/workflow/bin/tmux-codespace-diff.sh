#!/bin/sh
set -eu

if [ "${1:-}" != "--popup" ]; then
  pane=${1:-${TMUX_PANE:-}}
  if [ -z "$pane" ]; then
    echo "tmux-codespace-diff: no tmux pane supplied" >&2
    exit 1
  fi
  path=$(tmux display-message -p -t "$pane" '#{pane_current_path}')
  script=$(readlink -f "$0")
  script_quoted=$(printf "'%s'" "$(printf '%s' "$script" | sed "s/'/'\\\\''/g")")
  exec tmux display-popup -EE -t "$pane" -w 97% -h 97% -d "$path" \
    "exec $script_quoted --popup"
fi

printf 'Codespace SSH alias: '
IFS= read -r alias || exit 1
case "$alias" in
  ''|-*|*[!A-Za-z0-9_.-]*)
    echo "tmux-codespace-diff: invalid SSH alias" >&2
    exit 2
    ;;
esac

repo=$(git rev-parse --show-toplevel)
default_path="/workspaces/$(basename "$repo")"
printf 'Remote repo path [%s]: ' "$default_path"
IFS= read -r remote_path || exit 1
remote_path=${remote_path:-$default_path}
remote_path_quoted=$(printf "'%s'" "$(printf '%s' "$remote_path" | sed "s/'/'\\\\''/g")")

exec ssh -tt "$alias" \
  "cd -- $remote_path_quoted && base=\$(git merge-base origin/main HEAD) && nvim -c \"DiffviewOpen \$base\""
