#!/bin/sh
# cache-sync -- copy a content-addressed build cache between this machine and a tunnel host, so a
# build on one machine restores what the other already built.
#
# Run on the machine you connect FROM (needs the `Host <machine>` entry from tunnel-connect):
#   sh cache-sync.sh <machine> pull|push <cache-dir>
# or via bn run: BN_TUNNEL_TO=<machine> BN_SYNC=pull|push BN_SYNC_DIR=<dir> bn run cache-sync
#
# The cache must be flat and content-addressed: one directory per hash, never changed once
# written (lage/backfill's local cache is exactly that). So only entries the other side lacks are
# copied, and nothing is ever overwritten or deleted. <cache-dir> must be under $HOME; the same
# path relative to $HOME is used on the other machine.
#
# Built for a slow or flaky link. rsync compresses (build output shrinks several-fold) and works
# in batches of BN_SYNC_BATCH entries (default 200). Each batch lands in <cache-dir>/.incoming
# and every entry is moved into place only once its batch is complete, so a build never sees a
# half-copied entry, and an interrupted run resumes where it stopped. Entries written in the last
# 5 minutes are skipped: they may still be being written.
#
# Env: BN_SYNC_MAX_SECONDS (default 1800) stops starting new batches after that long, so a
# pre-build pull cannot hold the build up forever; BN_SYNC_BWLIMIT (KB/s, rsync --bwlimit) caps
# the bandwidth.
set -u

TO="${1:-${BN_TUNNEL_TO:-}}"; [ $# -gt 0 ] && shift
DIR_ACTION="${1:-${BN_SYNC:-}}"; [ $# -gt 0 ] && shift
DIR="${1:-${BN_SYNC_DIR:-}}"
BATCH="${BN_SYNC_BATCH:-200}"
MAX="${BN_SYNC_MAX_SECONDS:-1800}"

say() { echo "cache-sync: $*"; }
die() { echo "cache-sync: $*" >&2; exit 1; }
[ -n "$TO" ] && [ -n "$DIR_ACTION" ] && [ -n "$DIR" ] || die "usage: cache-sync.sh <machine> pull|push <cache-dir>"
case "$DIR_ACTION" in pull|push) ;; *) die "pull or push, not '$DIR_ACTION'" ;; esac
case "$BATCH$MAX${BN_SYNC_BWLIMIT:-0}" in *[!0-9]*) die "BN_SYNC_BATCH, BN_SYNC_MAX_SECONDS and BN_SYNC_BWLIMIT are numbers" ;; esac
DIR=${DIR%/}
case "$DIR" in "$HOME"/*) REL=${DIR#"$HOME"/} ;; *) die "$DIR is not under \$HOME" ;; esac
case "$REL" in *[!A-Za-z0-9_./-]*) die "unsupported characters in $REL" ;; esac
command -v rsync >/dev/null 2>&1 || die "rsync is not installed here"

# A dead link fails in 30s instead of hanging in the SSH banner exchange.
remote() { ssh -o BatchMode=yes -o ConnectTimeout=30 "$TO" "$@"; }
remote "command -v rsync >/dev/null" || die "cannot reach $TO, or rsync is not installed there"
mkdir -p "$DIR"
remote "mkdir -p '$REL'" || die "cannot create $REL on $TO"

# Finished entries (not touched for 5 minutes) on each side, sorted for comm.
list_cmd="cd '$REL' && find . -mindepth 1 -maxdepth 1 -type d ! -name .incoming -mmin +5 -printf '%f\n' | sort"
all_cmd="cd '$REL' && find . -mindepth 1 -maxdepth 1 -type d ! -name .incoming -printf '%f\n' | sort"
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
if [ "$DIR_ACTION" = pull ]; then
  remote "$list_cmd" > "$work/src" || die "cannot list $REL on $TO"
  sh -c "cd && $all_cmd" > "$work/dst"
else
  sh -c "cd && $list_cmd" > "$work/src"
  remote "$all_cmd" > "$work/dst" || die "cannot list $REL on $TO"
fi
comm -23 "$work/src" "$work/dst" > "$work/missing"
total=$(wc -l < "$work/missing" | tr -d ' ')
say "$DIR_ACTION $TO:$REL -- $total of $(wc -l < "$work/src" | tr -d ' ') entries to copy"
[ "$total" -gt 0 ] || exit 0

# Move a finished batch from .incoming into place, on whichever side received it. An entry that
# appeared meanwhile (the other side's own build) wins; ours is dropped.
place='while read -r e; do [ -d ".incoming/$e" ] || continue; if [ -e "$e" ]; then rm -rf ".incoming/$e"; else mv ".incoming/$e" "$e"; fi; done'

bw=""; [ -n "${BN_SYNC_BWLIMIT:-}" ] && bw="--bwlimit=$BN_SYNC_BWLIMIT"
split -l "$BATCH" "$work/missing" "$work/batch."
start=$(date +%s); done_n=0
for b in "$work"/batch.*; do
  if [ $(( $(date +%s) - start )) -ge "$MAX" ]; then
    say "stopping after ${MAX}s; $((total - done_n)) entries left for the next run"
    break
  fi
  n=$(wc -l < "$b" | tr -d ' ')
  # --files-from turns off -a's recursion; -r puts it back. Exit 24 (a file vanished on the
  # sending side) is fine for a cache.
  if [ "$DIR_ACTION" = pull ]; then
    rsync -a -r -z --partial --timeout=120 -e "ssh -o BatchMode=yes -o ConnectTimeout=30" $bw --files-from="$b" "$TO:$REL/" "$DIR/.incoming/"
  else
    rsync -a -r -z --partial --timeout=120 -e "ssh -o BatchMode=yes -o ConnectTimeout=30" $bw --files-from="$b" "$DIR/" "$TO:$REL/.incoming/"
  fi
  rc=$?
  if [ "$rc" -ne 0 ] && [ "$rc" -ne 24 ]; then
    say "rsync failed ($rc) after $done_n of $total entries; the next run resumes from here"
    exit 1
  fi
  if [ "$DIR_ACTION" = pull ]; then
    (cd "$DIR" && sh -c "$place") < "$b"
  else
    remote "cd '$REL' && $place" < "$b"
  fi
  done_n=$((done_n + n))
  say "$done_n/$total entries in place ($(( $(date +%s) - start ))s)"
done
