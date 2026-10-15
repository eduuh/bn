#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
head_sha=$(git -C "$repo" rev-parse HEAD)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin" "$tmp/cache"
cat >"$tmp/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1-}" == "--version" ]]; then
  echo "cargo 9.9.9"
  exit 0
fi

printf 'CARGO_TARGET_DIR=%s\nGITHUB_SHA=%s\nARGS=%s\n' \
  "${CARGO_TARGET_DIR-}" "${GITHUB_SHA-}" "$*" >"$RUN_LOG"

mkdir -p "${CARGO_TARGET_DIR}/debug"
cat >"${CARGO_TARGET_DIR}/debug/bn" <<'BN'
#!/usr/bin/env bash
printf 'BN_BIN=%s\nBN_ARGS=%s\n' "$0" "$*"
BN
cat >"${CARGO_TARGET_DIR}/debug/bn-mcp" <<'BNMCP'
#!/usr/bin/env bash
exit 0
BNMCP
chmod +x "${CARGO_TARGET_DIR}/debug/bn" "${CARGO_TARGET_DIR}/debug/bn-mcp"
EOF
chmod +x "$tmp/bin/cargo"

run_case() {
  local name="$1" expected_target="$2"
  shift 2
  local out="$tmp/$name.out"
  local log="$tmp/$name.log"

  (
    cd "$repo"
    env PATH="$tmp/bin:$PATH" RUN_LOG="$log" "$@" ./.bn/jobs/run.sh --version >"$out"
  )

  grep -Fx "CARGO_TARGET_DIR=$expected_target" "$log" >/dev/null
  grep -Fx "GITHUB_SHA=$head_sha" "$log" >/dev/null
  grep -Fx "ARGS=build -p bn-cli -p bn-mcp" "$log" >/dev/null
  grep -Fx "BN_BIN=$expected_target/debug/bn" "$out" >/dev/null
  grep -Fx "BN_ARGS=--version" "$out" >/dev/null
}

default_target="$tmp/cache/bn/cargo-target/bn"
run_case default "$default_target" XDG_CACHE_HOME="$tmp/cache"

override_target="$tmp/custom-target"
run_case override "$override_target" XDG_CACHE_HOME="$tmp/cache" CARGO_TARGET_DIR="$override_target"

echo "build run wrapper: PASS"
