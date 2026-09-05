#!/bin/bash
# Prove the editor MCP server actually works, end to end, in a throwaway repo — a real nvim, a real
# MCP session over stdio, real tool calls. Nothing here touches your editor, your config, or any
# repo you care about; the sandbox and its nvim die with the script.
#
#   ./scripts/try-nvim-mcp.sh                 # run the checks
#   NVIM_MCP_PLUGIN=<dir> ./scripts/try-nvim-mcp.sh   # plugin checkout to load (default: find it)
#
# Why a script and not a Rust test: what can break here is the wiring between three programs bn
# does not own — nvim, the nvim-mcp plugin, and the nvim-mcp binary — and the failure this catches
# is exactly the one that unit tests cannot see. The released 0.7.2 binary globs /tmp for the
# plugin's sockets while the plugin on main registers them under $XDG_RUNTIME_DIR, so a mismatched
# pair discovers nothing and reports it as "no editor open". Run this after changing either half.
set -u
REPO="${BN_REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
SB="$(mktemp -d)"
NVIM_PID=""

cleanup() {
  [ -n "$NVIM_PID" ] && kill "$NVIM_PID" 2>/dev/null
  rm -rf "$SB"
}
trap cleanup EXIT INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

command -v nvim >/dev/null 2>&1 || fail "nvim not found."
command -v python3 >/dev/null 2>&1 || fail "python3 not found (it drives the MCP session)."
command -v nvim-mcp >/dev/null 2>&1 || {
  echo "SKIP: nvim-mcp is not on PATH." >&2
  echo "      Add linw1995/nvim-mcp to your nvim config with build = 'cargo install --path .'," >&2
  echo "      which installs the binary from the same commit as the plugin." >&2
  exit 0
}

# The plugin half. Point NVIM_MCP_PLUGIN at a checkout, or let it find the one your plugin manager
# already installed — that is the copy your real editor loads, so it is the one worth testing.
plugin="${NVIM_MCP_PLUGIN:-}"
if [ -z "$plugin" ]; then
  for cand in \
    "$HOME/.local/share/nvim/lazy/nvim-mcp" \
    "$HOME/.local/share/nvim/site/pack/"*/start/nvim-mcp \
    "$HOME/.local/share/nvim/site/pack/"*/opt/nvim-mcp; do
    [ -f "$cand/lua/nvim-mcp/init.lua" ] && { plugin="$cand"; break; }
  done
fi
[ -n "$plugin" ] && [ -f "$plugin/lua/nvim-mcp/init.lua" ] || {
  echo "SKIP: no nvim-mcp plugin checkout found." >&2
  echo "      Install the plugin, or set NVIM_MCP_PLUGIN=<dir containing lua/nvim-mcp/init.lua>." >&2
  exit 0
}
echo "nvim-mcp: $(command -v nvim-mcp)"
echo "plugin:   $plugin"

# A git repo, because the plugin names its socket after the git root and the server looks it up the
# same way — matching those two IS the thing under test.
mkdir -p "$SB/repo"
(
  cd "$SB/repo" &&
    git init -q . &&
    printf 'local function greet(name)\n  return "hello " .. name\nend\n\nreturn { greet = greet }\n' > mod.lua &&
    git -c user.email=t@t -c user.name=t add -A &&
    git -c user.email=t@t -c user.name=t commit -q -m init
) || fail "could not create the sandbox repo"

# A minimal editor: this repo's plugin, nothing else. A stripped config keeps the test about the
# wiring instead of about whatever is in your real init.lua.
cat > "$SB/init.lua" <<LUA
vim.opt.rtp:append("$plugin")
require("nvim-mcp").setup({})
LUA

(cd "$SB/repo" && exec nvim --headless -u "$SB/init.lua" mod.lua </dev/null >/dev/null 2>&1) &
NVIM_PID=$!

# Wait for the editor to advertise itself rather than sleeping a guess.
for _ in $(seq 1 40); do
  kill -0 "$NVIM_PID" 2>/dev/null || fail "nvim exited before it started listening"
  ls "${XDG_RUNTIME_DIR:-/tmp}"/nvim-mcp.*.sock /tmp/nvim-mcp.*.sock >/dev/null 2>&1 && break
  sleep 0.25
done

python3 -u - "$SB/repo" <<'PY'
import json, subprocess, sys

repo = sys.argv[1]
failures = []


def check(ok, label, detail=""):
    print(("  ok   " if ok else "  FAIL ") + label + (("  — " + detail) if detail and not ok else ""))
    if not ok:
        failures.append(label)


srv = subprocess.Popen(
    ["nvim-mcp", "--connect", "auto"],
    cwd=repo,
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    text=True, bufsize=1,
)


def send(obj):
    srv.stdin.write(json.dumps(obj) + "\n")
    srv.stdin.flush()


def rpc(id_, method, params=None):
    send({"jsonrpc": "2.0", "id": id_, "method": method, "params": params or {}})
    # Read past notifications. The server pushes `notifications/tools/list_changed` the moment it
    # attaches to an editor, and mistaking that for the response leaves every later read one frame
    # behind — which surfaces as a hang, not an error.
    while True:
        line = srv.stdout.readline()
        if not line:
            print("the server exited. stderr:\n" + srv.stderr.read(), file=sys.stderr)
            sys.exit(1)
        msg = json.loads(line)
        if msg.get("id") == id_:
            return msg


def call(id_, tool, args):
    r = rpc(id_, "tools/call", {"name": tool, "arguments": args})
    if "error" in r:
        return None, json.dumps(r["error"])
    content = r["result"].get("content") or []
    text = "".join(c.get("text", "") for c in content)
    return text, None


print("MCP session:")
r = rpc(1, "initialize", {
    "protocolVersion": "2025-06-18",
    "capabilities": {},
    "clientInfo": {"name": "bn-try-nvim-mcp", "version": "0"},
})
check("result" in r, "initialize handshake", json.dumps(r)[:200])
send({"jsonrpc": "2.0", "method": "notifications/initialized"})

# The whole point of --connect auto: it found the editor without being told where it is.
r = rpc(2, "resources/read", {"uri": "nvim-connections://"})
conns = json.loads(r["result"]["contents"][0]["text"]) if "result" in r else []
check(bool(conns), "--connect auto attached to the sandbox nvim",
      "no connection — plugin and binary are probably from different commits")
if not conns:
    print("stderr:\n" + srv.stderr.read()[:2000], file=sys.stderr)
    sys.exit(1)
cid = conns[0]["id"] if isinstance(conns[0], dict) else conns[0]

r = rpc(3, "tools/list")
names = {t["name"] for t in r["result"]["tools"]}
for want in ("list_buffers", "buffer_diagnostics", "lsp_definition", "lsp_references", "exec_lua"):
    check(want in names, f"tools/list advertises {want}")

text, err = call(4, "list_buffers", {"connection_id": cid})
check(text is not None and "mod.lua" in text, "list_buffers sees the file the editor opened",
      err or (text or "")[:200])

# A diagnostic the test controls, so the assertion does not depend on a language server being
# installed and warm. This proves the diagnostic path end to end; whether rust-analyzer (or any
# other server) attaches in a real worktree is a separate, environment-specific question.
lua = (
    "local ns = vim.api.nvim_create_namespace('bn_try')\n"
    "local buf = vim.fn.bufnr('mod.lua')\n"
    # `source` is not optional here: nvim-mcp's deserializer requires it, and a diagnostic without
    # one fails the whole call. Real language servers always set it.
    "vim.diagnostic.set(ns, buf, {{ lnum = 1, col = 0, severity = vim.diagnostic.severity.ERROR,"
    " source = 'bn-try', message = 'bn-try-nvim-mcp planted this' }})\n"
    "return buf\n"
)
text, err = call(5, "exec_lua", {"connection_id": cid, "code": lua})
check(err is None, "exec_lua reaches the editor", err or "")
buf = "".join(ch for ch in (text or "") if ch.isdigit())

text, err = call(6, "buffer_diagnostics", {"connection_id": cid, "id": int(buf or 0)})
check(text is not None and "bn-try-nvim-mcp planted this" in (text or ""),
      "buffer_diagnostics returns the diagnostic", err or (text or "")[:300])

srv.terminate()
print()
if failures:
    print("FAILED: " + ", ".join(failures))
    sys.exit(1)
print("All checks passed — the editor MCP wiring works end to end.")
PY
status=$?
[ "$status" -eq 0 ] || fail "the MCP session did not pass (see above)"
echo
echo "Sandbox: $SB (removed on exit). Repo under test: $REPO"
