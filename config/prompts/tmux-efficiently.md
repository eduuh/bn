---
name: tmux-efficiently
description: Use tmux as your working surface — a reusable command pane for visible commands, a disposable job pane per long-running task (never a new window), exit codes from tmux itself rather than echoed markers, and panes reaped once read. Covers launching independent agents in other sessions and reading, steering and stopping them.
when: About to run anything long, interactive, or side-effecting; about to spawn or supervise another agent; or you need to read what an agent in another session is doing.
keywords: [tmux, pane, panes, split, long-running, background, job, cleanup, reap, capture-pane, pipe-pane, remain-on-exit, pane_dead, wait-for, agent, fleet, spawn, steer, send-keys, another session, supervise]
---
You work in tmux. Treat it as structured state you can query and address, not as a terminal you
type into. Everything below is a handle you can hold across turns — that is what makes it
efficient, and what stops you reporting a job's status from the wrong pane.

## The window is not yours to expand

In this setup **a session is a repo and a window is a worktree**. The `` ` `` picker, the tab
badges, `bn tmux-reconcile` and the dashboard all read that mapping. A window opened for a build is
a window that lies about a worktree, so:

> **Never open a window for a long-running command.** Long work goes in an adjacent pane of the
> window you are already in — the one the user is talking to you in.

The sibling `tmux-run` skill decides *whether* a command should be visible at all and keeps one
narrow exception: a job that genuinely outlives the conversation, like a dev server you will still
be using tomorrow, may earn its own named window. Everything below is the other half — the
mechanics of the pane itself, its exit code, and its disposal.

Three pane roles live in that window:

| Role | How many | Lifetime |
|---|---|---|
| **agent pane** | one — you are in it | the session |
| **command pane** (`cmd`) | one, reused | the session |
| **job pane** | one per long-running task | until you read its outcome and reap it |

## Address panes by id, never by index

`%17` names one pane for as long as it lives. Pane *indices* renumber the moment a neighbour
closes, so `.1` silently becomes a different pane between two of your turns — and that is how a
capture ends up quoting someone else's output. Every split ends in `-P -F '#{pane_id}'`, and the
`%id` is what you carry.

Same for windows: `#{window_id}` (`@5`), not a name that a rename can invalidate.

**Anchor every lookup on `$TMUX_PANE`.** A bare `display-message` with no `-t` answers for the
*session's active pane*, which is only you by coincidence — measured here as `@37` in another
session while this agent was in `@77`. Every target derived from that points at somebody else's
window, and the split lands in their work.

```bash
me=${TMUX_PANE:?not inside tmux}
win=$(tmux display-message -p -t "$me" '#{window_id}')
```

## The command pane

One adjacent pane, reused all session, for commands the user should watch: installers, migrations,
package managers, anything that may prompt, and **any script you just wrote**. Find it by its
title, which is a handle you set — not by "the pane that isn't active", which stops being true the
moment a job pane exists.

```bash
cmd=$(tmux list-panes -t "$win" -F '#{pane_id} #{pane_title}' | awk '$2=="cmd" {print $1; exit}')
if [ -z "$cmd" ]; then
  cmd=$(tmux split-window -d -t "$win" -h -l 40% -P -F '#{pane_id}')
  tmux select-pane -t "$cmd" -T cmd
fi
```

**Check it is idle before sending.** A pane mid-command or sitting on a `Password:` prompt will
take your keystrokes as an answer to someone else's question.

```bash
tmux capture-pane -p -t "$cmd" -S -3
```

A shell prompt on the last line means idle. Anything else means busy — open a job pane instead.

```bash
tmux send-keys -t "$cmd" '<command>' Enter
```

Read-only inspection stays in the normal tool call. Routing `git status` or a `grep` through tmux
is noise, not transparency.

## Job panes — the whole lifecycle

A build, a clone, an install, a test suite, a dev server. You do not sit and wait for these.

```
 1. split a job pane ──▶ 2. name it, log it ──▶ 3. tell the user ──▶ 4. keep working
                                                                          │
                             7. reap ◀── 6. read outcome ◀── 5. block on the channel
```

**1–2. Create it, and make it self-reporting.**

```bash
job_log=$SCRATCH/build.log
chan=job-$$-$RANDOM                                       # the channel you will block on
tmux set-option -w -t "$win" remain-on-exit on            # inherited by the next split
job=$(tmux split-window -d -t "$win" -l 30% -P -F '#{pane_id}' \
      "trap 'tmux wait-for -S $chan' EXIT; cargo build --release")
tmux pipe-pane -t "$job" -O "cat >> $job_log"             # first, before output starts
tmux set-option -p -t "$job" remain-on-exit on            # pin it on the pane itself
tmux set-option -w -u -t "$win" remain-on-exit            # hand the window back
tmux select-pane -t "$job" -T build
```

**That order is the recipe, not a style choice.** Both halves are races you can lose:

- `remain-on-exit` goes on the **window first** so the new pane inherits it at birth. A command
  that fails in 50ms is otherwise gone before your pin lands, taking its exit code with it. Pin it
  on the **pane** so the setting survives, then **unset the window** so you have not quietly
  changed how the user's own shell behaves.
- `pipe-pane` captures only from the moment it attaches, so it goes **immediately** after the
  split. Put a `select-pane -T` in front of it and the job's first lines are already on screen and
  missing from the log — measured, not theoretical. The pane's own scrollback still has them
  (`capture-pane -S -200`), so the log is for grepping and the scrollback is the full record.

`-l 30%` keeps the job from crowding the work. `pipe-pane -O` gives you a real file — greppable
with ordinary tools, unbuffered, and it outlives the pane.

**3. Say where it is in one line** — "building in the `build` pane, ~4 min" — and move on. Do not
narrate progress you have not checked.

**4. Keep working.** Anything that does not depend on that job continues now.

**5. Block on the channel, never poll and never wait on text.**

```bash
tmux wait-for "$chan"
```

Run that with `run_in_background: true`. It wakes on the exit itself, so there is no 20s
granularity to lose and no timer burning while it waits. Do not chain sleeps — the harness
refuses them, and now you have nothing to sleep for.

**The `EXIT` trap is what makes the channel trustworthy.** It fires on a normal exit and on the
signals a shell can catch, so an interrupted job still signals. What it cannot survive is the pane
being destroyed outright — `kill-pane`, or SIGKILL — because then no shell runs to signal, and a
bare `wait-for` blocks forever. So a wait you cannot afford to lose keeps the pane check as its
fallback:

```bash
tmux display-message -p -t "$job" '#{pane_dead}'   # 1 = finished, empty = the pane is gone
```

**The empty answer is not decoration.** A pane that is *gone* — `remain-on-exit` never took, or
someone closed it — makes `display-message` print nothing **and exit 0**, so a check written as
`[ "$(…)" = 1 ]` never becomes true. tmux does not error on a missing pane; you have to treat "no
answer" as an ending. When it ends that way there is no status to read, and that is what you
report — not a success you did not see.

**6. Take the exit code from tmux.**

```bash
tmux display-message -p -t "$job" '#{pane_dead_status}'
```

This is the reason to bother with `remain-on-exit`. The old habit — appending
`; echo "--- exited $? ---"` and grepping for it — matches **the command line you typed**, which
tmux echoed into the pane before the job ever started. It reports success on a job that has not
begun. `#{pane_dead_status}` is tmux's own record and matches nothing.

Then read what happened, from the log rather than the last screen: an error that scrolled away
still happened.

```bash
grep -nE 'error|warning: unused|FAILED' "$job_log" | head -20
tmux capture-pane -p -t "$job" -S -200        # dead panes keep their scrollback
```

**7. Reap it.**

```bash
tmux kill-pane -t "$job"
```

## Cleaning up

**Read before you reap.** A killed pane's scrollback is gone; if you piped it, the log survives, so
`pipe-pane` is also your undo.

**Reap what you finished, when you finish it** — not in a batch at the end. A dead pane you have
already reported is clutter taking a third of the window.

**Sweep the window** for panes that died while you were elsewhere:

```bash
tmux list-panes -t "$win" -f '#{pane_dead}' -F '#{pane_id}' \
  | xargs -r -n1 tmux kill-pane -t
```

**Fire-and-forget** work that you will not read unless it breaks can use `remain-on-exit failed`
instead of `on`: a zero exit closes the pane by itself, a failure stays on screen for you.

**Never reap a live pane.** `kill-pane` on a running job kills the job, and `#{pane_dead}` is `0`
for exactly the panes you must leave alone. Never reap the `cmd` pane or a pane hosting an agent
mid-turn — an agent between tool calls has a quiet pane and looks idle.

## Serialising work across worktrees

The pool's worktrees share one git stash stack and one machine. Where two sessions can collide over
something with a single global slot — a stash push/pop above all — take a **server-wide lock**
instead of inventing a lockfile:

```bash
tmux wait-for -L stash        # blocks until whoever holds it releases
git stash push -u -m "$tag"
tmux wait-for -U stash        # release it
```

`-L` locks the channel, `-U` unlocks it, and every client on this tmux server queues on the same
name. Same primitive as the job signal above, used for exclusion instead of completion. The lock
lives in the tmux server, so it protects you against concurrent sessions but evaporates if the
server dies — it is not a crash-safe lock.

## tmux as a query surface

Format strings turn the server into a table. One call answers questions that otherwise cost a
dozen.

```bash
# everything running on this machine, one line per pane
tmux list-panes -a -F '#{session_name}:#{window_name} #{pane_id} #{pane_current_command} dead=#{pane_dead}'

# every dead pane anywhere — the cleanup queue
tmux list-panes -a -f '#{pane_dead}' -F '#{pane_id} #{pane_title}'

# which windows have unseen output
tmux list-windows -a -F '#{session_name}:#{window_name} activity=#{window_activity_flag}'
```

Useful fields: `pane_id window_id pane_title pane_dead pane_dead_status pane_pid
pane_current_command pane_current_path session_name window_name window_activity_flag`.

## Independent agents in other sessions

An agent is a long-running job with opinions. It goes where the model says it goes — **another
repo's session, in that branch's window** — because that placement is the only reason
`bn agent live`, the tab badges, the steer path and the kill path can find it at all. A hand-rolled
`tmux new-session` gets you an agent no tool can see.

**Launch through `bn dispatch`**, which leases a worktree, seeds the branch note with the task, and
opens the window in the right session:

```bash
bn dispatch --repo <repo> --agent claude "<what to do>"     # a ship task
bn dispatch --repo <repo> --agent claude --scout "<what to find out>"   # investigate, no code
```

`bn agent launch` is the same chokepoint when you are already in the worktree. Both record
`agent-session:` on the note, which is what makes resume, steering and the dashboard work.

**Find them.**

```bash
bn agent live --json      # every agent: target, state, health, session_id, idle_secs
bn agent supervise        # the same fleet, flagged for stuck
```

`target` is `repo:branch` — the handle for everything below.

**Read them.**

```bash
bn agent pane <repo:branch> --lines 60     # live tail of its pane
bn agent activity <session_id> --limit 40  # its transcript — works after the window closed
bn agent prompt <repo:branch>              # the dialog it is blocked on, options and meanings
```

Raw tmux works too, but its targeting will hand you the wrong agent if you let it. **`=` anchors
only the part it precedes**, so `=repo:branch` pins the session and leaves the *window* matching by
prefix: with no window named `main`, `=bn:main` cheerfully resolves to `main-stale`. Anchor both
halves:

```bash
tmux capture-pane -p -t '=<repo>:=<branch>' -S -80
```

With the window anchored, a miss is a real error — `capture-pane`, `list-panes` and `list-windows`
all exit 1 with `can't find window: <name>`, which is what you want: a target that names no window
should fail rather than answer.

**`display-message` is the exception, and it is the one people reach for.** With no `main` beside a
`main-stale`, `capture-pane -t '=bn:=main'` exits 1, while `display-message -p -t '=bn:=main'`
prints the session's *current* window and exits 0. So never test a window's existence with
`display-message`; ask something that fails:

```bash
tmux list-panes -t "=<repo>:=<branch>" -F '#{pane_id}' >/dev/null 2>&1 \
  || { echo "no window for <repo>:<branch>"; exit 1; }
```

`bn agent pane` is the better call regardless: it joins the target against recorded agent state
rather than guessing from window names, and since bn #1264 it anchors both halves too, so it
reports `no such window` instead of quoting a same-prefixed neighbour.

**Steer them.**

```bash
bn agent send <repo:branch> "<message>"            # delivers and returns the reply
bn agent send <repo:branch> "<message>" --detach   # fire-and-forget; it reports via bn agent done
bn agent answer <repo:branch> <option>             # resolve a blocked dialog, verified
bn agent instruct <repo:branch> "<standing rule>"  # persists on the note, survives a relaunch
bn agent kill <repo:branch>                        # process tree, window, lease and pool slot
```

Prefer these over raw keystrokes. They verify delivery, and `kill` actually frees the slot instead
of closing a window and leaving the harness reparented and running.

## Driving a harness TUI by hand

Only when the `bn agent` path is unavailable. Sending text to a TUI is not sending text to a shell,
and the failures are quiet ones — measured on Claude Code v2.1.260 / tmux 3.6, 2026-09-04:

**Paste as a block, never key by key.** An autosuggest or highlight TUI mangles a char-by-char
`send-keys`.

```bash
printf '%s' "$msg" | tmux load-buffer -b steer -
tmux paste-buffer -d -b steer -t "$pane"
```

**Submitting is harness-specific, and the obvious combination is the broken one.** `Escape`
followed immediately by a raw CR (`send-keys -H 0d`) does **not** submit in Claude Code: the two
arrive together, get read as Alt+Enter, and insert a newline. The text sits in the input box
looking exactly like a message you sent. Any of these do submit:

```bash
tmux send-keys -t "$pane" Enter                    # named key
tmux send-keys -t "$pane" -H 0d                    # raw CR, with no Escape before it
tmux send-keys -t "$pane" Escape; sleep 0.4; tmux send-keys -t "$pane" -H 0d
```

bn's own steer sends the raw CR because Copilot's compose TUI needs it. So do not pick one and
trust it — **send, then verify the box emptied**, and escalate to the other key if it did not:

```bash
box=$(tmux capture-pane -p -t "$pane" -S -5 | grep '❯' | tail -1 \
      | sed 's/\xc2\xa0/ /g; s/^[[:space:]]*❯//; s/[[:space:]]*$//')
[ -z "$box" ] || tmux send-keys -t "$pane" Enter      # still in the box — escalate
```

The `sed` is load-bearing. Claude Code pads its empty input box with **U+00A0**, a non-breaking
space, so the obvious `grep -qE '❯ +$'` never matches an idle box and reports every message as
unsent. Normalise the NBSP, strip the prompt and the padding, and *then* ask whether anything is
left.

**A fresh agent pane does not open on an input box.** Claude Code asks whether it should trust an
unfamiliar directory first. A readiness check that greps for `❯` matches *the dialog's own cursor*,
reports ready, and feeds your prompt to the dialog as a menu answer. Wait for the REPL's status
line (`? for shortcuts`), and read a dialog with `bn agent prompt` rather than firing digits at it.

## Rules that keep this honest

- **Never a new window for a long job.** Adjacent pane, same window.
- **Never send keys to a busy or prompting pane.** Open a job pane instead.
- **Never send secrets through `send-keys` or a paste buffer.** Both land in scrollback and in
  tmux's buffer list. Let the *user* type credentials, and say which pane is waiting.
- **Interactive prompts belong to the user.** Do not answer a credential or confirmation on their
  behalf.
- **A prompt may not be in the pane at all.** A GUI credential dialog or a browser flow surfaces
  outside tmux and the pane just looks hung. Check for a live child process before reporting a
  stall, and say *where* to look.
- **Report the real outcome**, exit code included. The pane and the log are the evidence.
- **Don't run it in tmux and also run it yourself** in a tool call. Pick one.
- **No tmux, no problem.** If `$TMUX` is unset or `tmux list-panes` fails, use the normal tool call
  and say once that you are doing so. Do not start a server to run one command.
