#!/bin/sh
# Check all tracked shell scripts, including hidden jobs, without running installers.
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo"

if git grep -nE \
  'bn-web|bn[[:space:]]+serve|tmux-mcp|BN_SKIP_UI|BN_REBUILD_UI|BN_WEB_' \
  -- '*.sh' '*.yml' '*.toml' \
  ':!scripts/install-retire-web.sh' ':!scripts/check-install-retire-web.sh' \
  ':!scripts/check-cli-only.sh'; then
  echo "error: retired web support remains in scripts or build configuration." >&2
  exit 1
else
  status=$?
  [ "$status" -eq 1 ] || exit "$status"
fi

if [ -n "$(git ls-files -- crates/bn-web)" ]; then
  echo "error: the retired web crate is still tracked." >&2
  exit 1
fi

git ls-files -- '*.sh' | while IFS= read -r file; do
  case "$(head -n 1 "$file")" in
    '#!/bin/sh' | '#!/usr/bin/env sh') sh -n "$file" ;;
    *) bash -n "$file" ;;
  esac
done

echo "CLI-only scripts and build configuration: PASS"
