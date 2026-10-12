get_tmux_option() {
	local option=$1
	local default_value=$2
	local option_value=$(tmux show-option -gqv "$option")
	if [ -z "$option_value" ]; then
		echo "$default_value"
	else
		echo "$option_value"
	fi
}

# Ensures a message is displayed for 5 seconds in tmux prompt
display_message() {
	local message=$1
	tmux display-message -t "$pane_target" -d "${2:-5000}" -- "$message"
}

# simplest solution, taken from here: http://unix.stackexchange.com/a/81689
remove_empty_lines_from_end_of_file() {
	local file=$1
	local temp
	temp=$(cat -- "$file")
	printf '%s\n' "$temp" > "$file"
}

supported_tmux_version_ok() {
	"$CURRENT_DIR/check_tmux_version.sh" "$SUPPORTED_VERSION"
}

# Checking full path to logfile and expanding tmux format in normal path
# As example: expand %Y-%m-%d to current date
expand_tmux_format_path() {
	local tmux_format_path=$1
	local full_path
	full_path=$(tmux display-message -p -t "$pane_target" "${tmux_format_path}") || return
	local full_directory_path=${full_path%/*}
	mkdir -p -- "${full_directory_path}" || return
	chmod 700 -- "${full_directory_path}" || return
	mktemp "${full_path}.XXXXXX"
}
