#!/usr/bin/env bash

CURRENT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

source "$CURRENT_DIR/scripts/variables.sh"
source "$CURRENT_DIR/scripts/shared.sh"


main() {
	tmux bind-key -N "Logging · toggle recording this pane (off by default)" "$logging_key" run-shell -b "bash '$CURRENT_DIR/scripts/toggle_logging.sh' '#{pane_id}'"
	tmux bind-key -N "Logging · save the visible pane text" "$pane_screen_capture_key" run-shell -b "bash '$CURRENT_DIR/scripts/screen_capture.sh' '#{pane_id}'"
	tmux bind-key -N "Logging · save this pane's full scrollback" "$save_complete_history_key" run-shell -b "bash '$CURRENT_DIR/scripts/save_complete_history.sh' '#{pane_id}'"
}

main
