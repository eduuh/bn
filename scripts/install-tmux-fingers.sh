#!/usr/bin/env bash
# Install the pinned fingers binary; never download or build from a tmux key binding.

bn_install_ensure_tmux_fingers() {
  local destination="${1:-$HOME/.local/bin}" asset checksum target temporary
  target="$destination/tmux-fingers"
  if [ -x "$target" ] && [ "$("$target" version)" = "2.7.1" ]; then
    echo "tmux-fingers 2.7.1 already installed"
    return 0
  fi
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64)
      asset="tmux-fingers-2.7.1-linux-x86_64"
      checksum="48460fd9a9216d8a461e936b89ac6850a9022467d46ddafa24cfad0bb6cd10cf"
      ;;
    Darwin:arm64)
      asset="tmux-fingers-2.7.1-macos-arm64"
      checksum="32b94499f3569d548a6dffd0d89d88e1d6a600c7a8aa891d2b0ee6cc068a720c"
      ;;
    *)
      echo "tmux-fingers: no pinned binary for this platform; install from upstream source." >&2
      return 1
      ;;
  esac
  mkdir -p "$destination" || return
  temporary=$(mktemp "$destination/.tmux-fingers.XXXXXX") || return
  if ! curl --fail --location --silent --show-error --retry 2 \
    "https://github.com/Morantron/tmux-fingers/releases/download/2.7.1/$asset" \
    -o "$temporary"; then
    rm -f "$temporary"
    echo "tmux-fingers: pinned binary download failed." >&2
    return 1
  fi
  if ! python3 - "$temporary" "$checksum" <<'PY'
import hashlib
from pathlib import Path
import sys

if hashlib.sha256(Path(sys.argv[1]).read_bytes()).hexdigest() != sys.argv[2]:
    sys.exit("tmux-fingers: binary checksum mismatch; refusing installation")
PY
  then
    rm -f "$temporary"
    return 1
  fi
  if ! chmod 755 "$temporary" || ! "$temporary" version; then
    rm -f "$temporary"
    echo "tmux-fingers: downloaded binary cannot run on this machine." >&2
    return 1
  fi
  if ! mv -f "$temporary" "$target"; then
    rm -f "$temporary"
    return 1
  fi
  echo "Installed tmux-fingers 2.7.1 -> $target"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  set -euo pipefail
  bn_install_ensure_tmux_fingers "${1:-$HOME/.local/bin}"
fi
