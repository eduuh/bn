#!/bin/sh
# description: Inspect this worktree's command runs, retained logs and exact-command reruns
# usage: runs [--json | --status | --logs id | --rerun id | --pick]
set -eu
exec python3 "$(dirname "$0")/runs.py" "$@"
