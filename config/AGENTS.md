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
- Live agents are listed by `bn agent live` (this machine) and `bn agent live --mesh` (every machine); their `target` is `repo:branch`. Ask one for something with `bn agent msg <repo:branch> "..."`; it lands as that agent's next prompt.
- A prompt starting `[bn msg from <repo:branch>]` is another agent asking you. Do what it asks, then answer with the same command, addressed to that sender. Keep both directions to one message each.

## Across machines
- Start with `bn machine tunnels`: which machines are up, their nicknames, and why one is down. Reach one with `ssh <nickname>`. A box you can already ssh to (a VPS) joins with `bn machine join <name> <user@host>`; `bn machine mesh` shows those. The `multi-device` skill has the rest.
- Long work on another machine (build, install, clone): write it as a script there and run it in a tmux window, never on a bare ssh session. Start every machine first, then wait once.
- A machine without git credentials: push to it over ssh (`git push ssh://<machine><repo> <ref>:refs/remotes/origin/<ref>`), then fast-forward there. Pulling there hangs on a login prompt.
- Settings for one machine go in `~/.config/bn/`, never `~/.gitconfig`: dotfiles sync that file to every machine.

## Reach for these skills
- A reusable workflow becomes a bn skill (`bn skills new <name>`), not a folder under one harness: bn installs it for Claude Code and Copilot CLI on every machine.
- Steps you script again and again become a bn command: check `bn cmd` (MCP `bn_cmd`) first, make one with `bn cmd --new <name>`. `bn cmd window -- <cmd>` opens a background tmux window running `<cmd>`.
- PR reviews in Codespaces use `codespace-pr-review`: read-only PRs, private self-chat reports, application/E2E evidence, and one window named `review`.
- `grill-me` before committing to a plan with open branches.
- `verification-before-completion` before saying anything is done, fixed, or passing.
- `resolving-merge-conflicts` for an in-progress merge or rebase conflict.
- `handoff` or `stow` before a reset, compact, or hand-over.
- `playwright-cli` to drive a browser; `worktree-sweep` for stale worktrees and tmux.
- Before any task, check the available skills and invoke the one that matches. Do not improvise a workflow a skill already covers. Unsure which? `bn skills find "<task>"` and use the top hit.
- One job, one skill: prefer the owner skill over a near-duplicate. If two overlap, say so in one line and merge them.
- Close the loop: when a skill's steps were wrong or incomplete, fix its source in `config/prompts/` as part of the task, not later.
- Retire what is unused: a skill with no use in 90 days and no bn caller is a removal candidate. List it, never delete without asking.
- Every skill description is "what it does. Use when ...". Write new ones that way.

## Code-change handoff
- Before reporting completed code edits, the delivering agent opens the non-empty tracked diff from the merge base with main in the floating nvim used by tmux `prefix e`: `sh ~/.config/bn/repo/config/workflow/bin/tmux-git-diff.sh "$TMUX_PANE"`. Skip read-only tasks, intermediate delegated workers, empty diffs, and sessions outside tmux.
- For changes made by a Codespace agent, the local coordinator opens the remote diff after that agent finishes: `sh ~/.config/bn/repo/config/workflow/bin/tmux-codespace-diff.sh "$TMUX_PANE"`. It prompts for the SSH alias and remote repo path; run it from the local tmux pane, not inside the Codespace.

## Teams messages
- Write for Teams, not Markdown docs: a one-line lead with the outcome, then at most 3 short bullets. No headers, no tables, no code fences unless quoting a command.
- Put the ask or the link on its own line at the end. Use full URLs; PRs as `repo#123` plus the link.
- Plain, human tone in my voice. No em-dashes, no filler openers, no emoji unless I use them.
- Tell what changed and what I need from the reader. Name them only when I want them to act.
- Send to me first with `SendMessageToSelf`; I forward it. Call `GetRichMessageFormats` before using HTML, mentions or cards.

## Git
- Before editing a feature branch, run `git fetch origin` and rebase onto the latest `origin/main`. If the worktree is dirty, the branch is shared or protected, or fetch/rebase conflicts, stop and ask before proceeding.
- Commit each logical unit with a conventional message (`feat:`, `fix:`, `chore:`).
- Never bare `git stash` / `git stash pop`; the stash is shared across worktrees.
- Never force-push, reset --hard, or delete branches without being asked.
