#!/usr/bin/env bash
set -euo pipefail

: "${REPO:?REPO is required}"
: "${ARTIFACT_AGE_DAYS:?ARTIFACT_AGE_DAYS is required}"
: "${CACHE_AGE_DAYS:?CACHE_AGE_DAYS is required}"

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

now=$(date -u +%s)
artifact_cutoff=$(( now - ARTIFACT_AGE_DAYS * 86400 ))
cache_cutoff=$(( now - CACHE_AGE_DAYS * 86400 ))

is_transient_gh_error() {
  local message=${1,,}
  case "$message" in
    *"unexpected eof"*|*"connection reset"*|*"connection refused"*|*"connection timed out"*|*"i/o timeout"*|*"tls handshake timeout"*|*"502 bad gateway"*|*"503 service unavailable"*|*"504 gateway timeout"*|*"http 500"*|*"http 502"*|*"http 503"*|*"http 504"*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

gh_retry() {
  local label=$1
  shift

  local attempt=1
  local delay=1
  local stdout_file="$tmpdir/$label.stdout"
  local stderr_file="$tmpdir/$label.stderr"
  local stderr
  local rc=0

  while [ "$attempt" -le 3 ]; do
    if "$@" >"$stdout_file" 2>"$stderr_file"; then
      cat "$stdout_file"
      return 0
    else
      rc=$?
    fi

    stderr=$(<"$stderr_file")
    if ! is_transient_gh_error "$stderr"; then
      printf '%s\n' "$stderr" >&2
      return "$rc"
    fi

    if [ "$attempt" -eq 3 ]; then
      printf '%s\n' "$stderr" >&2
      return "$rc"
    fi

    sleep "$delay"
    delay=$(( delay * 2 ))
    attempt=$(( attempt + 1 ))
  done
}

echo "Repo: $REPO"
echo "Deleting artifacts older than ${ARTIFACT_AGE_DAYS}d, caches older than ${CACHE_AGE_DAYS}d"

# --- Artifacts -------------------------------------------------------
artifact_rows="$tmpdir/artifacts.tsv"
if ! gh_retry artifacts gh api --paginate "repos/$REPO/actions/artifacts" \
  --jq '.artifacts[] | [.id, .created_at, .name] | @tsv' >"$artifact_rows"; then
  exit 1
fi

while IFS=$'\t' read -r id created name; do
  [ -n "${id:-}" ] || continue
  created_ts=$(date -u -d "$created" +%s)
  if [ "$created_ts" -lt "$artifact_cutoff" ]; then
    echo "  artifact: deleting #$id '$name' (created $created)"
    gh_retry "artifact-$id" gh api -X DELETE "repos/$REPO/actions/artifacts/$id" >/dev/null || true
  fi
done <"$artifact_rows"

# --- Caches ----------------------------------------------------------
cache_rows="$tmpdir/caches.tsv"
if ! gh_retry caches gh api --paginate "repos/$REPO/actions/caches" \
  --jq '.actions_caches[] | [.id, .created_at, .key] | @tsv' >"$cache_rows"; then
  exit 1
fi

while IFS=$'\t' read -r id created key; do
  [ -n "${id:-}" ] || continue
  created_ts=$(date -u -d "$created" +%s)
  if [ "$created_ts" -lt "$cache_cutoff" ]; then
    echo "  cache: deleting #$id '$key' (created $created)"
    gh_retry "cache-$id" gh api -X DELETE "repos/$REPO/actions/caches/$id" >/dev/null || true
  fi
done <"$cache_rows"

echo "Cleanup complete."
