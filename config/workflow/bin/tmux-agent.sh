#!/usr/bin/env zsh
# tmux-agent.sh — spawn an AI coding agent in a new window on the current worktree.
#
# Usage: tmux-agent.sh [pane_path] [agent]   agent = copilot (default) | claude
#
# Thin tmux front-end for the resume-or-fresh launcher — now native `bn agent launch` (ported from
# the old workflow/bin/agent), which owns the rule: resume the worktree's recorded session if one
# exists, else start fresh (claude also opens in yolo mode). Opens a new window rooted at the
# worktree, named ⛏ + branch, and primes @bn_agent
# so the window self-decorates with live state (working ◌ / waiting ● / done ✓) from the hooks.
source "${0:A:h}/../lib/tmux-lib.sh"
tmux_init

pane_path="${1:-$("$TMUX_CMD" display-message -p '#{pane_current_path}')}"
[[ -d "$pane_path" ]] || pane_path="$HOME"
agent="${2:-copilot}"

branch=$(git -C "$pane_path" branch --show-current 2>/dev/null || echo "")
name="⛏ ${branch:-agent}"

# Call the resume-or-fresh launcher — now native `bn agent launch` (ported from the old
# workflow/bin/agent zsh script). The tmux server's PATH lacks the bindir, so reach bn by its
# stable installed path (the same one the other tmux binds use).
agent_cmd="$HOME/.config/bn/bn agent launch"

# Shell-backed pane: drop to a shell when the harness exits instead of running it as the pane
# root — with tmux's default `remain-on-exit off` a crashing harness would close the single-pane
# window instantly and hide its error output at the exact moment you need it. `${(q)...}` quotes
# the agent path + arg so a workflow path containing a space doesn't break the tmux command string.
win=$("$TMUX_CMD" new-window -P -F '#{window_id}' -c "$pane_path" -n "$name" "${agent_cmd} ${(q)agent}"'; exec "${SHELL:-zsh}"')
"$TMUX_CMD" set-window-option -t "$win" @bn_agent ""
"$TMUX_CMD" display-message "$agent spawned in ${branch:-$pane_path} —  working ·  waiting ·  done"
