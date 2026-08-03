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
  # `sequential = true` matters: plenary defaults to spawning one nvim per spec file and running
  # them ALL concurrently. Several specs here assert on async work — a stubbed `bn` subprocess whose
  # callback has to land inside a `vim.wait` budget — so seven competing nvim processes turn a
  # generous budget into a coin flip. That is why these specs pass alone and fail in a full run,
  # with a different file failing each time. Running one at a time trades a little wall-clock for a
  # suite whose result means something.
  cmd="PlenaryBustedDirectory nvim/tests/ { minimal_init = '$init', sequential = true }"
fi

nvim --headless -u "$init" -c "$cmd"
