#!/bin/sh
# Bootstrap the bn launchd user agents on macOS. This is a thin wrapper: it locates the installed
# bn and defers to `bn cron register`, which is the single source of truth on every platform. That
# reads config/crons.toml (+ any per-repo crons.toml) and renders + (re)bootstraps a launchd
# user-agent for **every** job — not a hand-maintained subset — then prunes any bn agent no longer
# in the manifest (including the legacy static com.bn.refresh / com.bn.daily agents earlier
# versions of this script installed). Idempotent: re-run any time to reconcile. macOS only.
set -eu

if ! command -v launchctl >/dev/null 2>&1; then
  echo "bn-launchd: launchctl not found — this installer is for macOS." >&2
  echo "  On Linux/WSL use config/jobs/systemd/install.sh instead." >&2
  exit 1
fi

# ~/.config/bn/bn is the stable, bindir-independent anchor the top-level install.sh links to the
# real binary. Fall back to a `bn` on PATH if the anchor isn't there yet.
bn_bin="$HOME/.config/bn/bn"
if [ ! -x "$bn_bin" ]; then
  if command -v bn >/dev/null 2>&1; then
    bn_bin="$(command -v bn)"
  else
    echo "bn-launchd: bn not found (looked for $HOME/.config/bn/bn and \`bn\` on PATH)." >&2
    echo "  Install bn first (run ./install.sh), then re-run this script." >&2
    exit 1
  fi
fi

"$bn_bin" cron register

echo "bn-launchd: registered via \`bn cron register\`. Loaded agents:"
launchctl list | grep com.bn || true
