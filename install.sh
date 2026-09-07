#!/usr/bin/env bash
# Deploy bn. Layers:
#   --core   Rust CLI only (the engine). Works anywhere — e.g. Codespaces, no tmux.
#   (full)   default: also installs the config/workflow/ layer (tmux + worktree orchestration).
#   --agents opt-in extra: global npm helpers for AXI-style agent workflows (gnhf,
#            lavish-axi) plus the upstream Lavish agent-skill. Also enabled by
#            BN_INSTALL_AGENT_TOOLS=1. Combines with either mode.
# The Rust crates have zero tmux dependency; the tmux layer is opt-in glue on top.
#
# This script never compiles anything: it installs main's published release binaries and wires
# every layer around them. Compiling an unmerged checkout is ./build.sh's job — it builds, then
# re-enters this script with the internal --from-build flag so the wiring lives in one place.
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

mode="full"
agents="${BN_INSTALL_AGENT_TOOLS:-0}"
# Internal, set by ./build.sh once it has compiled this checkout: take the binaries from its target
# dir instead of the release. Not for hand use — on its own it finds nothing to install.
from_build=0
for arg in "$@"; do
  case "$arg" in
    --core)       mode="core" ;;
    --agents)     agents=1 ;;
    --from-build) from_build=1 ;;
    *)
      echo "error: unknown flag '$arg' (use --core and/or --agents)" >&2
      exit 1
      ;;
  esac
done

# bn home — the single hub for tooling: source (repo/), binaries (bin/), config (config/),
# the jobs symlink, and bn-managed repos (repos/). New + fresh installs default to ~/.bn;
# an existing legacy install (~/.config/bn present, ~/.bn absent, BN_HOME unset) stays put so
# a plain re-run never surprise-migrates a daily driver. Notes/metrics/state are NOT here —
# they live in the synced notes repo. This mirrors resolver.rs::{bn_home,bin_dir,config_dir}.
bn_home="${BN_HOME:-$HOME/.bn}"
legacy_cfg="$HOME/.config/bn"
if [ -n "${BN_HOME:-}" ] || [ -d "$bn_home" ] || [ ! -d "$legacy_cfg" ]; then
  layout="home"
  cfgdir="${BN_CONFIG_DIR:-$bn_home/config}"
  # Bindir default mirrors resolver.rs::bin_dir() exactly: prefer <home>/bin when BN_HOME is
  # set or it already exists, else keep an existing legacy ~/.bin (so a config-only migration
  # doesn't yank binaries out from under a stow-managed ~/.bin that's still on PATH), else
  # default to <home>/bin. Without this, self-update would silently relocate the binary.
  if [ -n "${BN_HOME:-}" ] || [ -d "$bn_home/bin" ]; then
    default_bindir="$bn_home/bin"
  elif [ -d "$HOME/.bin" ]; then
    default_bindir="$HOME/.bin"
  else
    default_bindir="$bn_home/bin"
  fi
else
  layout="legacy"
  cfgdir="${BN_CONFIG_DIR:-$legacy_cfg}"
  default_bindir="$HOME/.bin"
fi

# Where binaries + tool symlinks land. Defaults to <home>/bin (or legacy ~/.bin).
# A manager whose bindir is a tracked symlink (e.g. dotfiles) can set BN_BIN_DIR to
# an unmanaged dir on PATH (e.g. ~/.local/bin) so install never clobbers tracked files.
bindir="${BN_BIN_DIR:-$default_bindir}"
mkdir -p "$bindir" "$cfgdir"

# In the ~/.bn layout, keep ~/.config/bn working as a compat alias for the real config dir:
# every hardcoded $HOME/.config/bn/... reference (tmux.conf, shell scripts) then
# resolves through the symlink. Only link when ~/.config/bn is absent or already a symlink —
# never clobber a real legacy dir (that needs an explicit migration).
if [ "$layout" = "home" ] && [ "$cfgdir" != "$legacy_cfg" ]; then
  if [ ! -e "$legacy_cfg" ] || [ -L "$legacy_cfg" ]; then
    mkdir -p "$(dirname "$legacy_cfg")"
    ln -sfn "$cfgdir" "$legacy_cfg"
    echo "Linked $legacy_cfg -> $cfgdir (compat alias)"
  else
    echo "note: $legacy_cfg is a real dir; leaving it. Move its contents into $cfgdir and replace it with a symlink to fully migrate." >&2
  fi
fi
echo "bn layout: $layout (home=$bn_home, config=$cfgdir, bin=$bindir)"
# Canonicalize to an absolute path so the stable anchor below (~/.config/bn/bn) never
# records a relative or symlinked bindir. Reject a temp dir outright: bn must be installed
# somewhere persistent, or the anchor dangles the moment the temp dir is reaped — silently
# bricking every tmux-bar bn call (tmux-status, attn badge, cron). This is the failure that
# left ~/.config/bn/bn -> /tmp/tmp.XXall dashboards empty.
bindir="$(cd "$bindir" && pwd -P)"
case "$bindir" in
  /tmp/* | /var/tmp/* | /private/tmp/* | */tmp.??????????/*)
    echo "error: refusing to install bn into a temp dir ($bindir)." >&2
    echo "       Set BN_BIN_DIR to a persistent dir on PATH, e.g. BN_BIN_DIR=\$HOME/.local/bin" >&2
    exit 1
    ;;
esac

# --- core: the Rust engine ------------------------------------------------------
# Rust bn is the only implementation: the zsh bn it grew out of is gone, so every command
# lives in the binary and an unknown one is a clap error rather than a pass-through.
#
# Binaries come from the rolling GitHub release, never from a compile here. main publishes a tested
# build after every push, so a new machine is wired up in the time three archives take to download
# instead of the minutes a cold cargo build costs — the whole point of the split. When the binaries
# you need are not in that release yet (an unmerged branch, a local patch, a platform the release
# skips), run ./build.sh instead; it compiles and hands the result back here.
slug="${BN_REPO_SLUG:-eduuh/bn}"
# shellcheck source=scripts/install-toolchain.sh
source "$repo/scripts/install-toolchain.sh"
# shellcheck source=scripts/install-tmux.sh
source "$repo/scripts/install-tmux.sh"

# Atomically place a binary: copy to a temp in the destination dir, then rename over the target.
# A rename on the same filesystem is atomic, so a process (e.g. a cron firing mid-install) that
# execs the binary sees either the old or the new file — never a half-written one, which a
# truncate-and-write `install` can hand a concurrent run as a clap exit 2.
atomic_install() {
  local src="$1" dst="$2" tmp
  tmp="$(mktemp "$(dirname "$dst")/.$(basename "$dst").XXXXXX")" || return 1
  cp "$src" "$tmp" && chmod 0755 "$tmp" && mv -f "$tmp" "$dst" || { rm -f "$tmp"; return 1; }
}

# Download the prebuilt bn, bn-web, and bn-mcp for this platform from the latest GitHub release.
# Non-zero on any missing tool, unknown platform, or unusable asset — and each failure says which
# one it was. There is no local-compile fallback behind this any more, so a silent return would
# leave the person at the keyboard guessing between five different causes.
download_release() {
  local target tmp app archive url bin src bn_src=""
  if ! command -v tar >/dev/null 2>&1; then
    echo "error: tar not found — cannot unpack the release archives." >&2
    return 1
  fi
  if ! target=$(bn_install_detect_target); then
    echo "error: no release target triple for this platform ($(uname -sm))." >&2
    return 1
  fi
  tmp=$(mktemp -d)
  echo "Downloading the latest $slug release for ${target}…"
  for app in bn-cli bn-web bn-mcp; do
    archive="${app}-${target}.tar.xz"
    # Prefer authenticated `gh` so this works for a PRIVATE repo (unauthenticated curl on
    # releases/latest/download 404s there). Fall back to curl when the repo is public or gh
    # is absent. Omitting the tag downloads the latest release.
    if command -v gh >/dev/null 2>&1 &&
      gh release download -R "$slug" --pattern "$archive" -D "$tmp" --clobber >/dev/null 2>&1; then
      :
    elif command -v curl >/dev/null 2>&1; then
      # Only reaches a PUBLIC repo: GitHub's browser download endpoint 404s on a private one even
      # with a token, which is why authenticated `gh` is tried first and named in the error below.
      url="https://github.com/${slug}/releases/latest/download/${archive}"
      if ! curl -fsSL "$url" -o "$tmp/$archive"; then
        echo "error: could not download $archive from the latest $slug release." >&2
        rm -rf "$tmp"
        return 1
      fi
    else
      echo "error: neither gh nor curl is available to fetch $archive." >&2
      rm -rf "$tmp"
      return 1
    fi
    if ! tar -xf "$tmp/$archive" -C "$tmp"; then
      echo "error: $archive did not unpack — the asset looks truncated or corrupt." >&2
      rm -rf "$tmp"
      return 1
    fi
  done
  for bin in bn bn-web bn-mcp; do
    src=$(find "$tmp" -type f -name "$bin" -perm -u+x | head -n1)
    if [ -z "$src" ]; then
      echo "error: the release archives for $target contain no $bin binary." >&2
      rm -rf "$tmp"
      return 1
    fi
    if [ "$bin" = "bn" ]; then bn_src="$src"; fi
  done
  # A GNU/Linux archive can exist yet need a newer glibc than this machine has. Probe before
  # replacing anything, so a binary that cannot run here never lands on top of one that does.
  if ! "$bn_src" --version >/dev/null 2>&1; then
    echo "error: the downloaded bn will not run here (usually a glibc older than the one the" >&2
    echo "       release was built against). Compile the checkout instead: ./build.sh" >&2
    rm -rf "$tmp"
    return 1
  fi
  for bin in bn bn-web bn-mcp; do
    src=$(find "$tmp" -type f -name "$bin" -perm -u+x | head -n1)
    if ! atomic_install "$src" "$bindir/$bin"; then
      echo "error: could not place $bin into $bindir." >&2
      rm -rf "$tmp"
      return 1
    fi
  done
  rm -rf "$tmp"
  echo "Installed prebuilt bn + bn-web + bn-mcp ($target) from the latest release."
}

# Place the binaries ./build.sh has just compiled. That script re-enters this one with
# --from-build rather than wiring anything itself, so the anchors, layers, cron and MCP
# registration below run in exactly one place however the binaries were obtained.
install_built_binaries() {
  # build.sh exports the profile it compiled with; its default is Cargo.toml's `local`.
  local target_dir="${BN_INSTALL_TARGET_DIR:-$repo/target/bn-install}/${BN_INSTALL_PROFILE:-local}" bin
  for bin in bn bn-web bn-mcp; do
    if [ ! -x "$target_dir/$bin" ]; then
      echo "error: $target_dir/$bin is missing." >&2
      echo "       --from-build is internal; run ./build.sh to compile and install this checkout." >&2
      exit 1
    fi
  done
  # Copy, don't symlink: a symlink into the target dir breaks on `cargo clean`,
  # silently bricking the live `bn`. Re-run ./build.sh after source changes to refresh.
  for bin in bn bn-web bn-mcp; do
    atomic_install "$target_dir/$bin" "$bindir/$bin"
  done
  echo "Installed bn + bn-web + bn-mcp into $bindir from $target_dir (source build)."
}

if [ "$from_build" = "1" ]; then
  install_built_binaries
else
  download_release || {
    echo "error: could not install bn from the latest $slug release." >&2
    echo "       A private repo's assets are only reachable through an authenticated gh, so on a" >&2
    echo "       fresh machine run \`gh auth login\` first and try again." >&2
    echo "       To compile this checkout instead: ./build.sh" >&2
    exit 1
  }
fi

# Sweep the retired bash fallback: older installs symlinked $bindir/bn-bash at the repo's zsh
# `bn`, which no longer exists. Left alone it is a dangling symlink on every upgraded box.
rm -f "$bindir/bn-bash"

# Remove the retired wrapper only when it is the guard installed by bn. Never touch a user's own
# tmux executable.
if [ -f "$bindir/tmux" ] && grep -q "refuse destructive kill verbs" "$bindir/tmux"; then
  rm -f "$bindir/tmux" "$bindir/tmux.real"
  echo "Removed the retired tmux guard from $bindir"
fi

# Stable binary anchor: the tmux status bar (and tmux-lib) call bn by absolute path
# because the tmux server's PATH lacks $bindir. ~/.config/bn/bn is bindir-independent,
# so the config never hardcodes ~/.bin vs ~/.local/bin. Created in the core path too,
# so `bn cron`/`bn tmux-status` badges work on a core-only (no-tmux) box.
# Guard: never anchor to a missing target — a dangling anchor fails silently in every bar
# call. If the binary isn't there, something upstream went wrong; fail loud instead.
if [ ! -x "$bindir/bn" ]; then
  echo "error: $bindir/bn is missing or not executable; refusing to link $cfgdir/bn." >&2
  exit 1
fi
ln -sfn "$bindir/bn" "$cfgdir/bn"

# --- retire the supervised-dispatcher units of the old design -------------------------------------
# #1322 made the queue dispatcher bn's own resident loop: any bn invocation that finds the lock
# free spawns one, and a running one hands itself over when the binary changes. A unit left from
# before then is not harmless — systemd restarts its copy every ten seconds into a lock it can
# never take, and on a binary change the two supervisors race to hand over. Nothing writes these
# units any more, so an install is the moment to take them away. Idempotent and quiet when there
# is nothing to do.
deregister_legacy_services() {
  local unit_dir removed="" unit plist
  unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
  if command -v systemctl >/dev/null 2>&1; then
    for unit in bn-queue.service bn-queue-tick.timer bn-queue-tick.service; do
      if [ -e "$unit_dir/$unit" ]; then
        systemctl --user disable --now "$unit" >/dev/null 2>&1 || true
        rm -f "$unit_dir/$unit"
        removed="$removed $unit"
      fi
    done
    if [ -n "$removed" ]; then
      systemctl --user daemon-reload >/dev/null 2>&1 || true
    fi
    systemctl --user reset-failed bn-queue.service bn-queue-tick.service >/dev/null 2>&1 || true
  fi
  if command -v launchctl >/dev/null 2>&1; then
    plist="$HOME/Library/LaunchAgents/com.bn.queue.plist"
    if [ -e "$plist" ]; then
      launchctl bootout "gui/$(id -u)/com.bn.queue" >/dev/null 2>&1 || true
      rm -f "$plist"
      removed="$removed com.bn.queue"
    fi
  fi
  if [ -n "$removed" ]; then
    echo "Retired legacy dispatcher unit(s):$removed — bn supervises its own queue loop now."
  fi
  return 0
}
deregister_legacy_services

# Neutralize any older bn sitting earlier on PATH than the one just installed. Installing to a
# dir that PATH resolves *after* another dir holding a bn means `bn` keeps running stale even
# though the install succeeded — the exact "I built the latest but nothing changed" trap, and it
# reports success the whole way. Repoint every such copy at the freshly installed binary so PATH
# order can't serve a stale bn.
#
# This used to check only ~/.bin, which missed the case it was written for: the documented escape
# hatch for a stow-managed ~/.bin is BN_BIN_DIR=~/.local/bin, so the stale copy lands in
# ~/.local/bin — a dir this never looked at. One machine ran a day-old `bn` in every shell while
# cron ran the current build through the anchor. So walk PATH itself rather than guessing which
# dir is the offender, and keep ~/.bin in the sweep even when it isn't on the installer's PATH
# (an interactive shell's PATH is usually longer than a script's).
#
# Deliberately conservative — this rewrites files outside $bindir:
#   * only dirs under $HOME, so a packaged /usr/local/bin/bn is never touched;
#   * only names that already exist, so no new PATH entry is ever created;
#   * never $bindir itself, resolved to its real path first — a stow-managed ~/.bin is often a
#     symlink to the dotfiles .bin and canonicalizes to the same dir as $bindir. A string compare
#     misses that and `ln` aborts with "are the same file" (fatal under set -e).
shadow_dirs=""
# Split on ':' only — a substitution like ${PATH//:/ } would also split a dir containing a space.
IFS=':' read -r -a path_dirs <<< "$PATH"
for candidate in ${path_dirs[@]+"${path_dirs[@]}"} "$HOME/.bin"; do
  [ -n "$candidate" ] && [ -d "$candidate" ] || continue
  candidate_real="$(cd "$candidate" && pwd -P)" || continue
  [ "$candidate_real" != "$bindir" ] || continue
  case "$candidate_real" in "$HOME"/*) ;; *) continue ;; esac
  case ":$shadow_dirs:" in *":$candidate_real:"*) continue ;; esac
  shadow_dirs="$shadow_dirs:$candidate_real"

  for b in bn bn-web; do
    if [ -e "$candidate/$b" ] || [ -L "$candidate/$b" ]; then
      ln -sfn "$bindir/$b" "$candidate/$b"
      echo "Repointed $candidate/$b -> $bindir/$b (was shadowing the current binary on PATH)"
    fi
  done
done

# Cron job scripts live in-repo (version-controlled); `bn run` resolves them via this path.
ln -sfn "$repo/config/jobs" "$cfgdir/jobs"
echo "Linked $cfgdir/jobs -> $repo/config/jobs"

# Strict self-describing agents (config/agents/<name>.md) live in-repo too; `bn` resolves the
# shipped tier through this symlink. Every manifest declares command/staff/hosted/specialist;
# `agent-scheduler` owns their `on_calendar` values, while static instructions stay in Markdown.
ln -sfn "$repo/config/agents" "$cfgdir/agents"
echo "Linked $cfgdir/agents -> $repo/config/agents"

# Invocable prompts (config/prompts/<name>.md) are the shipped tier the MCP prompt resolver serves
# as slash commands. No symlink needed: `global_skill_dirs()` reaches them through the stable
# `$cfgdir/repo` anchor (linked below) as `$cfgdir/repo/config/prompts`, so `bn serve` serves them
# in any launch mode. `$cfgdir/skills` is left alone as the user's *personal* skill tier.

# The shipped cron manifest (config/crons.toml) — resolved via config_dir()/crons.toml.
ln -sfn "$repo/config/crons.toml" "$cfgdir/crons.toml"
echo "Linked $cfgdir/crons.toml -> $repo/config/crons.toml"

# --- private config: link config.toml from personal-notes -----------------------
# config.toml is sensitive (note roots, work-repo names, tracker URL), so it lives in the
# private personal-notes repo — never in this one — and is symlinked here. personal-notes is
# already checked out on each machine; BN_PERSONAL_NOTES_DIR points at it (default below).
# If its bn/config.toml isn't there yet, a local config.toml is scaffolded so bn still works.
config="$cfgdir/config.toml"
personal_dir="${BN_PERSONAL_NOTES_DIR:-$HOME/projects/worktree/personal-notes/main}"
if [ ! -e "$config" ]; then
  src="$personal_dir/bn/config.toml"
  if [ -f "$src" ]; then
    ln -sf "$src" "$config"
    echo "Linked $config -> $src"
  else
    "$bindir/bn" setup >/dev/null 2>&1 || true
    echo "Scaffolded a local $config — move it to $src in personal-notes and re-run to sync it."
  fi
fi

# --- external runtime dependencies --------------------------------------------
# The Rust core has no tmux/fzf dependency, but the full workflow intentionally keeps those
# external boundaries: tmux owns sessions/windows and fzf owns the interactive picker UI. Install
# the required CLIs when a common package manager is available; otherwise print the same preflight
# report so a fresh machine knows exactly what to install. Set BN_INSTALL_DEPS=0 to skip.
required_cli_deps() {
  local scope="$1"
  if [ "$scope" = "full" ]; then
    echo "git jq curl tar tmux fzf"
  else
    echo "git jq curl tar"
  fi
}

optional_cli_deps() {
  local scope="$1"
  if [ "$scope" = "full" ]; then
    echo "gh az zsh lazygit nvim workiq"
  else
    echo "gh az"
  fi
}

dep_manager() {
  if command -v apt-get >/dev/null 2>&1; then echo apt
  elif command -v dnf >/dev/null 2>&1; then echo dnf
  elif command -v yum >/dev/null 2>&1; then echo yum
  elif command -v pacman >/dev/null 2>&1; then echo pacman
  elif command -v brew >/dev/null 2>&1; then echo brew
  else return 1
  fi
}

dep_package() {
  local manager="$1" tool="$2"
  # macOS ships /usr/bin/tar; Homebrew's GNU tar installs as `gtar`, so don't try to satisfy
  # command -v tar with a brew package.
  if [ "$manager" = "brew" ] && [ "$tool" = "tar" ]; then return 1; fi
  echo "$tool"
}

run_as_root() {
  if [ "${EUID:-$(id -u)}" = 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    echo "warning: sudo not found; cannot install missing dependencies automatically." >&2
    return 1
  fi
}

install_required_cli_deps() {
  local scope="$1" manager tool pkg
  local missing=() pkgs=()
  [ "${BN_INSTALL_DEPS:-1}" = "0" ] && return 0

  for tool in $(required_cli_deps "$scope"); do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
  done
  [ "${#missing[@]}" -gt 0 ] || return 0

  manager=$(dep_manager 2>/dev/null || true)
  if [ -z "$manager" ]; then
    echo "warning: missing required CLIs:${missing[*]} (no supported package manager found)." >&2
    return 0
  fi
  for tool in "${missing[@]}"; do
    pkg=$(dep_package "$manager" "$tool" 2>/dev/null || true)
    [ -n "$pkg" ] && pkgs+=("$pkg")
  done
  [ "${#pkgs[@]}" -gt 0 ] || {
    echo "warning: missing required CLIs:${missing[*]} (not installable via $manager)." >&2
    return 0
  }

  echo "Installing missing required CLIs via $manager:${pkgs[*]}"
  case "$manager" in
    apt)
      run_as_root apt-get update
      run_as_root apt-get install -y "${pkgs[@]}"
      ;;
    dnf)    run_as_root dnf install -y "${pkgs[@]}" ;;
    yum)    run_as_root yum install -y "${pkgs[@]}" ;;
    pacman) run_as_root pacman -S --needed --noconfirm "${pkgs[@]}" ;;
    brew)   brew install "${pkgs[@]}" ;;
  esac || echo "warning: automatic dependency install failed; see preflight below." >&2
}

preflight_report() {
  local scope="$1" t missing=""
  echo "Preflight — required CLIs:"
  for t in $(required_cli_deps "$scope"); do
    if command -v "$t" >/dev/null 2>&1; then echo "  ✓ $t"; else echo "  ✗ $t (missing)"; missing="$missing $t"; fi
  done
  echo "Preflight — optional CLIs (features degrade if absent):"
  for t in $(optional_cli_deps "$scope"); do
    if command -v "$t" >/dev/null 2>&1; then echo "  ✓ $t"; else echo "  · $t (not found)"; fi
  done
  [ -n "$missing" ] && echo "warning: install the missing required CLIs:$missing" >&2
  return 0
}

install_required_cli_deps "$mode"
# The package manager's tmux only guarantees *a* tmux. The workflow layer has a version floor
# (scripts/install-tmux.sh says why); build the pinned release over it when the box is below.
# Runs after the dependency install so curl/tar are present and the distro tmux stays as the
# fallback if the build cannot happen here. Same BN_INSTALL_DEPS=0 opt-out as the deps above.
if [ "$mode" = "full" ] && [ "${BN_INSTALL_DEPS:-1}" != "0" ]; then
  bn_install_ensure_tmux
fi
preflight_report "$mode"

# --- agent layer (opt-in): global npm helpers for AXI-style agent workflows ------
# Never runs by default — mutating the global npm prefix is opt-in via --agents or
# BN_INSTALL_AGENT_TOOLS=1. Idempotent: an already-installed helper is skipped, not
# reinstalled. Requires npm to already exist (we don't install Node); fails loud otherwise.
install_agent_tools() {
  if ! command -v npm >/dev/null 2>&1; then
    echo "error: --agents needs npm on PATH and none was found." >&2
    echo "       Install Node.js/npm first (package manager or nvm), then re-run ./install.sh --agents" >&2
    exit 1
  fi
  local pkg
  for pkg in gnhf lavish-axi; do
    if npm ls -g --depth=0 "$pkg" >/dev/null 2>&1; then
      echo "Agent tool $pkg already installed globally — skipped."
    else
      echo "Installing agent tool $pkg (npm install -g)…"
      npm install -g "$pkg" || {
        echo "error: npm install -g $pkg failed." >&2
        exit 1
      }
    fi
  done
  # Install the upstream public `lavish` agent-skill (Agent Skills format, agentskills.io) so
  # harnesses learn to reach for Lavish on plan/diagram/report tasks — it pairs with bn's own
  # `lavish` prompt, which distills the visual plan into the branch note. Global (-g →
  # ~/.claude/skills) to cover every repo; best-effort and fed no stdin so an interactive prompt
  # can't hang the installer. The skill runs `npx -y lavish-axi` on demand — no extra npm install
  # beyond the CLI above.
  echo "Installing the upstream lavish agent-skill (npx skills add …)…"
  if npx -y skills add kunchenguid/lavish-axi --skill lavish -g </dev/null >/dev/null 2>&1; then
    echo "Installed the upstream lavish skill (~/.claude/skills/lavish)."
  else
    echo "warning: could not install the lavish skill — run" >&2
    echo "         'npx skills add kunchenguid/lavish-axi --skill lavish -g' manually if you use Lavish." >&2
  fi
  if [ "${BN_SETUP_LAVISH_HOOKS:-0}" = "1" ]; then
    echo "Setting up Lavish hooks (BN_SETUP_LAVISH_HOOKS=1)…"
    lavish-axi setup hooks || echo "warning: lavish-axi setup hooks failed." >&2
  else
    echo "Skipped Lavish hooks (set BN_SETUP_LAVISH_HOOKS=1 to opt in)."
  fi
}
if [ "$agents" = "1" ]; then
  install_agent_tools
fi

# --- validate Markdown agents + register cron timers (idempotent) ----------------
# Catalog errors are migration errors, not an optional integration failure: print the exact
# manifest path and stop before installing a scheduler that would fail every minute.
if ! "$bindir/bn" agents validate; then
  echo "error: invalid Markdown agent catalog; fix the manifest above and re-run install.sh" >&2
  # Most often the manifest is fine and the *binary* is behind it. install.sh links this
  # checkout's config/agents but installs bn from the rolling release, so a platform whose
  # release assets have gone stale gets a bn that has never heard of a field main added since.
  # Compare the two before editing anything the checkout is right about.
  echo "       If the manifest matches main, this is version skew: the installed bn is" >&2
  echo "       $("$bindir/bn" --version 2>/dev/null || echo unknown)" >&2
  echo "       while config/agents comes from this checkout. Build the checkout instead: ./build.sh" >&2
  exit 1
fi

# Pick up crons.toml additions/changes automatically — no manual `bn cron register` after an
# install or update. One `agent-scheduler` timer owns every manifest schedule. Diffs against what's
# registered, so it only touches what changed; on a non-systemd host it just reports the plan.
# Platform timer installation remains non-fatal; catalog validation above does not.
"$bindir/bn" cron register || true

echo "Completion: add to ~/.zshrc -> source <(bn completion zsh)"
echo "Wrappers:   add to ~/.zshrc -> source <(bn shell-init zsh)   # claude|copilot start inside bn"
echo "            (nothing is appended for you — your rc is usually a symlink into a dotfiles repo."
echo "             'bn doctor' warns while the wrappers are missing, inactive, or shadowed.)"

# --- register the bn MCP server globally for MCP clients (Claude Code + VS Code Copilot + Copilot
# CLI) ---------------------------------------------------------------------------------------------
# bn's MCP tools are served by a per-worktree **stdio** launcher: each client spawns `bn mcp serve`,
# which derives its (repo, branch) scope from the client's cwd, so no per-call headers or running
# server are needed. Registering at *user* scope means every repo gets the current-worktree bn
# tools + session-safe skill prompts without a per-project .mcp.json. `bn` must be on PATH for
# clients to spawn it. The launcher is untokened (#523), so no credential is written.
register_mcp() {
  command -v jq >/dev/null 2>&1 || { echo "Skipped global MCP registration (jq not found)."; return; }
  local tmp
  local tools='["bn_add","bn_done","bn_pr","bn_goal","bn_cat","bn_context","bn_status","bn_sync","bn_handoff_write","bn_handoff_read"]'

  # Spawn bn by ABSOLUTE path, never the bare name. A GUI-launched client (Claude Desktop, VS Code
  # from the Dock) inherits none of the shell PATH — its PATH is /usr/bin:/bin:/usr/sbin:/sbin, so
  # `command: "bn"` simply fails to start and the client shows an MCP server that never connects.
  # An absolute path is also immune to a stale shadowing copy earlier on PATH. Prefer the stable
  # $cfgdir/bn anchor for the same reason the cron units use it: it survives the binary moving
  # between $bindir layouts.
  local bn_cmd="$cfgdir/bn"
  [ -x "$bn_cmd" ] || bn_cmd="$bindir/bn"

  # nvim-mcp — the editor MCP server (buffers, LSP diagnostics, definitions, references, rename),
  # registered next to bn wherever it is installed. Absolute path for the same reason bn is: a
  # GUI-launched client inherits none of the shell PATH. `--connect auto` discovers the nvim
  # running in the client's git root, so one user-scope entry covers every repo and no
  # machine-local socket path is ever written.
  #
  # bn deliberately does not install it. The binary and the nvim plugin must come from the SAME
  # commit — released 0.7.2 globs /tmp for sockets while the plugin on main registers them under
  # $XDG_RUNTIME_DIR, so a crates.io binary paired with a git plugin discovers nothing, silently.
  # Upstream's own lazy.nvim spec builds the binary from the checked-out plugin
  # (`build = "cargo install --path ."`), which is the one setup that cannot skew. So: the editor
  # owns the install, install.sh owns the registration.
  local nvim_mcp_cmd
  nvim_mcp_cmd=$(command -v nvim-mcp 2>/dev/null || true)
  if [ -z "$nvim_mcp_cmd" ]; then
    echo "Skipped nvim MCP registration (nvim-mcp not on PATH). To enable the editor tools, add"
    echo "  { 'linw1995/nvim-mcp', build = 'cargo install --path .', config = function() require('nvim-mcp').setup({}) end }"
    echo "to your nvim config, then re-run this script."
  fi

  # Claude Code — user scope lives in ~/.claude.json under .mcpServers
  local cc="$HOME/.claude.json"
  [ -f "$cc" ] || echo '{}' > "$cc"
  tmp=$(mktemp)
  if jq --arg bn "$bn_cmd" '.mcpServers.bn = {type: "stdio", command: $bn, args: ["mcp","serve"]}' \
        "$cc" > "$tmp" 2>/dev/null; then
    mv "$tmp" "$cc"; echo "Registered bn MCP for Claude Code (user scope) -> $cc"
  else
    rm -f "$tmp"; echo "warning: could not update $cc (not valid JSON?)" >&2
  fi
  if [ -n "$nvim_mcp_cmd" ]; then
    tmp=$(mktemp)
    if jq --arg nv "$nvim_mcp_cmd" \
          '.mcpServers.nvim = {type: "stdio", command: $nv, args: ["--connect","auto"]}' \
          "$cc" > "$tmp" 2>/dev/null; then
      mv "$tmp" "$cc"; echo "Registered nvim MCP for Claude Code (user scope) -> $cc"
    else
      rm -f "$tmp"; echo "warning: could not add nvim to $cc (not valid JSON?)" >&2
    fi
  fi

  # VS Code Copilot — user mcp.json. Covers Linux/WSL (~/.config/Code, ~/.vscode-server) AND
  # macOS (~/Library/Application Support/Code), stable + Insiders. The macOS paths matter: VS Code
  # stores user config under Application Support there, so a Linux-only list silently matched
  # nothing on every Mac and printed "No VS Code user profile found" even with VS Code installed.
  # Only writes profiles that exist; skips (without clobbering) any file jq can't parse, e.g. one
  # containing JSONC comments.
  local wrote=0 dir f
  for dir in \
    "$HOME/.config/Code/User" \
    "$HOME/.config/Code - Insiders/User" \
    "$HOME/Library/Application Support/Code/User" \
    "$HOME/Library/Application Support/Code - Insiders/User" \
    "$HOME/.vscode-server/data/User" \
    "$HOME/.vscode-server-insiders/data/User"; do
    [ -d "$dir" ] || continue
    f="$dir/mcp.json"
    # treat a missing or whitespace-only file as an empty object
    grep -q '[^[:space:]]' "$f" 2>/dev/null || echo '{}' > "$f"
    tmp=$(mktemp)
    if jq --arg bn "$bn_cmd" '.servers.bn = {type: "stdio", command: $bn, args: ["mcp","serve"]}' \
          "$f" > "$tmp" 2>/dev/null; then
      mv "$tmp" "$f"; echo "Registered bn MCP for VS Code Copilot -> $f"; wrote=1
    else
      rm -f "$tmp"; echo "warning: could not update $f (JSONC comments?) — see README for manual setup" >&2
    fi
    [ -n "$nvim_mcp_cmd" ] || continue
    tmp=$(mktemp)
    if jq --arg nv "$nvim_mcp_cmd" \
          '.servers.nvim = {type: "stdio", command: $nv, args: ["--connect","auto"]}' \
          "$f" > "$tmp" 2>/dev/null; then
      mv "$tmp" "$f"; echo "Registered nvim MCP for VS Code Copilot -> $f"
    else
      rm -f "$tmp"; echo "warning: could not add nvim to $f (JSONC comments?)" >&2
    fi
  done
  if [ "$wrote" = 0 ]; then
    echo "No VS Code user profile found; see README for manual Copilot MCP setup."
  fi

  # Claude Desktop — a separate product from Claude Code, with its own config file and its own
  # `mcpServers` key. Only register when it is actually installed.
  #
  # Caveat: the stdio launcher scopes itself to the client's cwd, and a GUI app's cwd is `/`, so the
  # branch-note *tools* report "no branch scope" there. The skill *prompts* still work (seeded from
  # the global tiers, not the worktree). Pointing Desktop at a repo needs the HTTP transport
  # (`bn serve` + X-Bn-Repo/X-Bn-Branch).
  local desktop_dir="$HOME/Library/Application Support/Claude"
  [ -d "$desktop_dir" ] || desktop_dir="$HOME/.config/Claude"   # Linux
  if [ -d "$desktop_dir" ]; then
    local dcfg="$desktop_dir/claude_desktop_config.json"
    grep -q '[^[:space:]]' "$dcfg" 2>/dev/null || echo '{}' > "$dcfg"
    tmp=$(mktemp)
    if jq --arg bn "$bn_cmd" '.mcpServers.bn = {command: $bn, args: ["mcp","serve"]}' \
          "$dcfg" > "$tmp" 2>/dev/null; then
      mv "$tmp" "$dcfg"; echo "Registered bn MCP for Claude Desktop -> $dcfg"
    else
      rm -f "$tmp"; echo "warning: could not update $dcfg (not valid JSON?)" >&2
    fi
  fi

  # GitHub Copilot CLI — a separate product from the VS Code extension, with its own user
  # config at ~/.copilot/mcp-config.json (key .mcpServers). It ignores the VS Code mcp.json
  # above. Only register when the CLI is present.
  if command -v copilot >/dev/null 2>&1 || [ -d "$HOME/.copilot" ]; then
    local cli="$HOME/.copilot/mcp-config.json"
    mkdir -p "$HOME/.copilot"
    grep -q '[^[:space:]]' "$cli" 2>/dev/null || echo '{}' > "$cli"
    tmp=$(mktemp)
    if jq --argjson tools "$tools" --arg bn "$bn_cmd" \
          '.mcpServers.bn = {type: "local", command: $bn, args: ["mcp","serve"], tools: $tools}' \
          "$cli" > "$tmp" 2>/dev/null; then
      mv "$tmp" "$cli"; echo "Registered bn MCP for GitHub Copilot CLI -> $cli"
    else
      rm -f "$tmp"; echo "warning: could not update $cli (not valid JSON?)" >&2
    fi
    if [ -n "$nvim_mcp_cmd" ]; then
      tmp=$(mktemp)
      # `tools: ["*"]` — unlike bn, whose surface is deliberately trimmed to the session tools,
      # every nvim tool is worth having: which ones matter depends entirely on the task.
      if jq --arg nv "$nvim_mcp_cmd" \
            '.mcpServers.nvim = {type: "local", command: $nv, args: ["--connect","auto"], tools: ["*"]}' \
            "$cli" > "$tmp" 2>/dev/null; then
        mv "$tmp" "$cli"; echo "Registered nvim MCP for GitHub Copilot CLI -> $cli"
      else
        rm -f "$tmp"; echo "warning: could not add nvim to $cli (not valid JSON?)" >&2
      fi
    fi
  fi
}
register_mcp

# PIDs listening on a TCP port, via whichever tool the host actually has. lsof is present on
# macOS by default and common on Linux; ss covers modern Linux without lsof; fuser is the last
# resort. Prints nothing (success) when the port is free or nothing can inspect it.
port_listeners() {
  local port="$1"
  if command -v lsof >/dev/null 2>&1; then
    lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true
  elif command -v ss >/dev/null 2>&1; then
    ss -lptnH "sport = :$port" 2>/dev/null | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | sort -u
  elif command -v fuser >/dev/null 2>&1; then
    fuser -n tcp "$port" 2>/dev/null | tr -s ' ' '\n' | grep -E '^[0-9]+$' || true
  fi
}

# Restart the dashboard so the freshly installed binary is the one actually serving.
#
# bn-web bakes its UI in at compile time (build.rs embeds crates/bn-web/ui/dist), so a *running*
# server keeps serving the UI it was built with. Replacing $bindir/bn-web on disk changes nothing
# until the process is replaced too — "I reinstalled and the dashboard looks identical" is this,
# every time.
#
# Stopping is not sufficient on its own. `bn serve stop` unregisters the managed service, but a
# process reparented to init (PPID 1 — an orphan left by an earlier plist/unit generation) is no
# longer tracked by launchd/systemd and survives the unload. It then keeps the port, and every
# respawn dies with "cannot bind 127.0.0.1:7770: Address already in use" in a crash loop that,
# from the outside, is indistinguishable from nothing having happened.
restart_bn_web() {
  local port pids i
  # Ask bn where it serves (config `[web].port`), rather than assuming the default.
  port="$("$bindir/bn" serve status --json 2>/dev/null | sed -n 's/.*"port":\([0-9]*\).*/\1/p')"
  [ -n "$port" ] || port=7770

  "$bindir/bn" serve stop >/dev/null 2>&1 || true

  # Reap whatever still holds the port, tracked or orphaned. TERM first so bn-web can close its
  # listener cleanly; escalate to KILL only if it is still there after a few seconds.
  for i in 1 2 3 4 5; do
    pids="$(port_listeners "$port")"
    [ -n "$pids" ] || break
    if [ "$i" -ge 4 ]; then
      # shellcheck disable=SC2086  # word-splitting is intended: pids is a PID list
      kill -9 $pids 2>/dev/null || true
    else
      # shellcheck disable=SC2086
      kill $pids 2>/dev/null || true
    fi
    sleep 1
  done

  if [ -n "$(port_listeners "$port")" ]; then
    echo "warning: port $port is still held by another process — bn-web will fail to bind." >&2
    echo "         Inspect it with: lsof -iTCP:$port -sTCP:LISTEN" >&2
  fi

  "$bindir/bn" serve --daemon || true
}

# Auto-start `bn serve` (bn-web) for the dashboard, orchestrator, and the tmux `bnweb:<port>`
# badge. MCP no longer depends on it — each client spawns its own `bn mcp serve` stdio launcher —
# so this is best-effort and can be skipped with BN_SETUP_NO_SERVE=1.
if [ "${BN_SETUP_NO_SERVE:-0}" != "1" ]; then
  restart_bn_web
else
  echo "Skipped bn serve auto-start (BN_SETUP_NO_SERVE=1)."
fi

if [ "$mode" = "core" ]; then
  echo "Core install complete (Rust CLI only; skipped the tmux workflow layer)."
  exit 0
fi

# --- full: the tmux + worktree workflow layer -----------------------------------
echo "Installing the workflow layer (tmux + worktrees)…"
# Canonical, location-independent anchor for the tmux config. tmux.conf resolves every
# popup script through $HOME/.config/bn/repo/config/workflow/bin, so the config works wherever
# this repo is cloned without baking in an absolute checkout path.
ln -sfn "$repo" "$cfgdir/repo"
echo "Linked $cfgdir/repo -> $repo"

# Copilot CLI and Claude Code both load *native* Agent Skills (~/.copilot/skills/<name>/SKILL.md,
# ~/.claude/skills/<name>/SKILL.md), so (re)generate both roots here from the shipped prompts
# (config/prompts) plus any personal skills. Claude also gets these over MCP via `bn serve`; the
# native copy is what keeps this in step with the dashboard, whose skill CRUD writes both roots on
# every edit. Idempotent and safe (only ever touches bn-managed SKILL.md files), and a harness whose
# root holds a same-named skill bn doesn't own is skipped rather than failing the other.
if "$bindir/bn" skills install >/dev/null 2>&1; then
  echo "Regenerated ~/.copilot/skills and ~/.claude/skills from bn skills (slash commands)"
else
  echo "note: 'bn skills install' skipped/failed; native slash-command skills not refreshed" >&2
fi

# Upstream open-source skills (Agent Skills format) that cover what bn's own prompts do not:
# plan interrogation, conversation handoff, merge-conflict resolution, git guardrails,
# verify-before-done, and snapshot-based Playwright driving. `npx skills add -g` puts each under
# ~/.agents/skills and symlinks it into ~/.claude/skills; Claude Code and Copilot CLI both read
# those roots natively, so one install serves both harnesses. Best-effort: needs npx, and a
# failed entry is reported, never fatal. Names must not collide with bn's own skills, because
# `bn skills install` refuses to overwrite a skill it does not own. One `<owner/repo> <skill>` per
# line. Set BN_SKIP_UPSTREAM_SKILLS=1 to skip.
install_upstream_skills() {
  local entries repo skill failed=""
  entries="
mattpocock/skills grill-me
mattpocock/skills handoff
mattpocock/skills resolving-merge-conflicts
mattpocock/skills git-guardrails-claude-code
obra/superpowers verification-before-completion
microsoft/playwright-cli playwright-cli
"
  if [ "${BN_SKIP_UPSTREAM_SKILLS:-0}" = "1" ]; then
    echo "Skipped upstream agent skills (BN_SKIP_UPSTREAM_SKILLS=1)."
    return 0
  fi
  if ! command -v npx >/dev/null 2>&1; then
    echo "note: npx not found; upstream agent skills not installed (install Node, re-run ./install.sh)." >&2
    return 0
  fi
  echo "Installing upstream agent skills for Claude Code + Copilot CLI (npx skills add …)…"
  while read -r repo skill; do
    [ -n "$repo" ] || continue
    if npx -y skills add "$repo" --skill "$skill" -g -a claude-code -a github-copilot -y </dev/null >/dev/null 2>&1 \
       && [ -e "$HOME/.agents/skills/$skill" ]; then
      echo "  ✓ $skill ($repo)"
    else
      echo "  ✗ $skill ($repo)" >&2
      failed="$failed $skill"
    fi
  done <<EOF_SKILLS
$entries
EOF_SKILLS
  if [ -n "$failed" ]; then
    echo "warning: some upstream skills did not install:$failed" >&2
    echo "         re-run: npx skills add <owner/repo> --skill <name> -g -a claude-code -a github-copilot -y" >&2
  fi
}
install_upstream_skills

# One global instructions file for both harnesses: config/AGENTS.md, read through the repo anchor
# so it follows whichever bn build is live (link-build snapshots config/ into every build).
# Copilot CLI reads ~/.copilot/copilot-instructions.md, so that becomes a symlink to it; Claude
# Code reads ~/.claude/CLAUDE.md, which gets one idempotent `@` import line. A real
# copilot-instructions.md the user wrote is never replaced.
agents_md="$cfgdir/repo/config/AGENTS.md"
if [ -f "$agents_md" ]; then
  copilot_ci="$HOME/.copilot/copilot-instructions.md"
  mkdir -p "$HOME/.copilot"
  if [ -L "$copilot_ci" ] || [ ! -e "$copilot_ci" ]; then
    ln -sfn "$agents_md" "$copilot_ci"
    echo "Linked $copilot_ci -> $agents_md (Copilot CLI global instructions)"
  else
    echo "note: $copilot_ci is a regular file — not replaced; reference $agents_md from it by hand" >&2
  fi
  claude_md="$HOME/.claude/CLAUDE.md"
  import_line="@$agents_md"
  mkdir -p "$HOME/.claude"
  [ -e "$claude_md" ] || : > "$claude_md"
  if grep -qxF "$import_line" "$claude_md"; then
    echo "Claude Code global CLAUDE.md already imports $agents_md"
  else
    printf '\n%s\n' "$import_line" >> "$claude_md"
    echo "Added '$import_line' to $claude_md (Claude Code global instructions)"
  fi
else
  echo "note: $agents_md missing — global agent instructions not linked" >&2
fi

# Review specialists (repo-scoped PR reviewers) are compiled to *native* Copilot/Claude custom
# agents (~/.copilot/agents/<name>.agent.md + ~/.claude/agents/<name>.md), so a running
# Copilot/Claude session discovers them under `/agent`. Their source `.md` files live in the
# personal overlay ($BN_OVERLAY_DIR, default the personal-notes `bn/agents` dir) layered on top of
# the notes roots, so `bn agents new --scope global-personal` writes them into personal-notes and
# they sync across machines. (Re)compile them here from the discovered specialist sources.
# Idempotent, guarded, and prunes stale bn-managed agent files.
if "$bindir/bn" agents install >/dev/null 2>&1; then
  echo "Regenerated ~/.copilot/agents + ~/.claude/agents from bn review specialists"
else
  echo "note: 'bn agents install' skipped/failed; review specialists not refreshed" >&2
fi

# The worktree/session/agent helpers are now native bn subcommands (`bn tat`/`bn wt clone`/
# `bn agent launch`) — nothing extra to put on PATH here.

# Own ~/.tmux.conf: a symlink to the in-repo config keeps it live-editable (edit the repo
# file, reload tmux — no reinstall). Back up any pre-existing real file once.
tmux_conf="$HOME/.tmux.conf"
if [ -e "$tmux_conf" ] && [ ! -L "$tmux_conf" ]; then
  mv "$tmux_conf" "$tmux_conf.bak"
  echo "Backed up existing ~/.tmux.conf -> ~/.tmux.conf.bak"
fi
ln -sfn "$repo/config/workflow/tmux.conf" "$tmux_conf"
echo "Linked ~/.tmux.conf -> $repo/config/workflow/tmux.conf"

# tmux plugins are vendored in-repo (config/workflow/plugins/, sourced directly by tmux.conf) — no TPM,
# no network fetch, nothing to bootstrap. A fresh machine works the moment ~/.tmux.conf is linked.
echo "Full install complete."
