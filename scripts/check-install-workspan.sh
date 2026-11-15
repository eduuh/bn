#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
source "$repo/scripts/install-workspan.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/config"
printf '#!/bin/sh\nprintf "compatibility\\n"\n' > "$tmp/bin/bn"
chmod +x "$tmp/bin/bn"
bn_install_workspan "$tmp/bin" "$tmp/config"
[ "$("$tmp/bin/workspan")" = compatibility ]
[ "$("$tmp/config/workspan")" = compatibility ]
[ ! -e "$tmp/bin/wp" ]
bn_install_workspan "$tmp/bin" "$tmp/config"

WORKSPAN_INSTALL_WP=1 bn_install_workspan "$tmp/bin" "$tmp/config"
if [ -L "$tmp/bin/wp" ]; then
  [ "$(readlink "$tmp/bin/wp")" = workspan ]
fi
# Remove only the alias created by this isolated test before planting a real command.
if [ -L "$tmp/bin/wp" ]; then
  rm "$tmp/bin/wp"
fi
printf '#!/bin/sh\necho wordpress\n' > "$tmp/bin/wp"
chmod +x "$tmp/bin/wp"
WORKSPAN_INSTALL_WP=1 bn_install_workspan "$tmp/bin" "$tmp/config"
[ "$("$tmp/bin/wp")" = wordpress ]

rm "$tmp/bin/workspan"
printf 'unrelated tool\n' > "$tmp/bin/workspan"
if bn_install_workspan "$tmp/bin" "$tmp/config"; then
  echo "expected conflicting workspan command to be preserved" >&2
  exit 1
fi
[ "$(cat "$tmp/bin/workspan")" = 'unrelated tool' ]

rm "$tmp/bin/workspan" "$tmp/config/workspan"
printf 'unrelated anchor\n' > "$tmp/config/workspan"
if bn_install_workspan "$tmp/bin" "$tmp/config"; then
  echo "expected conflicting Workspan anchor to be preserved" >&2
  exit 1
fi
[ "$(cat "$tmp/config/workspan")" = 'unrelated anchor' ]
echo "check-install-workspan: ok"
