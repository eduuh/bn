#!/usr/bin/env bash

# graphify (PyPI `graphifyy`, two y's) turns a repo into a queryable knowledge graph so agents ask
# the graph instead of re-reading files. Sourced by install.sh and its check. Full mode installs the
# CLI with uv and registers graphify's own skill for both harnesses (~/.claude/skills/graphify,
# ~/.copilot/skills/graphify); upstream owns that skill, so bn ships no copy of it. Per-repo hooks
# stay opt-in through the setup-bn skill. Set BN_SKIP_GRAPHIFY=1 to skip.
#
# Best-effort, same posture as the upstream skills: no uv or a failed step is a warning, never
# fatal, and every call is fed no stdin so a prompt cannot hang an unattended install.
BN_GRAPHIFY_PLATFORMS="claude copilot"

bn_install_graphify() {
  local platform bin failed=""
  if [ "${BN_SKIP_GRAPHIFY:-0}" = "1" ]; then
    echo "Skipped graphify (BN_SKIP_GRAPHIFY=1)."
    return 0
  fi
  if ! command -v graphify >/dev/null 2>&1; then
    if ! command -v uv >/dev/null 2>&1; then
      echo "note: uv not found; graphify not installed (install uv, re-run ./install.sh)." >&2
      return 0
    fi
    echo "Installing graphify (uv tool install graphifyy)…"
    if ! uv tool install graphifyy </dev/null >/dev/null 2>&1; then
      echo "warning: could not install graphify — run 'uv tool install graphifyy' manually." >&2
      return 0
    fi
    # uv's tool bin dir may not be on this shell's PATH yet; `uv tool update-shell` fixes new shells.
    bin=$(uv tool dir --bin 2>/dev/null || true)
    [ -z "$bin" ] || PATH="$bin:$PATH"
    hash -r 2>/dev/null || true
    if ! command -v graphify >/dev/null 2>&1; then
      echo "warning: graphify installed but not on PATH — run 'uv tool update-shell'." >&2
      return 0
    fi
  fi
  for platform in $BN_GRAPHIFY_PLATFORMS; do
    if graphify install --platform "$platform" </dev/null >/dev/null 2>&1; then
      echo "  ✓ graphify skill ($platform)"
    else
      echo "  ✗ graphify skill ($platform)" >&2
      failed="$failed $platform"
    fi
  done
  if [ -n "$failed" ]; then
    echo "warning: graphify skill did not install for:$failed" >&2
    echo "         re-run: graphify install --platform <name>" >&2
  fi
}
