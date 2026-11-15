# Workspan

Move work between projects, agents, and machines.

Workspan is a local development execution layer: manage Git worktrees, launch and steer coding
agents, supervise commands, and move between projects and machines without rebuilding your
workspace every time. Entire captures agent sessions and checkpoints; Workspan handles execution.

## What it does

- **Workspaces.** `workspan wt` manages bare-clone worktrees and warm pools; `workspan tat` opens
  the project/worktree picker.
- **Agents.** `workspan agent launch`, `workspan agent live`, and `workspan agent msg` launch,
  observe, and steer agents. Shared skills and MCP integration configure their tools.
- **Execution.** `workspan build`, `workspan verify`, and scheduled jobs run project commands
  with logs and verification evidence.
- **Machines.** `workspan machine` manages machine access and tunnel discovery.
- **Attention.** `workspan next` gathers actionable work from existing integrations.
- **Session history.** Project setup requires Entire capture with an explicitly approved private
  checkpoint destination. Workspan does not replace Entire or move your code hosting.
- **Dotfiles.** Shipped tmux, Claude, Copilot, and Git configuration live in `config/`.

`workspan --help` lists the command surface. Commands support `--json`.

## Install

From a release:

```sh
curl -fsSL https://raw.githubusercontent.com/eduuh/workspan/main/install.sh | bash
```

From source (Rust toolchain pinned in `rust-toolchain.toml`):

```sh
bn wt clone https://github.com/eduuh/workspan
# In the checkout reported by the worktree manager:
./build.sh
```

Workspan runs on macOS, Linux and WSL. It needs `git`; `gh`, `tmux` and `fzf` unlock integrations.
Project onboarding also needs the [Entire CLI](https://docs.entire.com/installation).

## Project setup

Use the `setup-bn` skill (its compatibility name is retained) to configure agent tools and the
repository's build/verification commands. It includes required Entire setup.
To configure capture directly, save an approved `github:owner/private-checkpoints` destination in
`~/.config/bn/entire-checkpoint-remote`, then run from the project:

```sh
workspan-setup-entire copilot
# Or: workspan-setup-entire claude
```

The helper verifies the destination is private before enabling capture, writes local settings,
and preserves existing hooks through Entire's setup command. It never imports history or pushes
transcripts during setup. Missing prerequisites and failed privacy checks block onboarding.
See [the setup reference](config/reference/entire-setup.md).

## Compatibility

Workspan was previously named `bn`. The installer provides `workspan` as the primary entry point
while keeping `bn` and existing command behavior available. Set `WORKSPAN_INSTALL_WP=1` during
installation to request an optional `wp` alias; an existing `wp` command is never replaced.

This is an incremental identity migration, not a storage migration. Existing `BN_*` environment
variables, `~/.config/bn`, `.bn` project files, worktree paths, internal crate names, and release
binary names remain compatible. Legacy note functionality is retained while its dependencies
are phased out; it is not Workspan's product focus. Existing users do not need to move their data.

Configuration: [docs/config.md](docs/config.md). Releases: [docs/releases.md](docs/releases.md).
The name overlaps an existing WorkSpan software brand; this project is independent of that company.

## Layout

| Path | What |
|---|---|
| `crates/bn-cli` | CLI and compatibility binary |
| `crates/bn-core` | Shared execution, workspace, integration, and legacy storage logic |
| `crates/bn-mcp` | Stdio MCP server |
| `crates/bn-e2e` | Hermetic end-to-end tests |
| `config/` | Shared skills, agents, jobs, tmux configuration and stow packages |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE)
