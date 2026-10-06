# Agent instructions (global, harness-neutral)

Shipped with bn. Claude Code reads this through an `@` import in `~/.claude/CLAUDE.md`; Copilot CLI reads it as `~/.copilot/copilot-instructions.md`. Keep it short: every line here is loaded into every session.

## Be lean
- Answer first, then only the detail the reader needs. No preamble, no recap of what you were asked.
- Short sentences. No em-dashes. No headers in short replies.
- Do not paste long command output into chat. Summarise it and name the file or command.
- Do only what was asked. Mention adjacent problems in one line instead of fixing them.
- Prefer one targeted tool call over a sweep. Read the part of a file you need, not the whole file.
- Waiting on something slow: start it in the background and wait once on its exact done-marker. No sleep-and-check loops.

## Track work with bn
- Read the branch note at session start (`bn --cat`). Set the goal if it is empty: `bn goal "..."`.
- Record as you go, never batched: `bn add todo|decision|blocker|research|ask "..."`, and `bn done <id>` the moment a todo is finished.
- Repo knowledge that will still be true next month goes in the reference notes: `bn ref --repo` to read the index, `bn ref append <name> --repo --section <s> "<fact> (src, date)"` to add one fact.
- Save any URL worth going back to (PR, work item, portal blade, dashboard, docs page) the moment it surfaces, without being asked: `bn links add <url> --name "..." --tags <t>`. Closing the note saves its PR and work item for you.
- Need a URL (board, dashboard, docs page, PR)? Check `bn links list <words>` before searching the web or asking the user.
- Session hooks save the PRs and work items you mention, marked new. Reviewing them (`bn links new`) is the user's call, not yours. A link only needed for a while gets `--ttl 7d`.
- Find a workflow before improvising: `bn skills find "<what you are about to do>"`, then invoke it (`/bn:<name>` in Claude Code, `/<name>` in Copilot CLI).

## Talk to other agents
- Other live agents on this machine are listed by `bn agent live` (their `target` is `repo:branch`). Ask one for something with `bn agent msg <repo:branch> "..."`; it lands as that agent's next prompt.
- A prompt starting `[bn msg from <repo:branch>]` is another agent asking you. Do what it asks, then answer with the same command, addressed to that sender. Keep both directions to one message each.
- Work another agent must *do* is a queue row, not a message: `bn queue create <repo:branch> "..."`. A `[bn queue qN …]` prompt means a row is waiting for you: `bn queue claim qN`, then close it with a note saying where the work went (`bn queue done qN --note …`) or `bn queue handoff`. Never park blocked work silently: `bn queue block qN --on … --wake-after 1h`.

## Across machines
- Start with `bn tunnels`: which machines are up, their nicknames, and why one is down. Reach one with `ssh <nickname>`. The `multi-device` skill has the rest.
- Long work on another machine (build, install, clone): write it as a script there and run it in a tmux window, never on a bare ssh session. Start every machine first, then wait once.
- A machine without git credentials: push to it over ssh (`git push ssh://<machine><repo> <ref>:refs/remotes/origin/<ref>`), then fast-forward there. Pulling there hangs on a login prompt.
- Settings for one machine go in `~/.config/bn/`, never `~/.gitconfig`: dotfiles sync that file to every machine.

## Reach for these skills
- A reusable workflow becomes a bn skill (`bn skills new <name>`), not a folder under one harness: bn installs it for Claude Code and Copilot CLI on every machine.
- `grill-me` before committing to a plan with open branches.
- `verification-before-completion` before saying anything is done, fixed, or passing.
- `resolving-merge-conflicts` for an in-progress merge or rebase conflict.
- `handoff` or `/bn:stow` before a reset, compact, or hand-over.
- `playwright-cli` to drive a browser; `worktree-sweep` for stale worktrees and tmux.

## Git
- Commit each logical unit with a conventional message (`feat:`, `fix:`, `chore:`).
- Never bare `git stash` / `git stash pop`; the stash is shared across worktrees.
- Never force-push, reset --hard, or delete branches without being asked.
