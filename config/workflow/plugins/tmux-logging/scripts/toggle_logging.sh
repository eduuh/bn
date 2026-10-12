#!/usr/bin/env bash
set -euo pipefail

CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$CURRENT_DIR/variables.sh"
source "$CURRENT_DIR/shared.sh"

supported_tmux_version_ok
pipe_active=$(tmux display-message -p -t "$pane_target" '#{pane_pipe}')
logging_file=$(tmux show-option -pqv -t "$pane_target" @bn_logging_file)

if [ "$pipe_active" = "1" ]; then
	if [ -z "$logging_file" ]; then
		message="This pane already has an output pipe; not replacing it."
		display_message "$message"
		echo "$message" >&2
		exit 1
	fi
	tmux pipe-pane -t "$pane_target"
	tmux set-option -pu -t "$pane_target" @bn_logging_file
	display_message "Ended logging to $logging_file"
else
	file=$(expand_tmux_format_path "$logging_full_filename")
	bash "$CURRENT_DIR/start_logging.sh" "$file" "$pane_target"
	tmux set-option -p -t "$pane_target" @bn_logging_file "$file"
	display_message "Started logging to $file"
fi
