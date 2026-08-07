#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/install-toolchain.sh
source "$repo/scripts/install-toolchain.sh"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
target=$(bn_install_detect_target)
channel=$(bn_install_toolchain_channel "$repo")
toolbin="$tmp/rustup/toolchains/$channel-$target/bin"
mkdir -p "$toolbin" "$tmp/cargo/bin" "$tmp/home"

for tool in cargo rustc; do
  printf '#!/bin/sh\nexit 0\n' >"$toolbin/$tool"
  chmod +x "$toolbin/$tool"
  ln -s rustup "$tmp/cargo/bin/$tool"
done

(
  export HOME="$tmp/home"
  export CARGO_HOME="$tmp/cargo"
  export RUSTUP_HOME="$tmp/rustup"
  export PATH="/usr/bin:/bin"
  unset BN_INSTALL_CARGO_DIR

  bn_install_activate_cargo "$repo"
  [ "$BN_INSTALL_CARGO_DIR" = "$toolbin" ]
  [ "$(command -v cargo)" = "$toolbin/cargo" ]
  [ "$(command -v rustc)" = "$toolbin/rustc" ]
)

(
  export HOME="$tmp/home"
  export CARGO_HOME="$tmp/empty-cargo"
  export RUSTUP_HOME="$tmp/empty-rustup"
  export PATH="/usr/bin:/bin"
  unset BN_INSTALL_CARGO_DIR

  if bn_install_activate_cargo "$repo"; then
    echo "installer unexpectedly found Cargo in an empty toolchain root" >&2
    exit 1
  fi
)

echo "install toolchain discovery: PASS"
