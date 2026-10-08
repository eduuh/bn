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
