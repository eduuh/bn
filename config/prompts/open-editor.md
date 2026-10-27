---
name: open-editor
description: "Open local or Codespace nvim on the branch diff for a human. Use when handing finished code back for the user to read."
when: Work on this branch is done (or paused at something reviewable) and a person should look at the diff. The last step of a session, after the commits exist. Not for mid-work.
keywords: [open editor, open nvim, review the diff, hand off, work is done, show me the diff, land, finished, codespace, remote diff, ssh, remote ui]
---
You are handing finished work back to a human. The point is that they arrive looking at **what
changed**, not at an empty editor they then have to drive.

Choose the checkout, not just the terminal: use the local steps below for a local worktree, and
**Codespace mode** for work in a Codespace. Never show a local clone's diff as evidence of remote
changes. If running inside the Codespace with the human attached to its tmux session, the local
steps work there too. Otherwise hand the remote checkout back to a local coordinator; an editor
opened in an unattended remote session is not a visible handoff.

## Local or attached remote tmux

1. Make sure there is something to look at. Run `bn status --diff --three-dot` in the worktree. If it reports
   no changes vs the base, say so and stop — opening an editor on an empty diff wastes the gesture.
   Commit outstanding work first if that is what is missing.

   `--three-dot` is not optional garnish, and **never drop it to get the command to run**. Bare
   `bn status --diff` is two-dot, which reports the *base's* own newer commits as if they were yours — on a
   worktree cut from a stale local main that turns a 30-file branch into a 79-file diff with
   thousands of phantom deletions. The editor itself always diffs three-dot, so a two-dot stat line
   describes something nobody is about to look at. If you quote it, you are describing the wrong
   diff to a human who is about to see a different one.

2. Land the editor:

   ```
   bn edit land
   ```

   That creates this worktree's nvim (or restores it if it was parked), opens Diffview on
   `<base>...HEAD`, and spawns the agent terminal. The base is resolved by bn — `origin/HEAD`, then
   the branch's upstream, then `origin/main`/`main`/`origin/master`/`master` — so the diff is
   against the *remote* default branch, not a stale local one.

   - Add `--term visible` when an agent is still running and its output is part of what they should
     see; the default spawns it hidden so the diff gets the screen.
   - Add `--base <ref>` only when the work is deliberately against something else.

   It is idempotent: landing twice focuses the existing diff rather than stacking a second one.

3. Tell them where it is and what to look at. `bn edit land` does **not** switch their tmux client —
   it deliberately does not yank focus, because this often fires while they are elsewhere. So name
   the window, and give them the way back:

   - `prefix + e` toggles the editor pane in that window (park it and it keeps running).
   - `prefix + backtick` or `bn jump` reaches the window itself.

4. Finish with 2–3 lines: what changed, what you want them to look at first, and anything you are
   unsure about. A diff with no pointer is a diff they have to re-derive.

## Codespace mode (from the local machine)

Neovim can edit individual `scp://` paths when netrw is available, but that does not give local
Diffview, Git or LSP a remote repository. Neovim also supports `--remote-ui`: the UI is local while
the editor and its plugins run remotely. For either remote UI or SSH, keep nvim and Git rooted in
the actual Codespace checkout. Do not copy files locally or mount the repository just to show a diff.

1. Resolve the existing Codespace SSH alias and exact remote worktree path. Set up SSH access first
   if needed (`bn cs connect <codespace>`); reuse the intended account and Codespace, do not create one for
   a handoff. Check the branch, dirty state and three-dot diff **there**, not in the local clone.
   Use `bn status --diff --three-dot` from that remote worktree when available; otherwise use
   `git diff --stat '<base>...HEAD'`. Stop on an empty committed diff; surface any outstanding work
   rather than silently committing someone else's changes.

2. In local tmux, use the existing SSH popup:

   ```sh
   sh ~/.config/bn/repo/config/workflow/bin/tmux-codespace-diff.sh "$TMUX_PANE"
   ```

   The human selects the SSH alias (`cs-*` aliases appear first) and enters the **actual worktree
   path**, which may differ from the suggested `/workspaces/<repo>`. This runs nvim in the
   Codespace over `ssh -tt`, not local nvim on remote filenames. It needs remote nvim with Diffview
   and a reachable SSH alias; it does not require a full bn install remotely.

   The helper currently uses `git merge-base origin/main HEAD` and opens Diffview against the
   working tree. Use it when `origin/main` is the intended base; disclose uncommitted changes if
   they appear. For another base or an exact committed-only view, use the SSH fallback below.
   This popup is not `bn edit land`: it is not an idempotent parked editor, and quitting ends nvim.

3. Without local tmux or the helper, attach a remote tmux session over SSH, then open the diff from
   the confirmed checkout. Substitute the alias, path and base below; quote actual paths:

   ```sh
   ssh -t <alias> 'tmux new-session -A -s review-diff'
   # In the attached remote terminal:
   cd -- /workspaces/REPO
   nvim -c 'DiffviewOpen origin/main...HEAD'
   ```

   Use a dedicated session for this review; do not change another task's session. Remote tmux keeps
   nvim alive across an SSH disconnect. If tmux is unavailable, plain interactive SSH and the same
   nvim command still work, but do not promise persistence. Report missing nvim/Diffview or SSH
   errors; do not claim the diff opened.

   For a **verified bn-managed remote worktree layout**, `bn edit remote <alias>` is another option:
   it forwards nvim's socket over SSH and attaches a local `--remote-ui`. Do not use it blindly for
   `/workspaces/...`: it resolves or creates a bn-managed checkout, not an arbitrary path. After
   attachment, open `:DiffviewOpen <base>...HEAD`; `:detach` leaves the remote editor running.

4. Name the Codespace alias, remote path, branch and base in the handoff, plus the first thing to
   review. For the popup, `:qall` closes it; for remote tmux, `prefix + d` detaches without quitting.
   Do not promise local `prefix + e` will reopen a remote popup or switch the human to a remote
   session automatically.

## When this does not apply

- **Not in tmux** — local `bn edit` needs a window to put the pane in. For a Codespace, use the SSH
  fallback above; otherwise summarize the diff in chat rather than pretending an editor opened.
- **`$EDITOR` is not nvim** — landing needs nvim's `--listen` socket. bn will say so; fall back to
  summarizing.
- **The editor does not answer** — a modal prompt (a plugin error leaving `Press ENTER`) blocks
  nvim's main loop, and bn gives up after 2 seconds with *"editor did not answer"*. Report that
  honestly rather than retrying in a loop; the fix is a keystroke in that pane, not another call.
- **The installed `bn` predates this skill** — `error: unrecognized subcommand 'edit'`, or
  `unexpected argument '--three-dot'`. The skill is not wrong and the flag is not gone: the binary
  on `$PATH` is older than the worktree you are standing in, which is what a stale
  `worktree/bn/main` produces. Check with `bn --version` against the repo's own build. When you are
  inside the bn repo, `./target/debug/bn edit land` uses the code in front of you and works;
  anywhere else, say the install is stale and summarize the diff instead. Do not paper over it by
  dropping flags until something runs — that is how the two-dot trap above gets sprung.
