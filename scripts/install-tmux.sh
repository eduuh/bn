#!/usr/bin/env bash

# tmux for the workflow layer. Sourced by install.sh (full mode) and by its regression check.
#
# The package-manager tmux is often too old for the workflow layer, so this builds a pinned release
# from source when the installed one sits below the floor. It used to live in the dotfiles setup
# (ensure_tmux_version); it belongs here because bn's tmux.conf is what sets the floor.
#
# Floor: 3.7. Below it, tmux commit f6c9052 (2026-01-06) is missing: an OSC 4 palette reply that
# arrives split across two reads is swallowed up to the split and the remainder is delivered to the
# active pane as keystrokes — the "848/5656" hex fragments that appeared in the shell every time an
# interactive Copilot CLI (which queries all 16 palette entries at startup) opened in another
# window. 3.7 also carries everything the config already relied on (popup borders/titles from 3.4,
# OSC 52 clipboard from 3.3).
BN_TMUX_MIN_VERSION="3.7"
# The release built when the floor is not met. Override: TMUX_VERSION=3.7b ./install.sh
BN_TMUX_VERSION="${TMUX_VERSION:-3.7c}"
# Installed system-wide, not into $bindir: bn's own systemd timers and cron shells run with the
# default PATH (/usr/local/bin:/usr/bin:/bin) and call `tmux` by name, so a copy in ~/.local/bin
# would leave those talking to a different tmux than the shell. /usr/local sorts ahead of the
# distro's /usr/bin on every Linux PATH.
BN_TMUX_PREFIX="${BN_TMUX_PREFIX:-/usr/local}"

# The version `tmux -V` reports, bare: "tmux 3.7c" -> "3.7c", "tmux next-3.8" -> "3.8".
# Pass the -V output as $1 so the check script can feed it strings.
bn_install_tmux_version_of() {
  local out="$1"
  out="${out#tmux }"
  out="${out#next-}"
  printf '%s\n' "$out"
}

# 0 when $1 satisfies the $2 floor. Letter suffixes are patch levels (3.7 < 3.7a < 3.7c), which is
# what `sort -V` does with them; an empty $1 (no tmux) never satisfies anything.
bn_install_tmux_satisfies() {
  local have="$1" min="$2"
  [ -n "$have" ] || return 1
  [ "$(printf '%s\n%s\n' "$min" "$have" | sort -V | head -n1)" = "$min" ]
}

# Build toolchain for the release tarball. bison stays: the tarball ships the generated parser, but
# configure still aborts with "yacc not found" without one (tmux 3.7c on Ubuntu 26.04).
bn_install_tmux_build_deps() {
  local manager="$1"
  case "$manager" in
    apt)    run_as_root apt-get update && run_as_root apt-get install -y libevent-dev libncurses-dev build-essential bison pkg-config ;;
    dnf)    run_as_root dnf install -y libevent-devel ncurses-devel gcc make bison pkgconf-pkg-config ;;
    yum)    run_as_root yum install -y libevent-devel ncurses-devel gcc make bison pkgconfig ;;
    pacman) run_as_root pacman -S --needed --noconfirm libevent ncurses base-devel bison pkgconf ;;
    *)      return 1 ;;
  esac
}

# Download, build and install tmux $BN_TMUX_VERSION under $BN_TMUX_PREFIX.
bn_install_tmux_from_source() {
  local version="$BN_TMUX_VERSION" tmp url
  local jobs="${CARGO_BUILD_JOBS:-$(nproc 2>/dev/null || echo 2)}"
  tmp=$(mktemp -d) || return 1
  url="https://github.com/tmux/tmux/releases/download/${version}/tmux-${version}.tar.gz"
  echo "Building tmux $version from ${url}…"
  if ! curl -fsSL "$url" -o "$tmp/tmux.tar.gz"; then
    echo "error: could not download tmux $version." >&2
    rm -rf "$tmp"
    return 1
  fi
  if ! tar -xzf "$tmp/tmux.tar.gz" -C "$tmp"; then
    echo "error: tmux-$version.tar.gz did not unpack." >&2
    rm -rf "$tmp"
    return 1
  fi
  if ! (cd "$tmp/tmux-$version" && ./configure --prefix="$BN_TMUX_PREFIX" >"$tmp/configure.log" 2>&1 &&
        make -j"$jobs" >"$tmp/make.log" 2>&1); then
    echo "error: tmux $version failed to build; logs in $tmp." >&2
    return 1
  fi
  if ! (cd "$tmp/tmux-$version" && run_as_root make install >"$tmp/install.log" 2>&1); then
    echo "error: tmux $version built but could not be installed under $BN_TMUX_PREFIX; logs in $tmp." >&2
    return 1
  fi
  rm -rf "$tmp"
  hash -r 2>/dev/null || true
  echo "Installed tmux $version into $BN_TMUX_PREFIX/bin."
}

# Make sure the tmux on PATH meets the floor, building the pinned release when it does not.
# Non-fatal by design: a too-old tmux still runs the workflow layer, minus the fixes above, so a
# failed build is a warning here and the preflight report names the gap.
bn_install_ensure_tmux() {
  local have="" manager had_server=0
  if command -v tmux >/dev/null 2>&1; then
    have=$(bn_install_tmux_version_of "$(tmux -V 2>/dev/null)")
  fi
  if bn_install_tmux_satisfies "$have" "$BN_TMUX_MIN_VERSION"; then
    echo "tmux $have meets the workflow floor ($BN_TMUX_MIN_VERSION)."
    return 0
  fi
  if [ -n "$have" ]; then
    echo "tmux $have is below the workflow floor ($BN_TMUX_MIN_VERSION); building $BN_TMUX_VERSION."
  else
    echo "tmux not found; building $BN_TMUX_VERSION."
  fi

  manager=$(dep_manager 2>/dev/null || true)
  if [ "$manager" = "brew" ]; then
    # Homebrew tracks the latest release, so the source build is never needed on macOS.
    brew upgrade tmux 2>/dev/null || brew install tmux || {
      echo "warning: brew could not install tmux >= $BN_TMUX_MIN_VERSION." >&2
      return 0
    }
    echo "Installed tmux $(bn_install_tmux_version_of "$(tmux -V)") via brew."
    return 0
  fi
  if [ -z "$manager" ] || ! bn_install_tmux_build_deps "$manager"; then
    echo "warning: could not install the tmux build toolchain; keeping tmux ${have:-<none>}." >&2
    return 0
  fi
  tmux list-sessions >/dev/null 2>&1 && had_server=1
  bn_install_tmux_from_source || {
    echo "warning: tmux $BN_TMUX_VERSION was not installed; keeping tmux ${have:-<none>}." >&2
    return 0
  }
  local now
  now=$(bn_install_tmux_version_of "$(tmux -V 2>/dev/null)")
  if ! bn_install_tmux_satisfies "$now" "$BN_TMUX_MIN_VERSION"; then
    echo "warning: PATH still resolves tmux to $(command -v tmux) ($now); put $BN_TMUX_PREFIX/bin ahead of it." >&2
    return 0
  fi
  if [ "$had_server" = 1 ]; then
    echo "note: a tmux server from the old version is still running. New sessions pick up $now only"
    echo "      after it exits — save your layout (bn's resurrect hook does this on session close),"
    echo "      then \`tmux kill-server\` and reattach."
  fi
}
