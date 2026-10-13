# Commands

Your own commands: the thing you do over and over, written once as `<name>.sh` and run with
arguments by you or by an agent.

    bn cmd                       # list them, with each one's `# description:` line
    bn cmd <name> [args...]      # run one in the current directory
    bn cmd <name> --which        # print its path
    bn cmd --pick                # fzf picker with a preview of each script (tmux prefix c)
    bn cmd --new <name>          # scaffold one (--scope global|repo|project|branch)

Agents get the same thing as one MCP tool, `bn_cmd`: no name lists, a name runs it and returns
the exit code, stdout and stderr (120s timeout by default).

## Where a command lives

Like jobs, highest precedence first: the branch note's `cmds/`, the project's committed
`.bn/cmds/`, the notes repo's `cmds/<repo>/`, then global: your work and personal notes'
`cmds/` (`--new` writes to personal) and this directory, which ships with bn.

## Writing one

Plain `sh`. The leading comments describe it:

    # description: One line, shown by `bn cmd` and to agents
    # usage: name [-f] [args...]

It runs in the caller's directory with bn's job PATH, plus `$BN_CMD_REPO` and `$BN_CMD_BRANCH`
(the worktree it was called from, empty outside one) and `$BN_CMD_LIB` (the shared shell helpers
in `config/jobs/lib`). Unlike a job, a run is not recorded to the cron ledger.

## Shipped

- `window`: open a tmux window in the repo's session, optionally running a command in it.
- `run`: stream a foreground batch command, save a private log, and report its status.
- `runs`: inspect this worktree's recorded runs, read retained logs, or rerun a command.

## Long batch commands

    bn cmd run -- cargo clippy --workspace
    bn cmd run -- --label tests -- cargo test --workspace

`run` reports its start, elapsed time every 60 seconds, and final exit status. After 180 seconds
without output it warns that progress is unknown, not that the process is stuck. `--label` names
the stage; otherwise it shows the executable. Use `--interval` and `--idle-after` to change the
timing. Status messages go to stderr; the command's stdout, stderr, stdin and exit status are
preserved. Requires Python 3.

Logs contain only command output, have mode `0600`, and default to
`${XDG_CACHE_HOME:-~/.cache}/bn/commands/`. `--log <path>` selects a new file; an existing log is
never overwritten. Interrupts and termination signals are forwarded to the command's process
group. Use this for foreground batch jobs, not interactive editors or commands that daemonize.

Run it in the visible tmux pane for a long job, then wait on that job's completion signal.
The runner emits heartbeats; the agent does not poll. Check with
`python3 scripts/test-command-heartbeat.py` and `python3 scripts/test-command-runs.py`.

## Build/test results in tmux

Wrap a build or test with `run` to record its real result:

    bn cmd run -- --label build -- bn build
    bn cmd run -- --label tests -- cargo test -p bn-core
    bn cmd runs                         # list this worktree's runs, newest first
    bn cmd runs -- --pick                # inspect logs or confirm a rerun (tmux prefix B)
    bn cmd runs -- --json                # structured results
    bn cmd runs -- --logs <id>           # original stdout/stderr, without runner messages
    bn cmd runs -- --rerun <id>          # explicit rerun of original argv and working directory

The tmux bar shows the selected worktree's label, result, and running duration, for example
`build RUN 42s`, `tests FAIL 37`, or `build PASS`. Every window tab has a short result badge.
Other labels' retained failures appear as `!N`, unknown outcomes as `?N`, and additional active
runs as `+N`. Active runs take priority; a failed result remains visible until a new run with
the same label supersedes it. The popup lists full history, including superseded results.
No result appears in a worktree with no recorded runs; agent badges are separate.

`PASS` means exit zero, `FAIL` means nonzero, and `CANCEL` means the runner received an interrupt
or termination signal, or the command exited from a signal. A signal-handling command may
return zero while still being marked cancelled. Quiet output never implies completion.
The runner holds a file lock for its lifetime; if it disappears before recording an outcome,
the view says `UNKNOWN`, never success. Metadata errors are reported explicitly without
changing the wrapped command's exit code; a broken record displays `RUNS ERROR`.

Records and lock files are local under
`${XDG_CACHE_HOME:-~/.cache}/bn/commands/runs/<worktree-path-hash>/`, in private `0700`
directories with `0600` files. Publication is atomic, and concurrent runs have distinct IDs
and logs. Git subdirectories resolve to the worktree root; separate worktrees remain separate.
Outside Git, the current directory is the workspace. History is retained until you remove its
cache directory; do not remove records/locks for active runs. Treat this cache as sensitive:
records contain command arguments and logs may contain secrets. Pass secrets through the
environment rather than argv. The environment itself is not recorded.

Reruns use the original argument array, directory, label, and heartbeat settings, with **new**
records/logs and the **current** environment. They do not invoke an extra shell or overwrite
earlier output, and live runs cannot be rerun. The popup confirms before executing. `UNKNOWN`
may mean an orphaned child still exists: inspect it before rerunning. This is foreground batch
tracking, not daemon supervision. Result staleness after source edits is not yet tracked:
`PASS` is historical evidence, not a claim that the current source has been verified.
