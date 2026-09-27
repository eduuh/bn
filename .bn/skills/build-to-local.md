---
name: build-to-local
description: Build the bn checkout you are standing in and link it in as the live `bn`, either as a branch build (build-to-local) or as the production build (publish-prod, from any checkout, not only main). Builds are files under ~/.local/share/bn/builds; everything the machine runs is a link to one of them.
when: You changed bn source on a branch and want to run the real binary (bn, bn-web, bn-mcp) instead of reasoning about it, a test needs the branch's `bn` on PATH, or you want to promote a checkout to the production build you run day to day.
keywords: [build-to-local, publish-prod, build bn, install bn, test branch, bn build, cargo target, pool slot, local install, prod build, link build, switch bn, bn-web ui, from source]
---
The unit tests prove the crates; only the installed binary proves the command. Whenever a
change touches something a user types (`bn <verb>`, the MCP server, the web dashboard), build
this checkout and run it.

## The two commands

```bash
bn build build-to-local             # build THIS checkout into its branch dir, link it in
bn build publish-prod               # build THIS checkout into the prod dir, link it in
BN_SKIP_UI=1 bn build <either>      # Rust-only (dashboard untouched)
```

Both compile the checkout the cwd is in, whatever branch that is, and differ only in which dir
receives the build. Prod is "the build I run day to day" and can be promoted from any checkout,
not only main. Run either from anywhere inside a bn checkout: a `wt` worktree or a `_pool/N`
slot both work. They are committed project-tier jobs in `.bn/jobs/`, so they resolve from a plain
checkout with no bn-notes setup.

## Layout: builds are files, the live bn is a link

| Path | Holds |
|---|---|
| `~/.local/share/bn/builds/prod/` | the production build, written by `publish-prod` from whichever checkout you promote |
| `~/.local/share/bn/builds/branches/<branch>/` | one dir per branch build, written by `build-to-local` |
| `~/.local/bin/{bn,bn-web,bn-mcp}` | links to the active build dir (what PATH finds) |
| `~/.bin/{bn,bn-web,bn-mcp}` | links to `~/.local/bin/*` (stow-managed dir; zshrc helpers call it) |
| `~/.bn/config/bn` | anchor to `<active>/bn`; cron units and MCP clients spawn bn through it |
| `<build dir>/repo/config/` | snapshot of the checkout's `config/` tree, taken at build time |
| `~/.bn/config/repo`, `jobs`, `agents`, `crons.toml`, `~/.tmux.conf` | links into that snapshot (prompts, agents, shipped jobs, cron registry, tmux config) |

Switching what is live is re-pointing those links, and both jobs do it as their last step. A
branch build never overwrites prod, and prod never overwrites a branch build.

The snapshot matters because `install.sh` links those anchors at the checkout it ran from. A
pool slot is a leased dir that gets reassigned to another branch, so an anchor into `_pool/N`
silently starts serving a different branch's prompts, agents and tmux config. With the snapshot
a build is self-contained; the checkout can move on. The cost is that `~/.tmux.conf` is no longer
live-editable from the checkout: edit it there and rerun the job (`BN_SKIP_UI=1` makes that
seconds).

## What both jobs do

1. Scrub every build dir out of PATH. `install.sh` ends by repointing any `bn`/`bn-web` it finds
   in a PATH dir under `$HOME`, and `bn build` puts the running bn's own dir on the job's PATH.
   Without the scrub, installing prod turned the branch build's binaries into links to prod.
2. Point `BN_INSTALL_TARGET_DIR` at the shared `${XDG_CACHE_HOME:-~/.cache}/bn/target`, so
   only the workspace crates rebuild per branch. `./build.sh` on its own defaults to
   `<checkout>/target/bn-install`, one cold dependency build per slot.
3. Set `BN_BIN_DIR` to the receiving build dir and run the repo's own `./build.sh`: bn-web UI
   (npm + vite), `cargo build --release` for bn-cli, bn-web and bn-mcp, then
   `./install.sh --from-build`, which copies the binaries there and rewires tmux, cron and MCP.
4. Copy the checkout's `config/` into `<build dir>/repo/config` (about 1.4 MB).
5. Link the build dir in (table above) and print `bn version`.

Main's *published* release (no compile) is still `./install.sh` with no flags; it installs into
the default bindir rather than the prod dir and anchors at the checkout, so follow it with
`publish-prod` from main if you want the linked layout back.

## Costs and expectations

- First build into an empty shared target dir: about 15 minutes with the `-j4` cap from
  `~/.zshrc`. Later branches reuse the cached deps and take a few minutes; `BN_SKIP_UI=1` on an
  unchanged tree is seconds.
- Run it in a visible tmux pane (see `tmux-run`) and keep working; do not wait inside a tool call.
- It changes the live `bn` for every session on the machine, other agents' lanes included. Say
  so when you run it while other work is in flight.
- `bn version` prints the git sha it was built from. Check it matches your HEAD before trusting
  what you observe.
- The self-update cron runs `publish-prod` from the main worktree whenever main advances (it
  falls back to `install.sh --core` only on a box with no such job). So when main moves, prod
  is rebuilt from main and linked live, and a branch build under test is superseded. If a branch
  build seems to have vanished overnight, that is why: run `build-to-local` again.

## When it fails

- `not inside a git checkout` / `build.sh not found`: you are not in a bn checkout. `cd` in first.
- Cargo hangs on `Blocking waiting for file lock`: another slot is building into the shared
  target dir. Wait; do not kill it. `BN_INSTALL_KILL_CARGO_LOCK_HOLDER=1` is only for a lock
  left by a dead process.
- The UI step fails: the binary still installs, with the "UI not built" placeholder dashboard.
  Fix Node/npm and rerun; Rust behaviour is unaffected.
- `link-build: … is missing`: install.sh did not place all three binaries; read its output
  above the link step. Nothing was re-pointed, so the previous build is still live.
- `which bn` shows `~/.bin/bn`: that is a link chain ending in the active build dir.
  `readlink -f "$(which bn)"` settles which one.
