---
name: nvim-explore
description: Ask the worktree's editor instead of re-deriving it — LSP diagnostics, definitions, references, hover, rename, and a real debugger, over MCP
when: You need a semantic fact about the code — does this compile, where is this defined, who calls it, what type is this — and grep or a full build is the slow way to get it. Also when you want a debugger, because a breakpoint hit gives real values instead of a guess. Needs the `nvim` MCP server (nvim-mcp) and an nvim open on this worktree.
keywords: [nvim, editor, lsp, diagnostics, definition, references, hover, rename, code actions, symbols, nvim-mcp, debugger, debug, dap, nvim-dap, breakpoint, step through, inspect locals, call hierarchy, where is this defined, who calls this]
---
An editor that has already indexed this repo knows things you would otherwise re-derive: which
line the type error is on, where a symbol is defined, everything that calls it. The `nvim` MCP
server (nvim-mcp) exposes that over the same tool interface as everything else.

Reach for it when the question is **semantic**. Keep using grep for text and the build for truth.

## Getting a connection

The connection-aware tools (`list_buffers`, `buffer_diagnostics`, `lsp_definition`, …) only appear
once the server is attached to a running nvim. If all you can see is `get_targets` / `connect` /
`connect_tcp`, there is no editor yet:

1. `get_targets` — lists the nvim instances advertising themselves. A hit means one is running;
   pass its path to `connect` and the rest of the tools appear.
2. Nothing listed? Open one, in the worktree: `bn edit show` puts this worktree's nvim on screen,
   `bn edit land` opens it on the branch's diff. Then `get_targets` again.
3. Still nothing after an editor is open? The editor half is missing — nvim advertises itself only
   with the nvim-mcp plugin loaded (`require('nvim-mcp').setup({})`). Say so plainly and fall back
   to grep and the build; do not keep retrying.

## What it is good for

- **Diagnostics after an edit** — `wait_for_lsp_ready`, then `buffer_diagnostics`. Seconds, against
  a build's minutes, when you want to know whether what you just wrote type-checks.
- **Definitions and references** — `lsp_definition`, `lsp_references`, `lsp_implementations`. The
  answer grep gives you is every textual match; the answer here is the actual binding.
- **Shape of a file** — `lsp_document_symbols` beats reading 800 lines to find three functions.
- **Types you are guessing at** — `lsp_hover` on the expression rather than inferring from names.
- **Repo-wide rename** — `lsp_rename` moves every reference, including the ones your regex missed.
- **Who calls this** — `lsp_call_hierarchy_prepare`, then `lsp_call_hierarchy_incoming_calls` with
  the item it returns. A caller graph, not a text search.
- **Running a debugger** — `exec_lua` reaches nvim-dap, so an agent can set a breakpoint, launch,
  read locals and step. See below; it is the one place where driving the editor beats every
  alternative, because the values are real rather than reasoned about.

## Calling the tools

Three parameter shapes are worth knowing before your first call, because each is rejected outright
rather than coerced:

- **Documents** take **one of** `buffer_id`, `project_relative_path`, or `absolute_path` — not a
  `uri`. A `file://` URI is refused. `list_buffers` gives you both the ids and the paths.
- **Every `lsp_*` tool wants `lsp_client_name`** ("rust-analyzer", "tsserver", …). `lsp_clients`
  lists what is attached; pass the name from there.
- **Positions are flat and zero-based**: `line` and `character` sit alongside `document`, not
  nested in a `position` object.

So a hover is `{connection_id, lsp_client_name, document: {absolute_path}, line, character}`.

## Driving the debugger

`exec_lua` is a full escape hatch into the editor, and the highest-value thing behind it is
nvim-dap: a breakpoint hit gives you the actual values, where reading the code only gives you your
own guess about them.

DAP is asynchronous and `exec_lua` is one round trip, so keep probe state between calls and use
bounded `vim.wait` calls. First check `dap.session()` and `dap.sessions()`; never replace another
debug session. Use `nvim-debug` for runtime investigation and `debug` for the eventual fix.

```lua
-- call 1: arm listeners, set the breakpoint, launch
_G.__dbg = { stopped = false }
local dap = require("dap")
dap.listeners.after.event_stopped["x"] = function() _G.__dbg.stopped = true end
local buf = vim.fn.bufadd(vim.fn.getcwd() .. "/src/main.rs"); vim.fn.bufload(buf)
vim.api.nvim_buf_call(buf, function()
  vim.fn.setpos(".", { 0, 4, 1, 0 }); dap.set_breakpoint()
end)
dap.run({ type = "codelldb", request = "launch", name = "probe",
          program = vim.fn.getcwd() .. "/target/debug/<bin>", cwd = vim.fn.getcwd() })

-- call 2: separate round trip, so require again — and wait for the frame, not just the event
assert(vim.wait(6000, function()
  local s = require("dap").session()
  return _G.__dbg.stopped and s ~= nil and s.current_frame ~= nil
end, 50), "debugger did not stop at a frame before the deadline")
```

Two things will bite you:

- **`stopped` fires before `session().current_frame` is populated.** Wait for the frame itself, not
  just the event, or the next call indexes a nil.
- **`context = "repl"` is not expression evaluation** — codelldb reads it as an LLDB command, so
  `total` comes back as *"'total' is not a valid command"*. Use `context = "watch"` for expressions,
  or the `scopes` → `variables` requests for a whole frame of locals at once.

`dap.continue()` runs to the next hit, so a breakpoint in a loop lets you watch a value evolve. The
program's own stdout arrives on `event_output`.

### Complex failures: inspect the frame, not just the expression

Use the exact thread/source/frame at the intended stop. JS debug adapters can create a child
session for the target: inspect the stopped session, not just the launcher. A previous frame
must not be mistaken for a new stop after stepping. Each request below completes asynchronously;
wait for its callback with a bounded deadline and surface its error before using its response.

```lua
local s = assert(require("dap").session(), "no debug session")
local frame = assert(s.current_frame, "not stopped at a frame")
_G.__dbg = _G.__dbg or {}
s:request("stackTrace", { threadId = s.stopped_thread_id }, function(err, body)
  if err then error(vim.inspect(err)) end
  _G.__dbg.stack = body.stackFrames
end)
s:request("scopes", { frameId = frame.id }, function(err, body)
  if err then error(vim.inspect(err)) end
  _G.__dbg.scopes = body.scopes
end)
-- Request variables for the intended local/closure scope from the scopes response:
-- s:request("variables", { variablesReference = scope.variablesReference }, callback)
```

Scope names are adapter-specific: js-debug returned `Local: lineAmount`, not `Local`.
Keep only relevant locals and frame locations in evidence; never dump credentials or the entire
global scope. Async caller frames and `exceptionInfo` can show where a caught failure originated.

`dap.set_breakpoint("index === 1")` sets a conditional JS breakpoint.
`dap.set_breakpoint(nil, nil, "item={index} amount={amount}")` sets a logpoint without pausing.
Use only adapter-supported exception filters. Start with `uncaught` for Node; temporarily
selecting `all` catches handled throws too and can stop frequently in third-party code.
Save and restore the previous settings instead of changing defaults permanently.

### Verify the real debug console

Open `require("dap.repl").open()` and execute through `require("dap.repl").execute(expression)`;
inspect the `dap-repl` buffer to confirm the result actually appeared. A successful DAP watch
request alone does not verify the console UI. Evaluation can execute code: start with read-only
expressions and avoid application calls that send requests, mutate data or expose secrets.
The installed nvim-dap has no `dap.repl.get_buf()`; find its loaded buffer instead:

```lua
for _, buf in ipairs(vim.api.nvim_list_bufs()) do
  if vim.api.nvim_buf_is_loaded(buf) and vim.bo[buf].filetype == "dap-repl" then
    print(vim.inspect(vim.api.nvim_buf_get_lines(buf, 0, -1, false)))
  end
end
```

- **Node/js-debug:** console entries are JS expressions; `input + result` evaluated normally.
- **CodeLLDB:** console is command mode. Use `? result` or `expression result`; bare `result`
  is interpreted as an LLDB command. DAP watch evaluation still uses `context = "watch"`.
- **Program output:** Node `console = "internalConsole"` routes output into the debug console.
  CodeLLDB `terminal = "console"` does the same; otherwise output may be in an integrated
  terminal. Missing console output is not necessarily a failed program.

In the current user config, `;b` toggles a breakpoint, `;B` prompts for a condition, `;L`
creates a logpoint, `;d` continues, and `;o`/`;i`/`;u` step over/into/out.
`<leader>dw` adds a watch and `<leader>de` evaluates. `;e` selects exception filters.
`<leader>nd` debugs the nearest JS/TS test. These are configuration-specific, not nvim-dap
defaults; check the current keymaps before sending keys or starting a runner.

### Attach without killing the service

Prefer launch for disposable reproductions. Attach only after identifying the intended process
and its source tree; enable an inspector or signal a live process only with permission.
For Node, use the configured `pwa-node` adapter with `request = "attach"`, loopback `address`,
the confirmed inspector `port`, and the correct `cwd`/source mappings.
Detach with `dap.disconnect({ terminateDebuggee = false })`, not `dap.terminate()`.
Verify the service remains responsive after detach. Remove only probe-owned breakpoints/listeners
and temporary files; leave the person's editor running or parked.

### Verified behavior and limits

Disposable probes on 2026-10-13 exercised installed nvim-dap, js-debug and CodeLLDB:
Node and Rust breakpoints, step-over, watches changing from 42 to 43, actual REPL results, and
program output. A Node async-failure probe verified conditional breakpoints, logpoints,
stack/scopes/locals, console evaluation and caught-exception inspection. An attach probe
verified the owned Node process stayed alive and produced more output after detach.
The Node breakpoint/watch/console also ran in the visible editor with the full user config;
repeatable complex probes used isolated headless editors and those same installed adapters.
These are not evidence that browser, TypeScript source maps, Lua, or remote debugging work in
every project. Revalidate the actual launch configuration and source mappings before claiming so.

### Dependency preflight is a separate requirement

Check the active adapter and its runtime directly before running a probe. In the user's nvim
checkout, `lua/config/deps.lua` declares dependencies and `install-deps.sh --yes` installs them.
Do not call a partial check proof that everything is installed: its `--check` mode can report
"All required dependencies present" while optional tools or Python venv support are missing.
Successful DAP tests also do not prove all language servers, parsers or formatters are installed.

The repeated Mason `clang-format` failure here was traced to system Python lacking `ensurepip`;
`python3 -c 'import ensurepip'` reproduced it. The existing installer provisions `python3-venv`
on apt systems. Run a privileged install only in a visible terminal with authentication in that
same terminal; never request a password in chat. Afterwards check the installer result, Python
venv creation, the actual formatter executable/Mason receipt, and configured plugin/parser/tool
status. An authentication failure before the installer starts is not a dependency install.

## What it is not

- **A diagnostic list is not a build.** It is one language server's opinion about the files it has
  loaded. Clean diagnostics are not proof the project compiles — run the build before claiming it
  does.
- **LSP has to have attached, and `wait_for_lsp_ready` is not the whole wait.** It returns when a
  client is attached, which is earlier than the first diagnostics: rust-analyzer only publishes
  after its check run finishes, so the call right behind it comes back `[]`. That empty list means
  "not yet", not "no problems" — poll for a few seconds before believing it, and never report a
  file as clean on the strength of one immediate empty read.
- **You are in someone's editor.** `lsp_apply_edit`, `lsp_rename`, `lsp_formatting` and `exec_lua`
  write into a live session a person may be watching, and unsaved buffers do not match the files on
  disk that your other tools read. Prefer reading through the editor and writing through the normal
  file tools; when you do write through it, say so.
- **Not a file reader.** For plain content, the ordinary read tools are simpler and cheaper.
- **Some tools fail for reasons that are not your fault.** `lsp_type_hierarchy_*` returns
  *"unknown request"* against rust-analyzer, which does not implement it. `lsp_range_formatting`
  refuses on stable Rust — rustfmt range formatting is nightly-only. Both are server limits; report
  them as such instead of retrying with different arguments.
- **A call that legitimately returns nothing may come back as an error.** On nvim-mcp at f564bbd,
  a server answering "no result" (hover on a blank line, a document already formatted) surfaces as
  *"Failed to parse … result: expected value at line 1 column 1"*, because the empty Lua table
  encodes as `[]` where the Rust side wants a tagged object. Read that message as "nothing here",
  not as a broken tool.

## Wiring

The server is registered per machine by `install.sh` (user scope, Claude Code + both Copilots), and
can also be wired per repo with `bn setup mcp init <claude|copilot> --with-extras` or `[mcp] extras` in
config.toml. It discovers the nvim for the current git root via `--connect auto`, so no socket path
is configured anywhere. The binary and the nvim plugin must come from the same commit — upstream's
lazy.nvim spec builds it from the plugin checkout (`build = "cargo install --path ."`), which is
the setup that cannot drift.
