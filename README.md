# bn

Branch notes for people who work across many git worktrees, often alongside AI coding agents.

Every branch gets a plain-Markdown note: its goal, todos, blockers, decisions and progress. bn
keeps those notes current from the command line, serves them to Claude Code and Copilot CLI over
MCP, and grows outward from there into worktree management, a cross-repo "what next" queue, cron
jobs and tmux integration.

## What it does

- **Branch notes.** `bn add todo "..."`, `bn done t1`, `bn goal "..."`, `bn cat`. Notes are files
  in a git repo of your own (`~/projects/branch-notes` by default), synced across machines.
- **Worktrees.** `bn wt` manages a bare-clone + worktree layout, with a warm pool of pre-built
  worktrees for large repos.
- **What next.** `bn next` ranks failing crons, attention items, PRs awaiting review and today's
  goals into one list; `bn dispatch` turns an item into a worktree with its note seeded.
- **Agents.** `bn mcp serve` gives an agent tools scoped to the branch it is working in. Skills in
  `config/prompts/` are served as MCP prompts and installed as native Claude/Copilot skills.
- **Jobs and crons.** `bn run <job>` runs shell scripts resolved branch → repo → global, and
  `bn cron` schedules and watches them.
- **tmux.** Status-bar segments, a project picker (`bn tat`) and a jump list (`bn jump`).

`bn --help` lists everything; every command takes `--json`.

## Install

From a release:

```sh
curl -fsSL https://raw.githubusercontent.com/eduuh/bn/main/install.sh | sh
```

From source (Rust toolchain pinned in `rust-toolchain.toml`):

```sh
git clone https://github.com/eduuh/bn && cd bn
./build.sh
```

bn runs on macOS, Linux and WSL. It needs `git`; `gh`, `tmux` and `fzf` unlock the features built
on them.

## Configuration

bn needs no config file. See [docs/config.md](docs/config.md) for the settings and where they
live, and [docs/releases.md](docs/releases.md) for how releases are cut.

## Layout

| Path | What |
|---|---|
| `crates/bn-cli` | The `bn` binary |
| `crates/bn-core` | Notes, resolver, jobs, skills: everything the CLI and MCP server share |
| `crates/bn-mcp` | The stdio MCP server |
| `crates/bn-e2e` | Hermetic end-to-end tests |
| `config/` | Shipped skills, agents, jobs, tmux config and dotfile packages |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE)
