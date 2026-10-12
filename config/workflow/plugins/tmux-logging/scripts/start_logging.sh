#!/usr/bin/env bash
set -euo pipefail
umask 077

file="$1"
pane_target="$2"
printf -v quoted_file '%q' "$file"

if command -v ansifilter >/dev/null 2>&1; then
	filter="ansifilter"
elif [ "$(uname)" = "Darwin" ]; then
	filter=$'sed -l -E \'s/\033\\[[0-?]*[ -/]*[@-~]|\r//g\''
else
	filter="sed -u -E 's/\\x1B\\[[0-?]*[ -/]*[@-~]|\\r//g'"
fi
tmux pipe-pane -o -t "$pane_target" "umask 077; exec $filter >> $quoted_file"
