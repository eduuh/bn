#!/usr/bin/env bash

CURRENT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
FINGERS_BINARY=""

if command -v "tmux-fingers" &>/dev/null; then
  FINGERS_BINARY="tmux-fingers"
elif [[ -x "$HOME/.local/bin/tmux-fingers" ]]; then
  FINGERS_BINARY="$HOME/.local/bin/tmux-fingers"
elif [[ -f "$CURRENT_DIR/bin/tmux-fingers" ]]; then
  FINGERS_BINARY="$CURRENT_DIR/bin/tmux-fingers"
fi

if [[ -z "$FINGERS_BINARY" ]]; then
  message="tmux-fingers is missing: run bash ~/.config/bn/repo/scripts/install-tmux-fingers.sh"
  tmux display-message "$message"
  echo "$message" >&2
  exit 1
fi

CURRENT_FINGERS_VERSION="$("$FINGERS_BINARY" version)"

pushd "$CURRENT_DIR" &> /dev/null
CURRENT_GIT_VERSION=$(cat shard.yml | grep "^version" | cut -f2 -d':' | sed "s/ //g")
popd &> /dev/null

SKIP_WIZARD=$(tmux show-option -gqv @fingers-skip-wizard)
SKIP_WIZARD=${SKIP_WIZARD:-0}

if [ "$SKIP_WIZARD" = "0" ] && [ "$CURRENT_FINGERS_VERSION" != "$CURRENT_GIT_VERSION" ]; then
  echo "tmux-fingers version mismatch: expected $CURRENT_GIT_VERSION, got $CURRENT_FINGERS_VERSION" >&2
  exit 1
fi

if [[ "$TERM" == "dumb" ]]; then
  # force term value to get proper colors in systemd and tmux 3.6a
  # https://github.com/Morantron/tmux-fingers/issues/143
  FINGERS_TERM=$(tmux show-option -gqv default-terminal)
else
  FINGERS_TERM="$TERM"
fi

printf -v quoted_binary '%q' "$FINGERS_BINARY"
printf -v quoted_term '%q' "$FINGERS_TERM"
tmux run "TERM=$quoted_term $quoted_binary load-config"
exit $?
