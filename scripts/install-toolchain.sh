#!/usr/bin/env bash

# Shared installer helpers. This file is sourced by install.sh and its focused regression check.

bn_install_detect_target() {
  case "$(uname -s)" in
    Linux)  case "$(uname -m)" in
              x86_64) echo x86_64-unknown-linux-gnu ;;
              aarch64|arm64) echo aarch64-unknown-linux-gnu ;;
              *) return 1 ;; esac ;;
    Darwin) case "$(uname -m)" in
              x86_64) echo x86_64-apple-darwin ;;
              arm64) echo aarch64-apple-darwin ;;
              *) return 1 ;; esac ;;
    *) return 1 ;;
  esac
}

bn_install_toolchain_channel() {
  local file="$1/rust-toolchain.toml"
  [ -f "$file" ] || return 1
  sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$file" |
    head -n1
}

bn_install_rustup_default() {
  local file="$1/settings.toml"
  [ -f "$file" ] || return 1
  sed -n 's/^[[:space:]]*default_toolchain[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$file" |
    head -n1
}

bn_install_try_cargo_dir() {
  local dir="$1" previous_path="${PATH:-}"
  [ -x "$dir/cargo" ] && [ -x "$dir/rustc" ] || return 1
  PATH="$dir${previous_path:+:$previous_path}"
  export PATH
  if cargo --version >/dev/null 2>&1 && rustc --version >/dev/null 2>&1; then
    BN_INSTALL_CARGO_DIR="$dir"
    export BN_INSTALL_CARGO_DIR
    return 0
  fi
  PATH="$previous_path"
  export PATH
  return 1
}

# Put a complete Cargo + rustc toolchain on PATH.
#
# rustup normally exposes proxies in CARGO_HOME/bin, but an interrupted update can leave those
# symlinks dangling while the actual pinned toolchain remains intact under RUSTUP_HOME/toolchains.
# Prefer the checkout's rust-toolchain.toml, then rustup's configured default, then another
# host-compatible installed toolchain.
bn_install_activate_cargo() {
  local repo="$1" cargo_home rustup_home target="" channel="" default="" candidate

  if command -v cargo >/dev/null 2>&1 &&
     command -v rustc >/dev/null 2>&1 &&
     cargo --version >/dev/null 2>&1 &&
     rustc --version >/dev/null 2>&1; then
    BN_INSTALL_CARGO_DIR="$(dirname "$(command -v cargo)")"
    export BN_INSTALL_CARGO_DIR
    return 0
  fi

  cargo_home="${CARGO_HOME:-$HOME/.cargo}"
  rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
  bn_install_try_cargo_dir "$cargo_home/bin" && return 0

  target="$(bn_install_detect_target 2>/dev/null || true)"
  channel="$(bn_install_toolchain_channel "$repo" 2>/dev/null || true)"
  if [ -n "$channel" ] && [ -n "$target" ]; then
    bn_install_try_cargo_dir "$rustup_home/toolchains/$channel-$target/bin" && return 0
  fi
  if [ -n "$channel" ]; then
    bn_install_try_cargo_dir "$rustup_home/toolchains/$channel/bin" && return 0
  fi

  default="$(bn_install_rustup_default "$rustup_home" 2>/dev/null || true)"
  if [ -n "$default" ]; then
    bn_install_try_cargo_dir "$rustup_home/toolchains/$default/bin" && return 0
  fi

  if [ -n "$target" ]; then
    for candidate in "$rustup_home"/toolchains/*-"$target"/bin; do
      bn_install_try_cargo_dir "$candidate" && return 0
    done
  fi
  return 1
}
