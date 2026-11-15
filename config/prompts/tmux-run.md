---
name: tmux-run
description: "Run long-running or interactive commands in tmux, leaving short commands in the session. Use for long builds or installs, nvim, servers, or interactive agents."
when: Commands expected to take more than ~30s, running servers, or commands needing user interaction such as nvim.
keywords: [tmux-run, tmux, run, commands, visible, pane, instead, hidden, tool, calls, drive, long-running, heartbeat, progress, inactivity]
---
Run short commands directly in the session. Reserve a visible tmux pane for long-running jobs
and commands that need user interaction. The user can watch a long job, answer prompts, use an
editor, or stop a server without interrupting the agent.

## The rule

Use a tmux side pane only when the command **is expected to take more than ~30 seconds, stays
running, or needs user interaction**. Side effects, network access, and being a newly written
script do not by themselves justify opening a pane.

Use the side pane for:

- Long builds, installs, clones, migrations, test suites, or other jobs taking more than ~30 seconds.
- Servers and watchers that stay running.
- Interactive editors and TUIs such as nvim, or commands needing credentials or user confirmation.
- Interactive agents the user needs to watch or steer.

Keep in the normal tool call:

- `ls`, `cat`, `grep`, `git status`, `git diff`, `git log`, `--help`, `--version`.
- Reading a file to decide what to do next.
- Short non-interactive tests, builds, installs, scripts, and git operations.

Default to the session for short non-interactive commands. Use a pane when the expected duration
or interaction requires it, not just because a command changes files.

Write the command so it doesn't prompt unnecessarily — check before escalating (`dpkg-query` before
`sudo apt install`, "is it already installed" before installing). A needless prompt is not just
noise: it buries the one prompt that does matter, and it turns a re-runnable script into one that
stalls waiting for a password it never needed.

## Long-running jobs — the whole workflow

A build, a clone, an install, a dev server. Minutes to hours. **You do not sit and
wait for these in the foreground.** Start it beside you, say where it is, keep working, and
await its completion signal in the background.

```
 1. adjacent pane ──▶ 2. tell the user ──▶ 3. keep working
                                                │
                    6. report ◀── 5. read it ◀── 4. wait on REAL state
```

**1. A pane next to yours, in your own window.** The user triggered the job from a
conversation with you, so the job belongs beside that conversation — not in a window
they have to go find. Anchor on **`$TMUX_PANE`**, never on a bare `display-message`:

```bash
job=$(tmux split-window -d -t "$TMUX_PANE" -h -P -F '#{pane_id}' \
        -c ~/projects/worktree/bn/main \
        'bn build build-to-local; echo "--- exited $? ---"')
```

* `-t "$TMUX_PANE"` splits *your* pane, so the job lands in the window the user is
  already talking to you in.
* `-d` creates it without moving focus. A cron-like job must never steal the cursor.
* `-P -F '#{pane_id}'` hands back `%88` — a **stable id**. Pane *indexes* renumber when
  a neighbour closes, so an index you stored five minutes ago may now name a different
  pane.
* `-c` sets the cwd, so the command does not have to `cd` and you can read the pane and
  know where it ran.
* Let the pane close when the command exits. Do not append `exec zsh` or another shell to keep it
  open. If output must be inspected afterwards, tee it to the scratchpad and save the exit status
  there before the pane closes. Read those files instead of pane scrollback.

Use a separate named window (`tmux new-window -d -t "<session>:" -n <name>`) only when
the job genuinely outlives the conversation — a dev server you will still be using
tomorrow — or when your own window is already split and a third pane would leave
everything unreadably narrow. Say which you chose.

**2. Say where it is, in one line** — "building in the pane beside this one, ~4 min" —
and move on. Do not narrate progress you have not checked.

For long batch jobs, run `bn cmd run -- <command...>` in that pane. It streams output,
saves a private log, emits a heartbeat every 60 seconds, and warns after 180 seconds without
output. Use `bn cmd run -- --label "<stage>" -- <command...>` to name the stage.
An inactivity warning means progress is unknown, not that the command has hung.
Leave interactive editors and TUIs unwrapped.

**3. Keep working.** Anything not depending on that job continues now. Do not stall
the conversation on it.

**4. Wait on the job's completion signal, in the background.** Do not poll from the agent.
Have the job signal completion even when it fails, and retain its exit status:

```bash
done="build-$$"
status_file=<scratchpad>/build.exit
job=$(tmux split-window -d -t "$TMUX_PANE" -h -P -F '#{pane_id}' -c <dir> \
  "bn cmd run -- cargo build; code=\$?; printf '%s\n' \"\$code\" > '$status_file'; tmux wait-for -S '$done'; exit \$code")
tmux wait-for "$done"
```

Run the wait with the harness's background tool and read the wrapper's saved log and exit result
after completion. The job emits its own heartbeats while the agent works elsewhere.

**Keeping a shell open defeats automatic pane cleanup.** Do not append `exec zsh` after a
one-shot command. For commands sent into an existing interactive shell, save the exit status and
then run `exit`; treat pane disappearance as completion and inspect saved logs/status files.

For a persistent service, check readiness with a concrete probe such as its port or health endpoint;
then retain its supported stop handle or recorded PID. A readiness probe or a growing artifact is
not proof that a one-shot job finished: use its completion signal, saved exit status, and log.

**5. Read the saved outcome**, then report what it says — including failure. The pane closes when
the command exits, so inspect its log and recorded exit status rather than pane scrollback.

## Builds and test runs — capture the outcome, do not scrape for it

A long build or test suite is the job you will run most in a pane, and it is the one you most want
to *quote* afterwards: the failing test name, the compiler error, the pass count. The pane is for
the user to watch; a log is what you read. Get both at once:

```bash
log=<scratchpad>/build.log
status_file=<scratchpad>/build.exit
job=$(tmux split-window -d -t "$TMUX_PANE" -h -P -F '#{pane_id}' -c <dir> \
  "set -o pipefail; cargo build --release 2>&1 | tee $log; printf '%s\n' \"\$?\" > $status_file")
```

* **`tee` to the scratchpad**, never `| tail`: the pane still streams, and afterwards you read the
  log with `grep -E '^(error|test result)'` instead of paging through a capture.
* **The build's exit code, not `tee`'s.** Run the pipeline under `set -o pipefail`, save `$?` to
  the status file, and exit the shell. Read the log and status file after the pane closes.
* **Long builds belong in the pane; quick checks stay in the session.** A release build under
  load can take ten minutes. Background execution alone is not a substitute for a visible pane
  when the job is long-running.
* **bn itself:** build and test the worktree-local binary to try a change. `bn build build-to-local`
  uses a shared cache and replaces the live installation; run it only with explicit installation
  approval and coordination with affected owners.

## Six ways this goes wrong

Each of these produced a confident, wrong status report:

- **Assuming the pane you created is the pane you got.** `new-window` and
  `split-window` can *fail* — most often on a transient index collision, because
  several agents share one tmux session and another one claimed the index between
  tmux picking it and taking it. The `-P` output is then **empty**, and the next
  bare call answers about somebody else's pane:

  ```bash
  win=$(tmux new-window -d -t bn -n build -P -F '#{window_id}')   # -> "" on failure
  pid=$(tmux list-panes -t "$win" -F '#{pane_pid}')               # -t "" -> the ACTIVE window's pid
  ```

  That reads as success and then waits on an unrelated process. **Check the id came
  back before using it**, and retry rather than pressing on:

  ```bash
  [ -n "$win" ] || { echo "tmux refused the window; retrying"; win=$(tmux new-window -d -t bn: -n build -P -F '#{window_id}'); }
  ```
- **Anchoring on the session instead of your own pane.** `tmux display-message -p '#S'`
  with no `-t` answers for the session's **active** pane — which, for an agent running
  in a background window, is somebody else's. Measured: `$TMUX_PANE` was `%57` in
  window 4 while bare `display-message` reported window 3. Every target built from that
  points at the wrong window. **Anchor on `$TMUX_PANE`.**
- **Matching command text.** `pgrep -f "clone --depth 1000"` and
  `capture-pane | grep "fast exited"` both match **your own command line** — the pgrep
  pattern, and the echoed `echo "--- fast exited ---"` before it ever runs. Result: "it
  is still running" when it finished, and "it finished" when it had not started.
  **Match on a PID, a port, or a file. Not on text you also typed.**
- **Stale scrollback from the PREVIOUS run.** The same trap with a delay fuse:
  `capture-pane | grep "--- done ---"` for run #2 matches run #1's marker still
  sitting in the pane history. The wait returns instantly and you report a result the
  new run has not produced. Give each run its own completion signal and saved outcome, and
  use the harness's background wait rather than polling scrollback or process state.
- **Piping through `tail` or `less`.** `cmd | tail -40` buffers until exit — and a dev
  server never exits, so the window stays blank and the job looks hung. Never pipe a
  long job's output.
- **Chaining sleeps.** Do not repeatedly sleep and check from the agent. Use the harness's
  supported background-execution mechanism and completion handle; do not assume a tool or
  option name.
- **Killing only the parent.** `kill` on a build's top process leaves grandchildren
  running, and the orphan blocks the next attempt. Record the owned job's PID/process tree
  at launch. Stop it through the harness's supported process-tree stop handle or specific
  recorded PIDs, then verify those processes exited. Never use name/pattern-based killing.

## How

**1. Create a task-owned pane beside you.** Your own pane is `$TMUX_PANE` — use it, and let tmux resolve the
window from it. Do **not** ask `display-message` without a `-t`: that answers for the session's
active pane, which is usually not yours.

```bash
here=$TMUX_PANE
pane=$(tmux split-window -d -t "$here" -h -P -F '#{pane_id}')
[ -n "$pane" ] || { echo "tmux refused the split"; exit 1; }
```

Keep the `%id`, not the index: indexes renumber as panes come and go. Reuse an existing pane
only when this task created it or its owner explicitly approved reuse. An idle prompt does not
establish ownership.

**2. Check it's idle before sending.** Never type into a pane that is mid-command or waiting on a
prompt — you will answer someone else's question.

```bash
tmux capture-pane -p -t "$pane" -S -3
```

A shell prompt on the last line means idle, not authorized for reuse. A running command, a `Username:`/`Password:` prompt, or
a pager means busy: split a fresh pane off your own (`tmux split-window -d -t "$TMUX_PANE"`), or
open a named window (`tmux new-window -d -t "<session>:" -n <name>` — keep the trailing colon) when
a third pane would be too narrow to read. Say which you did and why.

**If the command needs privilege, follow the current harness's secure prompting mechanism.**
When secure askpass is available, run authorized commands with `sudo -A` through that harness.
Otherwise hand the privileged command to the user in an interactive terminal. Never collect
passwords yourself, bypass a denied request with another elevation mechanism, or retry a timed-out
prompt unchanged.

**3. Send the command, saving its exit status.** `send-keys` gives you the transcript, not the
status, so write it to a file:

```bash
tmux send-keys -t "$pane" '<command>; printf "%s\n" "$?" > <status-file>' Enter
```

Append `; exit` only in a pane you created in step 1. Never in a pane you found: that is the user's
shell, and `exit` closes it.

Long or hard-to-quote commands: write a script to the scratchpad first, then send its path. Tee
output to a log if it must be inspected after the pane closes.

**4. Await completion, don't poll.** For every job routed to tmux, follow **Long-running jobs**
above: wait on its completion signal in the background and read the saved log/status afterwards.
Read a pane only to diagnose a concrete prompt or unexpected state, not as a progress-polling loop.
Do not chain sleeps or grep scrollback for a marker you also typed — the echoed command matches it.

Tell the user which pane or window to look at. If the outcome must be read after completion, save
the full output to a log and the exit status to a file; the pane closes and its scrollback is gone.

**5. Read the exit code** from `<status-file>` once it exists. For a window you open yourself, pass
the command to `new-window` so it closes on completion:

```bash
tmux new-window -d -t "<session>:" -n <name> '<command>; printf "%s\n" "$?" > <status-file>'
```

**6. Clean up panes and windows you opened.** One-shot command panes close automatically when
their command exits. When an owned job or service is no longer needed, use its documented shutdown
method (Ctrl-C is appropriate for a foreground command you started), supported harness stop handle,
or specific PIDs recorded at launch. Await termination without polling and verify it stopped before
closing its pane; never stop a process by name or pattern. Then close a remaining idle pane or
window you created:

```bash
tmux kill-pane -t "$job"
tmux kill-window -t "$window"
```

Only run the command matching the resource you created. Do not close a pane to stop a service; that
can leave child processes running. Never close a window that contains other work.

## Beyond one pane: locks, queries, other agents

### Serialising work across worktrees

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

### tmux as a query surface

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

### Independent agents in other sessions

These launch and placement rules apply to standalone bn workers. Ordinary Copilot fleet /
Claude subagents use the harness's native delegation tools and do not require a separate
tmux session or `bn dispatch`. Their long-running commands still follow the command-routing
rule above.

An agent is a long-running job with opinions. It goes where the model says it goes — **another
repo's session, in that branch's window** — because that placement is the only reason
`bn agent live`, the tab badges, the steer path and the kill path can find it at all. A hand-rolled
`tmux new-session` gets you an agent no tool can see.

**Launch through `bn dispatch`**, which leases a worktree, seeds the branch note with the task, and
opens the window in the right session:

```bash
bn dispatch --repo <repo> --agent claude "<what to do>"     # a ship task
bn dispatch --repo <repo> --agent claude --skill investigate "<what to find out>"
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

### Driving a harness TUI by hand

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

- **Close every pane you opened as soon as its work is done.** One-shot commands exit on their
  own. An interactive agent or shell does not: when the task is finished and its result is saved
  or read, send `/exit` (or `exit`), confirm the pane is gone with `tmux list-panes -t "$TMUX_PANE"
  -F '#{pane_id}'`, and only then `tmux kill-pane -t "$job"` if it is still there. Never leave a
  finished pane for the user to tidy. Closing is part of the job, so do it in the same turn that
  reports the result.

- **Never start an agent in non-interactive mode in a pane.** `copilot -p`, `claude -p`, or any
  `--print`/`--prompt` form, and anything piped through `tee`, prints nothing until it exits, so the
  pane stays dark and looks hung (measured: 90s of blank pane). Start the agent interactively
  (`copilot`, `claude`), then deliver the prompt with `tmux send-keys` after the UI is up, and
  watch it live. Tool access in a fresh `-p` session also differed from an interactive one (the
  Teams MCP tools were missing). If output must be kept, read the session transcript, not a pipe.
- **Never send keys to a busy or prompting pane.** Open a window instead.
- **Never send secrets through `send-keys`.** It lands in scrollback and in tmux's buffers. If a
  command needs a token or password, run it in a visible pane and let the *user* type it — say
  plainly which window is waiting and what it wants.
- **Interactive prompts belong to the user.** Do not auto-answer a credential or confirmation
  prompt on their behalf.
- **A prompt may not be in the pane at all.** A Windows credential manager, a GUI auth dialog or a
  browser flow surfaces *outside* tmux — the pane just sits there looking hung. Before reporting a
  stall, check whether something is genuinely waiting (a live child process, `tasklist.exe` showing
  a dialog) and tell the user **where** to look.
- **Stop jobs before closing panes.** Closing a pane or window can kill its foreground job while
  leaving child processes behind. Use the owned job's shutdown method, supported stop handle, or
  recorded PIDs, await termination without polling, verify it stopped, then clean up the
  pane/window you opened. Never kill processes by name or pattern.
- **Report the real outcome**, including failures and non-zero exits. Use the saved log and exit
  status; if either shows failure, report it.
- **Don't run in tmux and then also run it yourself** in a tool call. Pick one.
- **Say where it is.** For a job pane, "beside this one"; for a window, name it (`repo-clone`,
  `test-run`) so the user can find it.

## When there is no tmux

If `$TMUX` is unset, tmux isn't installed, or `tmux list-panes` fails, fall back to the normal tool
call and say once that you're doing so. Don't create a server just to run one command.
