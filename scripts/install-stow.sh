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

bn_install_stow() {
  local dir="$1" target="$2" force="${3:-0}" pkg src rel dest want stamp skipped
  shift 3 || shift "$#"
  [ -d "$dir" ] || return 0
  dir=$(cd "$dir" && pwd)
  stamp=$(date +%Y%m%d%H%M%S)
  local use_stow=0
  command -v stow >/dev/null 2>&1 && use_stow=1

  while IFS= read -r pkg; do
    [ -d "$dir/$pkg" ] || continue
    skipped=0
    while IFS= read -r -d '' src; do
      rel="${src#"$dir/$pkg"/}"
      dest="$target/$rel"
      if [ -e "$dest" ] || [ -L "$dest" ]; then
        if [ -L "$dest" ] && [ "$(readlink -f "$dest")" = "$(readlink -f "$src")" ]; then
          continue
        fi
        if [ "$force" = "1" ]; then
          mv "$dest" "$dest.bak-$stamp"
          echo "stow: kept $dest as $dest.bak-$stamp"
        else
          echo "note: $dest already exists and is not the tracked link; re-run with --force-home to replace it ($pkg)" >&2
          skipped=1
          continue
        fi
      fi
      mkdir -p "$(dirname "$dest")"
      if [ "$use_stow" = "0" ]; then
        want=$(bn_install_stow_relpath "$src" "$dest")
        ln -s "$want" "$dest"
      fi
    done < <(find "$dir/$pkg" -type f -print0)

    # A skipped file would make stow refuse the whole package, so link such a package file by
    # file instead; stow handles only the conflict-free ones.
    if [ "$use_stow" = "1" ] && [ "$skipped" = "1" ]; then
      while IFS= read -r -d '' src; do
        dest="$target/${src#"$dir/$pkg"/}"
        [ -e "$dest" ] || [ -L "$dest" ] || {
          mkdir -p "$(dirname "$dest")"
          ln -s "$(bn_install_stow_relpath "$src" "$dest")" "$dest"
        }
      done < <(find "$dir/$pkg" -type f -print0)
    elif [ "$use_stow" = "1" ]; then
      stow --no-folding --restow -d "$dir" -t "$target" "$pkg" ||
        echo "warning: stow could not fully link package '$pkg'" >&2
    fi
    echo "Linked stow package '$pkg' into $target"
  done < <(bn_install_stow_packages "$dir" "$@")
}
