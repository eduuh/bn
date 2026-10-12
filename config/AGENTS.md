# Agent instructions (global, harness-neutral)

## Be lean

- Answer first, then only the detail the reader needs. No preamble, no recap of what you were asked.
- Prefer one targeted tool call over a sweep. Read the part of a file you need, not the whole file.
- Use Copilot fleet or Claude subagents for substantial research. Keep short lookups local, start with the smallest useful team, and expand only for independent work. Return concise findings with sources.
- Do only what was asked. Mention adjacent problems if needed use subagent to investigate this never the main agent.
- Keep one owner for integration. Workers must not launch further agents without that owner's approval.
- Run long batch commands through `bn cmd run -- <command...>` for heartbeats, logs and inactivity warnings. Start them in the background and wait once on their completion signal; do not poll from the agent.

## Track work with bn

- Run `bn discover` at session start and when changing tasks. Pass the task and invoke the matching workflow instead of improvising.
- Record progress as it happens. Save durable knowledge promptly.
- Keep links curated, not comprehensive. Before adding one, search `bn links list <purpose> --tag <category>` and add only an important, repeatedly useful link that fills a real gap. Use `bn links add <url> --category <category> --name "<what it is>"`; routine PRs, work items, review comments and one-off URLs do not belong.

## Talk to other agents

- Live agents are listed by `bn agent live` (this machine) and `bn agent live --mesh` (every machine); their `target` is `repo:branch`. Ask one for something with `bn agent msg <repo:branch> "..."`; it lands as that agent's next prompt.
- A prompt starting `[bn msg from <repo:branch>]` is another agent asking you. Do what it asks, then answer with the same command, addressed to that sender. Keep both directions to one message each.

## Across machines

- Start with `bn machine tunnels`: which machines are up, their nicknames, and why one is down. Reach one with `ssh <nickname>`. A box you can already ssh to (a VPS) joins with `bn machine join <name> <user@host>`; `bn machine mesh` shows those. The `multi-device` skill has the rest.
- Long work on another machine (build, install, clone): write it as a script there and run it in a tmux window, never on a bare ssh session. Start every machine first, then wait once.
- A machine without git credentials: push to it over ssh (`git push ssh://<machine><repo> <ref>:refs/remotes/origin/<ref>`), then fast-forward there. Pulling there hangs on a login prompt.
- Settings for one machine go in `~/.config/bn/`, never `~/.gitconfig`: dotfiles sync that file to every machine.

## Skills

- Keep reusable workflows, shared config, and automation in bn. Stow home dotfiles from `config/stow/`; keep repo-only files such as `.github/workflows/ci.yml` in the source checkout.
- Edit skills at their bn source, never generated copies. Run `bn skills install` to overwrite stale Copilot and Claude copies; set `BN_MCP_SKILLS_DIR="$PWD/config/prompts"` for shipped worktree edits.
- Retire what is unused: a skill with no use in 90 days and no bn caller is a removal candidate. List it, never delete without asking.
- Every skill description is "what it does. Use when ...". Write new ones that way.

## Code-change handoff

- Before reporting completed code edits, the delivering agent first commits, runs `git fetch origin main` and rebases the branch onto `origin/main` (stop and ask on conflicts or a dirty shared branch), then opens the diff in the floating nvim used by tmux `prefix e` (`DiffviewOpen origin/main`): `sh ~/.config/bn/repo/config/workflow/bin/tmux-git-diff.sh "$TMUX_PANE"`. Skip read-only tasks, intermediate delegated workers, empty diffs, and sessions outside tmux.
- For changes made by a Codespace agent, the local coordinator opens the remote diff after that agent finishes: `sh ~/.config/bn/repo/config/workflow/bin/tmux-codespace-diff.sh "$TMUX_PANE"`. It fuzzy-picks the SSH host (cs-* first) and asks for the remote repo path; run it from the local tmux pane, not inside the Codespace.

## Teams messages

- Write for Teams, not Markdown docs: a one-line lead with the outcome, then at most 3 short bullets. No headers, no tables, no code fences unless quoting a command.
- Put the ask or the link on its own line at the end. Use full URLs; PRs as `repo#123` plus the link.
- Use `write-like-me` for the wording and voice of Teams messages.
- Tell what changed and what I need from the reader. Name them only when I want them to act.
- Send to me first with `SendMessageToSelf`; I forward it. Call `GetRichMessageFormats` before using HTML, mentions or cards.

## Git

- Before editing a feature branch, run `git fetch origin` and rebase onto the latest `origin/main`. If the worktree is dirty, the branch is shared or protected, or fetch/rebase conflicts, stop and ask before proceeding.
- Commit each logical unit with a conventional message (`feat:`, `fix:`, `chore:`).
- Never bare `git stash` / `git stash pop`; the stash is shared across worktrees.
- Never force-push, reset --hard, or delete branches without being asked.
- Clean up stale worktrees in the current repo at session start and after a PR merges: `bn wt reap --all --yes`, then `bn wt pool prune --yes` to release pool leases nobody is in. Stale means what bn can prove is finished: a merged PR with a clean tree, or a finished scout with nothing uncommitted, or a leased pool slot with no live agent and a clean tree. Anything else stays. Never add `--force`, and never sweep other repos (`--all-repos`) without being asked.
