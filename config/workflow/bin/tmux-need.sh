#!/bin/sh
# tmux-need.sh — run a tool inside a popup, or say it is missing and wait for Enter.
#
# Usage: tmux-need.sh <tool> [args...]
#
# A `display-popup -E` whose command is not installed closes the instant it opens, so the key
# looks dead. Wrapping the command keeps the popup up long enough to say why.
tool=${1:?usage: tmux-need.sh <tool> [args...]}
if command -v "$tool" >/dev/null 2>&1; then
  exec "$@"
fi
printf '%s is not installed (install.sh lists it as optional). Press Enter to close.\n' "$tool"
read -r _
exit 127
