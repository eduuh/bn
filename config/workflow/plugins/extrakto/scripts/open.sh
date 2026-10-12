#!/bin/sh

script_dir=$(dirname "$0")
script_dir=$(
	cd "$script_dir"
	pwd
)
. "$script_dir/helpers.sh"
extrakto="$script_dir/../extrakto_plugin.py"

# Popups start from tmux's server environment, not this script's environment.
export PATH="${PATH:-/usr/bin:/bin}:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"
python=$(command -v python3) || {
	tmux display-message -l "extrakto: python3 is required"
	echo "extrakto: python3 is required" >&2
	exit 1
}
fzf_tool=$(get_option "@extrakto_fzf_tool" "fzf")
command -v "$fzf_tool" >/dev/null 2>&1 || {
	tmux display-message -l "extrakto: fzf is required (check @extrakto_fzf_tool)"
	echo "extrakto: fzf is required (check @extrakto_fzf_tool)" >&2
	exit 1
}

pane_id=$1
launch_command=$("$python" -c 'import shlex, sys; print(shlex.join(sys.argv[1:]))' \
	env "PATH=$PATH" "$python" "$extrakto" "$pane_id") || exit 1
split_direction=$(get_option "@extrakto_split_direction" "a")

if [ "$split_direction" = "a" ]; then
	if [ -n "$(tmux list-commands popup 2>/dev/null)" ]; then
		split_direction="p"
	else
		split_direction="v"
	fi
fi

extra_options=""
if [ -n "$2" ]; then
	# requires tmux 3.3 * Add -e flag to set an environment variable for a popup.
	extra_options="-e extrakto_inital_mode=$2"
fi

if [ "$split_direction" = "p" ]; then
	popup_size=$(get_option "@extrakto_popup_size" "90%")
	popup_width=$(echo $popup_size | cut -d',' -f1)
	popup_height=$(echo $popup_size | cut -d',' -f2)

	popup_position=$(get_option "@extrakto_popup_position" "C")
	popup_x=$(echo $popup_position | cut -d',' -f1)
	popup_y=$(echo $popup_position | cut -d',' -f2)

	rc=129
	while [ $rc -eq 129 ]; do
		tmux popup \
			-w "${popup_width}" \
			-h "${popup_height:-${popup_width}}" \
			-x "${popup_x}" \
			-y "${popup_y:-$popup_x}" \
			$extra_options \
			-E "$launch_command popup"
		rc=$?
	done
	exit $rc
else
	split_size=$(get_option "@extrakto_split_size" 7)
	tmux split-window \
		-${split_direction} \
		$extra_options \
		-l ${split_size} "tmux setw remain-on-exit off; $launch_command split"
fi
