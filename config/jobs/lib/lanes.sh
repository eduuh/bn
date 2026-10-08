# Sourced helper for the cross-repo worker lanes (red-main.sh, pr-babysit.sh): settings, guards,
# a deterministic checklist written into the agent's note, and the one report message back.
# Source it from a job:  . "$(dirname "$0")/lib/lanes.sh"
#
# POSIX sh: the scheduler runs jobs with sh (dash on Ubuntu).
#
# The checklist is the point. The lane works out the facts (which run failed, which PR conflicts)
# with plain gh and jq, and writes one todo per fact into the dispatched agent's branch note before
# it starts. The agent follows the list and `bn done`s each line, so every run takes the same
# steps and a supervisor can check the note instead of reading a pane.

BN="${BN:-bn}"
export PATH="$HOME/.bin:$HOME/.bn/bin:$HOME/.local/bin:$PATH"
LANE_ROOT="${BN_WORKTREE_DIR:-$HOME/projects/worktree}"

lane_log() { printf '  %s\n' "$*" >&2; }

# lane_init <name> -- settings from ~/.config/bn/<name>.env, the kill switch ~/.bn/<name>.off, and
# the tools. Sets LANE and OWNER. Exits 0 (a skipped run, not a failure) when the lane cannot run.
lane_init() {
  LANE=$1
  [ -f "$HOME/.config/bn/$LANE.env" ] && . "$HOME/.config/bn/$LANE.env"
  [ -f "$HOME/.bn/$LANE.off" ] && { lane_log "$LANE: disabled by ~/.bn/$LANE.off"; exit 0; }
  command -v gh >/dev/null 2>&1 && command -v jq >/dev/null 2>&1 || { lane_log "$LANE: needs gh and jq"; exit 0; }
  OWNER="${BN_LANE_OWNER:-$(gh api user --jq .login 2>/dev/null)}"
  [ -n "$OWNER" ] || { lane_log "$LANE: gh is not signed in"; exit 0; }
  LANE_LIVE=$("$BN" agent live --json 2>/dev/null) || { lane_log "$LANE: cannot read live agents; skipping"; exit 0; }
}

# lane_live <branch-prefix> [repo] -- live agents of this lane (on this machine), all repos or one.
# The prefix is bare (`redmain`): the fleet reports `redmain/x` as `redmain-x`, since a tmux window
# name cannot hold a slash, so both separators match.
lane_live() {
  printf '%s' "$LANE_LIVE" | jq -r --arg p "$1" --arg r "${2:-}" '[.data.agents[]?
    | select(.branch | test("^" + $p + "[-/]")) | select($r == "" or .repo == $r)
    | select(.has_window == true) | select(.state != "done" and .state != "gone")] | length'
}

# lane_clone <repo> -- make sure this machine has a checkout of OWNER/<repo>.
lane_clone() {
  [ -d "$LANE_ROOT/$1/main" ] || [ -d "$LANE_ROOT/$1/master" ] && return 0
  lane_log "$LANE: cloning $OWNER/$1"
  (cd "$HOME" && "$BN" wt clone "https://github.com/$OWNER/$1.git" </dev/null >/dev/null 2>&1)
}

# lane_dispatch <repo> <branch> <skill> <task> <checklist> -- one agent on <repo>, seeded with
# <skill>, and <checklist> (one line per step) written into its note as todos.
# BN_<LANE>_AGENT picks the harness; BN_LANE_DRYRUN=1 prints instead.
lane_dispatch() {
  _repo=$1; _branch=$2; _skill=$3; _task=$4; _steps=$5
  _agent=$(eval "printf '%s' \"\${BN_$(printf '%s' "$LANE" | tr 'a-z-' 'A-Z_')_AGENT:-}\"")
  set -- --repo "$_repo" --skill "$_skill" --branch "$_branch" --name "$LANE $_repo" --fresh --yolo --agent
  [ -n "$_agent" ] && set -- "$@" "$_agent"
  _task="$_task Your checklist is already in your branch note as todos, computed from GitHub: work through them and bn done each one."
  [ -n "${BN_LANE_REPORT_TO:-}" ] && _task="$_task When the pass ends, report once with bn agent msg $BN_LANE_REPORT_TO."
  set -- "$@" -- "$_task"
  if [ "${BN_LANE_DRYRUN:-0}" = 1 ]; then
    lane_log "DRYRUN: $BN dispatch $*"
    printf '%s\n' "$_steps" | sed 's/^/    todo: /' >&2
    return 0
  fi
  "$BN" dispatch "$@" >/dev/null 2>&1 || { lane_log "$LANE: dispatch failed for $_repo"; return 1; }
  printf '%s\n' "$_steps" | while IFS= read -r _line; do
    [ -n "$_line" ] || continue
    "$BN" add todo --repo "$_repo" --branch "$_branch" "$_line" >/dev/null 2>&1 \
      || lane_log "$LANE: could not add todo to $_repo/$_branch: $_line"
  done
  lane_log "$LANE: dispatched an agent on $_repo ($_branch)"
}

# lane_report <text> -- one message to BN_LANE_REPORT_TO (a repo:branch on any mesh machine), if
# set. Undelivered, bn parks it in the outbox and retries, so a sleeping laptop still gets it.
lane_report() {
  [ -n "${BN_LANE_REPORT_TO:-}" ] || return 0
  [ "${BN_LANE_DRYRUN:-0}" = 1 ] && { lane_log "DRYRUN report: $1"; return 0; }
  "$BN" agent msg "$BN_LANE_REPORT_TO" --from "bn:$LANE" "$1" >/dev/null 2>&1 || true
}

# lane_stamp -- a branch suffix: date and time to the minute.
lane_stamp() { date +%Y%m%d-%H%M; }
