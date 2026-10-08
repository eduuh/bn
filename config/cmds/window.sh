#!/bin/sh
# description: Open a tmux window in this repo's session (cwd here), optionally running a command in it; prints the window target
# usage: window [-n name] [-d dir] [-f] [-- command...]
#
# Without -f the window opens in the background, so an agent can start a dev server or build
# beside the user without stealing focus. The command is typed into the window's shell rather than
# exec'd, so the window outlives it and its output stays readable:
#   tmux capture-pane -p -t <target>
# The session is the repo's own (named after the repo, bn's convention), else the current one;
# the repo's session is created when neither exists.
set -eu

name=""
dir=$PWD
focus=0
while [ $# -gt 0 ]; do
  case $1 in
    -n) name=$2; shift 2 ;;
    -d) dir=$2; shift 2 ;;
    -f) focus=1; shift ;;
    --) shift; break ;;
    -h|--help) sed -n 's/^# usage: /usage: /p' "$0"; exit 0 ;;
    *) break ;;
  esac
done

command -v tmux >/dev/null 2>&1 || { echo "window: tmux is not installed" >&2; exit 2; }
[ -d "$dir" ] || { echo "window: no such directory: $dir" >&2; exit 2; }
[ -n "$name" ] || name=${1:-$(basename "$dir")}

repo=${BN_CMD_REPO:-}
session=""
if [ -n "$repo" ] && tmux has-session -t "=$repo" 2>/dev/null; then
  session=$repo
elif [ -n "${TMUX:-}" ]; then
  session=$(tmux display-message -p '#S')
fi

if [ -n "$session" ]; then
  target=$(tmux new-window -d -P -F '#{session_name}:#{window_index}' -t "=$session:" -n "$name" -c "$dir")
elif [ -n "$repo" ]; then
  target=$(tmux new-session -d -P -F '#{session_name}:#{window_index}' -s "$repo" -n "$name" -c "$dir")
else
  echo "window: not in tmux and not in a repo, so no session to open it in" >&2
  exit 2
fi

# Link the window to its worktree the way bn's own windows are, so badges and reconcile see it.
if [ -n "$repo" ]; then
  tmux set-option -w -t "$target" @bn_repo "$repo"
  [ -z "${BN_CMD_BRANCH:-}" ] || tmux set-option -w -t "$target" @bn_branch "$BN_CMD_BRANCH"
fi

if [ $# -gt 0 ]; then
  tmux send-keys -t "$target" -l -- "$*"
  tmux send-keys -t "$target" Enter
fi

if [ "$focus" = 1 ]; then
  tmux select-window -t "$target"
  [ -z "${TMUX:-}" ] || tmux switch-client -t "${target%%:*}"
fi

echo "$target"
