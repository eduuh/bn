---
name: build-to-local
description: Build the bn checkout you are standing in and link it in as the live `bn`, either as a branch build (build-to-local) or as the production build (publish-prod, from any checkout, not only main). Builds are files under ~/.local/share/bn/builds; the live `bn` is a link to one of them.
when: You changed bn source on a branch and want to run the real binary (bn, bn-mcp) instead of reasoning about it, a test needs the branch's `bn` on PATH, or you want to promote a checkout to the production build you run day to day.
keywords: [build-to-local, publish-prod, build bn, install bn, test branch, bn build, cargo target, pool slot, local install, prod build, link build, switch bn, from source]
---
The unit tests prove the crates; only the installed binary proves the command. Whenever a
change touches something a user types (`bn <verb>`, the MCP server), build this checkout and run it.

## The two commands

```bash
bn build build-to-local             # build THIS checkout into its branch dir, link it in
bn build publish-prod               # build THIS checkout into the prod dir, link it in
```

Both compile the checkout the cwd is in, whatever branch that is, and differ only in which dir
receives the build. Run either from anywhere inside a bn checkout (a `wt` worktree or a `_pool/N`
slot). They are committed project-tier jobs in `.bn/jobs/`, so they need no bn-notes setup.

## What both jobs do

1. Set `BN_BIN_DIR` to the receiving dir: `~/.local/share/bn/builds/prod/` or
   `~/.local/share/bn/builds/branches/<branch>/`.
2. Set `BN_INSTALL_TARGET_DIR` to the shared `${XDG_CACHE_HOME:-~/.cache}/bn/target`, so only the
   workspace crates rebuild per branch (`./build.sh` alone uses `<checkout>/target/bn-install`).
3. Run `./build.sh`: `cargo build --profile local -p bn-cli -p bn-mcp`, then
   `./install.sh --from-build`, which copies both binaries into `BN_BIN_DIR` and:
   - points the `~/.bn/config/bn` anchor at `<build dir>/bn` (cron units and the tmux bar call it);
   - repoints every existing `bn`/`bn-mcp` in a PATH dir under `$HOME` (and `~/.bin`) at the
     build dir, so `which bn` resolves to it;
   - anchors `~/.bn/config/{repo,jobs,agents,crons.toml}` and `~/.tmux.conf` at the repo's **main
     worktree**, not this checkout. A branch build changes the binaries only; prompts, agents and
     shipped jobs still come from main.

A branch build never overwrites prod, and prod never overwrites a branch build. Switching back is
rerunning the other job.

## Costs and expectations

- First build into an empty shared target dir: about 15 minutes with the `-j4` cap from
  `~/.zshrc`. Later branches reuse the cached deps and take a few minutes; an unchanged tree is seconds.
- Run it in a visible tmux pane (see `tmux-run`) and keep working; do not wait inside a tool call.
- It changes the live `bn` for every session on the machine, other agents' lanes included. Say
  so when you run it while other work is in flight.
- Done when `bn version` prints the git sha of your HEAD (`git rev-parse --short HEAD`). Check it
  before trusting what you observe.
- The self-update cron runs `install.sh --core` from the main worktree when main advances, which
  installs main's published release if it is newer than the installed `bn`. If a branch build
  seems to have vanished, that is why: run `build-to-local` again.

## When it fails

- `bn build: not in a git repo` or `no 'build-to-local' script`: you are not in a bn checkout. `cd` in first.
- Cargo hangs on `Blocking waiting for file lock`: another slot is building into the shared
  target dir. Wait; do not kill it. `BN_INSTALL_KILL_CARGO_LOCK_HOLDER=1` is only for a lock
  left by a dead process.
- `error: <target>/bn is missing`: the cargo build failed; read its output above. Nothing was
  copied, so the previous build is still live.
- `which bn` shows `~/.bin/bn`: that is a link chain ending in the active build dir.
  `readlink -f "$(which bn)"` settles which one.
