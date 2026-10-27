#!/usr/bin/env bash
# Exercise only the graphify phase, with stubbed uv/graphify: no network or changes to the user's home.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export HOME="$tmp/home"
mkdir -p "$HOME" "$tmp/bin" "$tmp/uvbin"
# shellcheck source=scripts/install-graphify.sh
source "$repo/scripts/install-graphify.sh"

fail() { echo "check-install-graphify: $*" >&2; exit 1; }

[ "$BN_GRAPHIFY_PLATFORMS" = "claude copilot" ] ||
  fail "BN_GRAPHIFY_PLATFORMS changed unexpectedly: $BN_GRAPHIFY_PLATFORMS"

# uv stub: `tool install` drops a graphify stub into its bin dir; `tool dir --bin` names that dir.
cat > "$tmp/bin/uv" <<EOF
#!/usr/bin/env bash
echo "uv \$*" >> "$tmp/calls"
case "\$*" in
  "tool install graphifyy") cp "$tmp/graphify-stub" "$tmp/uvbin/graphify" ;;
  "tool dir --bin") echo "$tmp/uvbin" ;;
  *) exit 1 ;;
esac
EOF
cat > "$tmp/graphify-stub" <<EOF
#!/usr/bin/env bash
echo "graphify \$*" >> "$tmp/calls"
[ "\$3" != "\${FAILED_PLATFORM:-}" ]
EOF
chmod +x "$tmp/bin/uv" "$tmp/graphify-stub"
base_path="$tmp/bin:/usr/bin:/bin"

# Fresh machine: installs via uv, then registers the skill for both harnesses.
(PATH="$base_path"; bn_install_graphify) > "$tmp/output" 2>&1
grep -qx "uv tool install graphifyy" "$tmp/calls" || fail "graphify was not installed via uv"
grep -qx "graphify install --platform claude" "$tmp/calls" || fail "claude skill not registered"
grep -qx "graphify install --platform copilot" "$tmp/calls" || fail "copilot skill not registered"

# Already installed: no uv call, skills still (re)registered.
: > "$tmp/calls"
(PATH="$tmp/uvbin:$base_path"; bn_install_graphify) > "$tmp/output" 2>&1
! grep -q "^uv " "$tmp/calls" || fail "reinstalled an existing graphify"
[ "$(grep -c '^graphify install' "$tmp/calls")" = 2 ] || fail "expected two skill registrations"

# A failed platform is reported and does not stop the other.
: > "$tmp/calls"
(PATH="$tmp/uvbin:$base_path"; FAILED_PLATFORM=claude bn_install_graphify) > "$tmp/output" 2>&1
grep -q "did not install for: claude" "$tmp/output" || fail "failed platform was not reported"
grep -qx "graphify install --platform copilot" "$tmp/calls" || fail "failure stopped copilot"

# Skip must call nothing.
: > "$tmp/calls"
(PATH="$tmp/uvbin:$base_path"; BN_SKIP_GRAPHIFY=1 bn_install_graphify) > "$tmp/output" 2>&1
[ ! -s "$tmp/calls" ] || fail "skip invoked uv or graphify"
grep -q "Skipped graphify" "$tmp/output" || fail "skip was not reported"

# No uv and no graphify: a note, not a failure.
(PATH="/usr/bin:/bin"; bn_install_graphify) > "$tmp/output" 2>&1 || fail "missing uv was fatal"
grep -q "uv not found" "$tmp/output" || fail "missing uv was not reported"

echo "check-install-graphify: ok"
