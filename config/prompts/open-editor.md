---
name: open-editor
description: Hand the finished work back to a human — open this worktree's nvim on the branch's diff
when: Work on this branch is done (or paused at something reviewable) and a person should look at the diff. The last step of a session, after the commits exist. Not for mid-work.
keywords: [open editor, open nvim, review the diff, hand off, work is done, show me the diff, land, finished]
---
You are handing finished work back to a human. The point is that they arrive looking at **what
changed**, not at an empty editor they then have to drive.

1. Make sure there is something to look at. Run `bn diff --three-dot` in the worktree. If it reports
   no changes vs the base, say so and stop — opening an editor on an empty diff wastes the gesture.
   Commit outstanding work first if that is what is missing.

   `--three-dot` is not optional garnish, and **never drop it to get the command to run**. Bare
   `bn diff` is two-dot, which reports the *base's* own newer commits as if they were yours — on a
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

## When this does not apply

- **Not in tmux** — `bn edit` needs a window to put the pane in. Say so and summarize the diff in
  chat instead; do not pretend an editor opened.
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
