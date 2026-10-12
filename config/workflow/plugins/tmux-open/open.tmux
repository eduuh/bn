#!/usr/bin/env bash

CURRENT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$CURRENT_DIR/scripts/helpers.sh"

# Resolve Python before copy-pipe starts, even with a minimal tmux server PATH.
export PATH="${PATH:-/usr/bin:/bin}:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"
python=$(command -v python3) || {
	tmux display-message -l "tmux-open: python3 is required"
	echo "tmux-open: python3 is required" >&2
	exit 1
}
selection_helper="$CURRENT_DIR/../../bin/tmux-open-selection.py"

shell_quote() {
	local value=${1//\'/\'\\\'\'}
	printf "'%s'" "$value"
}

selection_command() {
	local action="$1"
	local engine="${2:-}"
	printf '%s %s %s %s' "$(shell_quote "$python")" \
		"$(shell_quote "$selection_helper")" "'#{pane_id}'" "$(shell_quote "$action")"
	if [ "$action" = "search" ]; then
		printf ' %s' "$(shell_quote "$engine")"
	fi
}

set_copy_mode_bindings() {
	local key="$1"
	local command="$2"
	tmux bind-key -T copy-mode-vi "$key" send-keys -X copy-pipe-and-cancel "$command"
	tmux bind-key -T copy-mode "$key" send-keys -X copy-pipe-and-cancel "$command"
}

main() {
	local key
	local engine
	local keys
	keys=$(get_tmux_option "@open" "M-o")
	for key in $keys; do
		set_copy_mode_bindings "$key" "$(selection_command open)"
	done
	keys=$(get_tmux_option "@open-editor" "M-e")
	for key in $keys; do
		set_copy_mode_bindings "$key" "$(selection_command editor)"
	done
	for key in $(stored_engine_vars); do
		engine=$(get_engine "$key")
		set_copy_mode_bindings "$key" "$(selection_command search "$engine")"
	done
}

main
