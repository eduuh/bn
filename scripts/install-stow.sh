#!/usr/bin/env bash
# Link tracked dotfiles into a target dir (normally $HOME) from stow packages.
#
# config/stow/<package>/<path under $HOME>, one package per tool (copilot, claude, agency, git),
# so Copilot's ~/.copilot and Claude's ~/.claude stay separate. GNU stow does the linking when it
# is installed; otherwise the same per-file relative links are made with ln, so a fresh machine
# without stow ends up identical. Folding is off: a tool's state dir (~/.copilot holds sessions
# and auth tokens) must never become a symlink into the repo.
#
# Real files, and symlinks that point somewhere else, are not overwritten unless force=1, in which
# case the old one is kept as <file>.bak-<timestamp>.
#
# Source me, then: bn_install_stow <stow_dir> <target_dir> <force 0|1> [package...]

bn_install_stow_packages() {
  local dir="$1" p
  shift
  if [ "$#" -gt 0 ]; then printf '%s\n' "$@"; return; fi
  for p in "$dir"/*/; do [ -d "$p" ] && basename "$p"; done
}

# The path a link at $2 must have to reach $1, relative like stow's own links.
bn_install_stow_relpath() {
  python3 -c 'import os,sys; print(os.path.relpath(sys.argv[1], os.path.dirname(sys.argv[2])))' "$1" "$2" 2>/dev/null ||
    echo "$1"
}

bn_install_stow_canon() {
  python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$1" 2>/dev/null ||
    echo "$1"
}

bn_install_stow() {
  local dir="$1" target="$2" force="${3:-0}" pkg src rel dest want stamp
  shift 3 || shift "$#"
  [ -d "$dir" ] || return 0
  dir=$(cd "$dir" && pwd)
  stamp=$(date +%Y%m%d%H%M%S)

  while IFS= read -r pkg; do
    [ -d "$dir/$pkg" ] || continue
    while IFS= read -r -d '' src; do
      rel="${src#"$dir/$pkg"/}"
      dest="$target/$rel"
      if [ -e "$dest" ] || [ -L "$dest" ]; then
        if [ -L "$dest" ] && [ "$(bn_install_stow_canon "$dest")" = "$(bn_install_stow_canon "$src")" ]; then
          continue
        fi
        if [ "$force" = "1" ]; then
          mv "$dest" "$dest.bak-$stamp"
          echo "stow: kept $dest as $dest.bak-$stamp"
        else
          echo "note: $dest already exists and is not the tracked link; re-run with --force-home to replace it ($pkg)" >&2
          continue
        fi
      fi
      mkdir -p "$(dirname "$dest")"
      want=$(bn_install_stow_relpath "$src" "$dest")
      ln -s "$want" "$dest"
    done < <(find "$dir/$pkg" -type f -print0)
    echo "Linked stow package '$pkg' into $target"
  done < <(bn_install_stow_packages "$dir" "$@")
}
