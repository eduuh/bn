#!/bin/sh
# nightly-release — build `main` on THIS machine and publish the binaries to the rolling
# `nightly` GitHub release, which is pinned as the repo's Latest.
#
# Why it exists: the tag-driven cargo-dist workflow (.github/workflows/release.yml) is not
# producing assets — v0.4.1 shipped with none — so install.sh's download_release() always 404s
# and every install falls back to a from-source build. This job supplies the missing half for the
# platform I actually run: it discovers and builds every binary target in the Cargo workspace,
# gates them behind the workspace test suite, and uploads them as cargo-dist-named archives to a
# single rolling release.
#
# Contract:
#   - `nightly` is pinned as the repo's **Latest** release on every run, so it is what the GitHub
#     front page shows and what `releases/latest/download` — the URL install.sh reads — resolves
#     to. bn tracks `main`: this is deliberately the install channel, and the `v*` tags are
#     history. Pinning is explicit (`--latest`) rather than left to gh's date/version default,
#     which would hand the badge to any `v*` release cut after this row was created.
#   - The `nightly` tag is force-moved to the commit that was built, so the release always points
#     at the source inside it.
#   - Asset names match install.sh's download_release(): `<app>-<target>.tar.xz`, with <target>
#     from scripts/install-toolchain.sh, so an installer can consume them unchanged.
#   - It never installs anything. Keeping the installed `bn` in lock-step with main is
#     `self-update`'s job; this one only publishes.
#
# Host-pinned by design (this box builds darwin only). The gate that actually stops a run is
# `[jobs.only_on]` in config.toml, enforced for every job in run_recorded_job(); the `only_on`
# field on a crons.toml entry is registration-intent that cron_gen does not read today.
#
# Idempotent: a stamp records the last commit successfully published, so a run whose HEAD has not
# moved is a no-op. The stamp is written ONLY after a successful upload, so a failed build or a
# red test suite retries on the next tick.
#
# Builds in a dedicated `_release-main` worktree, including its own target/, so release preparation
# never resets a user checkout or contends with self-update's build in the ordinary main worktree.
#
# Queued, not run inline: a cron `bn run` hands every job to the machine-wide queue
# (cron.rs -> queue_producers::maybe_capture_job), and `nightly-release` is listed in
# `bn_core::queue::jobs::HEAVY_JOBS` because a release build plus the workspace suite is minutes of
# work — heavy work is admitted and spawned like an agent instead of holding the dispatcher on the
# express path. So it shows up in `bn queue`, respects capacity and pause, and sits at the queue's
# priority floor where it can never starve work a person is waiting on.
#
# Env knobs:
#   BN_NIGHTLY_TAG        rolling tag name                      (default: nightly)
#   BN_NIGHTLY_TMUX       0 to run inline instead of in a tmux session
#   BN_NIGHTLY_TMUX_SESSION  session name                       (default: bn-nightly)
#   BN_NIGHTLY_TIMEOUT    seconds before the session is killed  (default: 7200)
#   BN_NIGHTLY_FORCE      1 to rebuild + republish the same commit
#   BN_NIGHTLY_DRYRUN     1 to build + test + package, but never touch git/GitHub
#   BN_NIGHTLY_SKIP_TESTS 1 to bypass the test gate (manual runs only — never for cron)
#   BN_MAIN_WORKTREE      explicit checkout override (used by CI; otherwise `_release-main`)
#   BN_SKIP_UI            1 to skip the bn-web UI build (embeds whatever is in ui/dist already)
set -u

export PATH="$HOME/.local/bin:$HOME/.bin:$HOME/.bn/bin:$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"

BN="${BN:-bn}"
TAG="${BN_NIGHTLY_TAG:-nightly}"

log() { printf '  %s\n' "$*" >&2; }
die() { log "$*"; exit 1; }

# cargo is deliberately absent from this list: it is resolved below through the installer's own
# `bn_install_activate_cargo`, not required on PATH. A rustup install whose `~/.cargo/bin/rustup`
# has gone missing leaves every shim there — `cargo` included — a dangling symlink, so a PATH check
# reports "not found" while a perfectly good toolchain sits in ~/.rustup/toolchains. install.sh
# survives that; this job has to as well, or a broken shim silently stops every release.
for tool in git gh jq tar; do
  command -v "$tool" >/dev/null 2>&1 || die "nightly-release: $tool not found — cannot publish"
done

# --- transparency: do the work inside a detached tmux session --------------------------------
# The queue spawns heavy work detached with stdout and stderr on /dev/null (queue_tick's
# spawn_heavy) — deliberate, since minutes of build noise do not belong in the dispatcher's log.
# That leaves nowhere to *watch* a release being built, so this job re-execs itself into a
# detached tmux session and waits on it:
#
#     tmux attach -t bn-nightly
#
# Detached is the point: the pane exists to be looked at, never to appear in front of you, so the
# headless-cron contract in config/jobs/README.md still holds — a firing cron cannot steal focus.
# The outer process waits and exits with the inner status, so `bn run` still records the true
# outcome and the queue's completion callback still fires. The event log, not the pane, remains
# the durable record; the pane is the live view.
SESSION="${BN_NIGHTLY_TMUX_SESSION:-bn-nightly}"
if [ "${BN_NIGHTLY_TMUX:-1}" = "1" ] && [ "${BN_NIGHTLY_INNER:-0}" != "1" ] &&
  [ -z "${TMUX:-}" ] && command -v tmux >/dev/null 2>&1; then
  self=$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")
  rc_file=$(mktemp) || die "nightly-release: cannot create the status file"
  trap 'rm -f "$rc_file"' EXIT INT TERM

  # tmux hands the command to the *server*, which does not inherit this process's environment, so
  # every knob the inner run needs is passed explicitly rather than assumed.
  inner="BN_NIGHTLY_INNER=1 PATH='$PATH' HOME='$HOME'"
  inner="$inner BN_NIGHTLY_TAG='$TAG'"
  inner="$inner BN_NIGHTLY_FORCE='${BN_NIGHTLY_FORCE:-0}'"
  inner="$inner BN_NIGHTLY_DRYRUN='${BN_NIGHTLY_DRYRUN:-0}'"
  inner="$inner BN_NIGHTLY_SKIP_TESTS='${BN_NIGHTLY_SKIP_TESTS:-0}'"
  inner="$inner BN_SKIP_UI='${BN_SKIP_UI:-0}'"
  inner="$inner BN_MAIN_WORKTREE='${BN_MAIN_WORKTREE:-}'"
  inner="$inner BN_CONFIG_DIR='${BN_CONFIG_DIR:-}'"
  inner="$inner sh '$self'; printf '%s\n' \"\$?\" >'$rc_file'"

  # Replace a previous session rather than refusing: the last run's pane is kept on screen (see
  # remain-on-exit below), so without this a finished session would block every later run.
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  tmux new-session -d -s "$SESSION" -n build "$inner" ||
    die "nightly-release: cannot start the '$SESSION' tmux session"
  # Keep the finished pane readable until the next run replaces it. Set immediately after the
  # session exists, so a failure fast enough to beat this line loses its pane — which is why
  # completion is detected through the status file, and why the event log stays the real record.
  tmux set-option -t "$SESSION" -w remain-on-exit on >/dev/null 2>&1 || true

  echo "nightly-release: building in tmux session '$SESSION' — watch it with: tmux attach -t $SESSION"
  waited=0
  limit="${BN_NIGHTLY_TIMEOUT:-7200}"
  while [ ! -s "$rc_file" ]; do
    sleep 5
    waited=$((waited + 5))
    if [ "$waited" -ge "$limit" ]; then
      tmux kill-session -t "$SESSION" 2>/dev/null || true
      die "nightly-release: timed out after ${waited}s — killed the '$SESSION' session"
    fi
  done
  rc=$(cat "$rc_file")
  echo "nightly-release: tmux session '$SESSION' finished (exit $rc); its pane is kept for inspection"
  exit "$rc"
fi

# Workstation releases own a dedicated checkout. It may be fast-forwarded or reset while detached
# only after proving it has no tracked changes; ordinary user worktrees are never selected or
# modified. CI's explicit BN_MAIN_WORKTREE remains immutable and must match GITHUB_SHA.
jobdir=$(CDPATH= cd -- "$(dirname "$0")" && pwd -P)
. "$jobdir/nightly-release-worktree" ||
  die "nightly-release: cannot load checkout lifecycle helper"
repo=$(bn_nightly_prepare_worktree) || exit 1

# Target triple and toolchain both come from the installer's own helpers, so the asset names can
# never drift from what download_release() looks for and the build uses the same cargo install.sh
# would pick — including its fallback straight into ~/.rustup/toolchains when the shims are broken.
. "$repo/scripts/install-toolchain.sh" || die "nightly-release: cannot source install-toolchain.sh"
bn_install_activate_cargo "$repo" ||
  die "nightly-release: no usable Cargo toolchain (rustup shims broken? try 'rustup-init' or repair ~/.cargo/bin)"
target=$(bn_install_detect_target) || die "nightly-release: unsupported platform for a release build"

# --- pin to the prepared latest origin/main ------------------------------------------------
head=$(git -C "$repo" rev-parse HEAD 2>/dev/null) || die "nightly-release: cannot resolve HEAD"
short=$(git -C "$repo" rev-parse --short HEAD)

# --- stamp: skip a commit already published ------------------------------------------------
cfgdir="${BN_CONFIG_DIR:-$HOME/.config/bn}"
stamp="$cfgdir/.nightly-released"
if [ "${BN_NIGHTLY_FORCE:-0}" != "1" ] && [ -f "$stamp" ] &&
  [ "$(cat "$stamp" 2>/dev/null)" = "$head" ]; then
  echo "nightly-release: $TAG already published @ $short — nothing to do"
  exit 0
fi

# --- build ---------------------------------------------------------------------------------
# bn-web bakes its dashboard UI in at compile time (build.rs embeds crates/bn-web/ui/dist), so
# build it first or the published binary serves the "UI not built" placeholder. Node-optional and
# best-effort, exactly like install.sh's build_web_ui — a UI hiccup must not block the Rust
# binaries, it only costs the dashboard page.
ui="$repo/crates/bn-web/ui"
if [ "${BN_SKIP_UI:-0}" != "1" ] && [ -f "$ui/package.json" ] && command -v npm >/dev/null 2>&1; then
  echo "nightly-release: building the bn-web UI…"
  (
    cd "$ui" || exit 1
    if [ -f package-lock.json ]; then npm ci --silent; else npm install --silent; fi
    npm run build
  ) >/dev/null 2>&1 || log "warning: bn-web UI build failed — publishing with whatever is in ui/dist"
fi

bins=$(cd "$repo" && cargo metadata --no-deps --format-version 1 |
  jq -r '
    .packages[] as $package
    | $package.targets[]
    | select(.kind | index("bin"))
    | [$package.name, .name] | @tsv
  ') || die "nightly-release: cannot discover workspace binaries"
[ -n "$bins" ] || die "nightly-release: the workspace has no binary targets"

echo "nightly-release: building every workspace binary @ $short ($target)…"
( cd "$repo" && cargo build --release --workspace --bins ) ||
  die "nightly-release: release build failed @ $short"

# The gate: nothing reaches a downloadable asset unless the workspace suite is green here.
if [ "${BN_NIGHTLY_SKIP_TESTS:-0}" = "1" ]; then
  log "warning: test gate bypassed (BN_NIGHTLY_SKIP_TESTS=1)"
else
  echo "nightly-release: running the workspace test suite…"
  # The workspace suite has a small amount of shared-state test code; run it single-threaded so
  # the release gate matches the stable local path.
  ( cd "$repo" && cargo test --workspace -- --test-threads=1 ) ||
    die "nightly-release: tests failed @ $short — not publishing"
fi

# --- package ---------------------------------------------------------------------------------
# One tar.xz per binary package, laid out like cargo-dist's: a single `<app>-<target>/` directory
# holding the binary. Packages with multiple binary targets use `<package>-<binary>` as the app
# name, keeping every archive unique.
stage=$(mktemp -d) || die "nightly-release: cannot create a staging dir"
trap 'rm -rf "$stage"' EXIT INT TERM
archives=""
while IFS="$(printf '\t')" read -r package binname; do
  count=$(printf '%s\n' "$bins" | awk -F '\t' -v package="$package" '$1 == package { count++ } END { print count + 0 }')
  if [ "$count" -eq 1 ]; then app="$package"; else app="$package-$binname"; fi
  bin="$repo/target/release/$binname"
  [ -x "$bin" ] || die "nightly-release: $bin missing after the build"
  mkdir -p "$stage/$app-$target" || die "nightly-release: cannot stage $app"
  cp "$bin" "$stage/$app-$target/$binname" || die "nightly-release: cannot stage $binname"
  ( cd "$stage" && tar -cJf "$app-$target.tar.xz" "$app-$target" ) ||
    die "nightly-release: cannot archive $app-$target"
  archives="$archives $stage/$app-$target.tar.xz"
done <<EOF
$bins
EOF

if [ "${BN_NIGHTLY_DRYRUN:-0}" = "1" ]; then
  echo "nightly-release (dry-run): would publish $TAG @ $short with:"
  for a in $archives; do echo "  $(basename "$a") ($(wc -c <"$a" | tr -d ' ') bytes)"; done
  exit 0
fi

# --- publish -----------------------------------------------------------------------------------
slug=$(git -C "$repo" remote get-url origin 2>/dev/null |
  sed -e 's#^git@github.com:##' -e 's#^https://github.com/##' -e 's#\.git$##')
[ -n "$slug" ] || die "nightly-release: cannot resolve the origin slug"

# Move the rolling tag to what we built — but only ever FORWARD. Two hosts publish into this one
# release, so fedora finishing a build of an older commit after this Mac published a newer one must
# not drag the tag backwards onto it. When we are the older build we still upload our platform's
# archives; we just leave the tag where the newer build put it.
remote_tag=$(git -C "$repo" ls-remote origin "refs/tags/$TAG" 2>/dev/null | awk 'NR==1 {print $1}')
move_tag=1
if [ -n "$remote_tag" ] && [ "$remote_tag" != "$head" ] &&
  git -C "$repo" cat-file -e "${remote_tag}^{commit}" 2>/dev/null &&
  git -C "$repo" merge-base --is-ancestor "$head" "$remote_tag" 2>/dev/null; then
  move_tag=0
  log "note: '$TAG' already points at a newer commit — uploading $target's assets without moving it"
fi
if [ "$move_tag" = "1" ]; then
  git -C "$repo" push --force --quiet origin "$head:refs/tags/$TAG" ||
    die "nightly-release: cannot move the '$TAG' tag to $short"
fi

# Per-target provenance. Each host uploads only its own archives, so a single commit line in the
# body would be a lie about the other platform's assets the moment the two hosts are out of step.
# Instead the body carries one line per target: ours is rewritten, every other host's is preserved.
prov="- \`$target\` — \`$short\` (built on $(hostname 2>/dev/null || echo unknown))"
others=$(gh release view "$TAG" -R "$slug" --json body -q .body 2>/dev/null |
  grep '^- `' | grep -vF -- "- \`$target\`" || true)
built=$(printf '%s\n%s\n' "$prov" "$others" | grep -v '^[[:space:]]*$' | sort)

notes="Rolling build of \`main\`, published per platform by the machine that built it.

$built

This is the repo's **Latest** release, so it is what the front page shows and what
\`install.sh\` fetches from \`releases/latest/download\` — bn tracks \`main\`, and the versioned
\`v*\` tags remain as history rather than as the install channel. Each line above was uploaded
only after that machine's release build **and** the full workspace test suite passed there.

    gh release download $TAG -R $slug --pattern 'bn-cli-$target.tar.xz'"

# Pinned Latest explicitly, never left to gh's default. That default picks by date and version,
# so a rolling tag whose release row keeps its original creation date would quietly lose the badge
# to any `v*` release cut afterwards. `--latest` restates the intent on every run, which is what
# keeps this on the front page for good.
if gh release view "$TAG" -R "$slug" >/dev/null 2>&1; then
  # Retitle only when this build owns the tag; otherwise the newer build's title stands.
  if [ "$move_tag" = "1" ]; then
    set -- --title "nightly ($short)"
  else
    set --
  fi
  gh release edit "$TAG" -R "$slug" --prerelease=false --latest --notes "$notes" "$@" >/dev/null ||
    die "nightly-release: cannot update the '$TAG' release"
else
  gh release create "$TAG" -R "$slug" --latest --target "$head" \
    --title "nightly ($short)" --notes "$notes" >/dev/null ||
    die "nightly-release: cannot create the '$TAG' release"
fi

# shellcheck disable=SC2086 # $archives is a deliberate space-separated list of paths we built
gh release upload "$TAG" -R "$slug" $archives --clobber >/dev/null ||
  die "nightly-release: asset upload failed"

mkdir -p "$cfgdir"
printf '%s\n' "$head" >"$stamp"

# Feeds the deployment-frequency metric (see config/jobs/README.md). Best-effort: a metrics blip
# must never fail a successful publish.
"$BN" event deploy --field repo=bn --field tag="$TAG" --field commit="$short" >/dev/null 2>&1 || true

asset_count=$(printf '%s\n' "$bins" | grep -c .)
echo "nightly-release: published $TAG @ $short — $asset_count assets for $target"
echo "  https://github.com/$slug/releases/tag/$TAG"
