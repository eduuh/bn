#!/usr/bin/env bash

bn_install_workspan() {
  local bindir="$1" cfgdir="$2" target
  target="$bindir/workspan"
  if [ -e "$target" ] || [ -L "$target" ]; then
    if [ ! -L "$target" ] || [ "$(readlink "$target")" != bn ]; then
      echo "error: refusing to replace existing $target with Workspan" >&2
      return 1
    fi
  else
    ln -s bn "$target" || return 1
  fi
  if [ -e "$cfgdir/workspan" ] || [ -L "$cfgdir/workspan" ]; then
    if [ ! -L "$cfgdir/workspan" ] || [ "$(readlink "$cfgdir/workspan")" != "$target" ]; then
      echo "error: refusing to replace existing $cfgdir/workspan anchor" >&2
      return 1
    fi
  else
    ln -s "$target" "$cfgdir/workspan" || return 1
  fi
  if [ "${WORKSPAN_INSTALL_WP:-0}" = 1 ]; then
    if command -v wp >/dev/null 2>&1 || [ -e "$bindir/wp" ] || [ -L "$bindir/wp" ]; then
      echo "note: wp already exists; leaving it unchanged" >&2
    else
      ln -s workspan "$bindir/wp" || return 1
    fi
  fi
}
