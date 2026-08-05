#!/usr/bin/env bash
# Run the lazybn cockpit specs headless. Bootstraps plenary on first run.
#
#   nvim/tests/run.sh            # all specs
#   nvim/tests/run.sh render     # just nvim/tests/render_spec.lua
#
# Exits non-zero if any spec fails (plenary writes the failure count and we
# grep the output, since PlenaryBustedDirectory itself exits 1 on failure).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
nvim_dir="$(dirname "$here")"
repo_root="$(dirname "$nvim_dir")"
cd "$repo_root"

init="nvim/tests/minimal_init.lua"

if [[ $# -gt 0 ]]; then
  # Single file: nvim/tests/<name>_spec.lua (PlenaryBustedFile takes only a path;
  # the file is sourced in this same nvim, already bootstrapped by -u above).
  cmd="PlenaryBustedFile nvim/tests/${1%_spec}_spec.lua"
else
  cmd="PlenaryBustedDirectory nvim/tests/ { minimal_init = '$init' }"
fi

nvim --headless -u "$init" -c "$cmd"
