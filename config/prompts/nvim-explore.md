---
name: nvim-explore
description: Ask the worktree's editor instead of re-deriving it — LSP diagnostics, definitions, references, hover, rename, over MCP
when: You need a semantic fact about the code — does this compile, where is this defined, who calls it, what type is this — and grep or a full build is the slow way to get it. Needs the `nvim` MCP server (nvim-mcp) and an nvim open on this worktree.
keywords: [nvim, editor, lsp, diagnostics, definition, references, hover, rename, code actions, symbols, nvim-mcp, where is this defined, who calls this]
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

## Wiring

The server is registered per machine by `install.sh` (user scope, Claude Code + both Copilots), and
can also be wired per repo with `bn mcp init <claude|copilot> --with-extras` or `[mcp] extras` in
config.toml. It discovers the nvim for the current git root via `--connect auto`, so no socket path
is configured anywhere. The binary and the nvim plugin must come from the same commit — upstream's
lazy.nvim spec builds it from the plugin checkout (`build = "cargo install --path ."`), which is
the setup that cannot drift.
