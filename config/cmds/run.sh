#!/bin/sh
# description: Run a foreground command with live output, private logs, heartbeats and inactivity warnings
# usage: run [--label name] [--interval seconds] [--idle-after seconds] [--log path] -- command...
set -eu
exec python3 "$(dirname "$0")/run.py" "$@"
