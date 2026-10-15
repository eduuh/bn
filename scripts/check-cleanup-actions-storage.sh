#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
script="$repo_dir/scripts/cleanup-actions-storage.sh"

case "$(uname -s)" in
  Linux) ;;
  *) echo "This test expects GNU date on Linux." >&2; exit 2 ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

write_stub() {
  local mode=$1
  mkdir -p "$tmp/bin" "$tmp/state"
  cat >"$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

mode=${GH_STUB_MODE:?}
state_dir=${GH_STUB_STATE_DIR:?}
cache_calls_file="$state_dir/cache_calls"
artifact_calls_file="$state_dir/artifact_calls"
delete_calls_file="$state_dir/delete_calls"

count() {
  local file=$1
  local n=0
  [ -f "$file" ] && n=$(<"$file")
  n=$((n + 1))
  printf '%s' "$n" >"$file"
  printf '%s' "$n"
}

if [[ $* == api\ --paginate*actions/artifacts*--jq* ]]; then
    n=$(count "$artifact_calls_file")
    printf '101\t2026-10-01T00:00:00Z\tartifact-one\n'
elif [[ $* == api\ --paginate*actions/caches*--jq* ]]; then
    n=$(count "$cache_calls_file")
    if [ "$mode" = fail-once ] && [ "$n" -eq 1 ]; then
      printf 'Get "https://api.github.com/repos/eduuh/bn/actions/caches": unexpected EOF\n' >&2
      exit 1
    fi
    if [ "$mode" = always-fail ]; then
      printf 'Get "https://api.github.com/repos/eduuh/bn/actions/caches": unexpected EOF\n' >&2
      exit 1
    fi
    printf '201\t2026-10-01T00:00:00Z\tcache-one\n'
elif [[ $* == api\ -X\ DELETE*actions/artifacts/* ]]; then
    count "$delete_calls_file" >/dev/null
elif [[ $* == api\ -X\ DELETE*actions/caches/* ]]; then
    count "$delete_calls_file" >/dev/null
else
    printf 'unexpected gh invocation: %s\n' "$*" >&2
    exit 1
fi
EOF
  chmod +x "$tmp/bin/gh"
  export PATH="$tmp/bin:$PATH"
  export GH_STUB_MODE="$mode"
  export GH_STUB_STATE_DIR="$tmp/state"
}

run_success_case() {
  write_stub fail-once
  output="$(env REPO=eduuh/bn ARTIFACT_AGE_DAYS=7 CACHE_AGE_DAYS=7 "$script" 2>&1)"
  case "$output" in
    *"Cleanup complete."*) ;;
    *) echo "$output" >&2; exit 1 ;;
  esac
  [ "$(cat "$tmp/state/cache_calls")" = "2" ]
  [ -f "$tmp/state/delete_calls" ]
}

run_failure_case() {
  rm -rf "$tmp/state"
  write_stub always-fail
  set +e
  env REPO=eduuh/bn ARTIFACT_AGE_DAYS=7 CACHE_AGE_DAYS=7 "$script" >/dev/null 2>"$tmp/stderr"
  status=$?
  set -e
  [ "$status" -ne 0 ]
  grep -q 'unexpected EOF' "$tmp/stderr"
}

run_success_case
run_failure_case

echo "cleanup-actions-storage retry test: PASS"
