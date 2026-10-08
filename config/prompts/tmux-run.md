---
name: tmux-run
description: "Run commands in a visible tmux pane and drive long jobs, other agents, locks and TUIs from tmux. Use for builds, installs, anything slow or interactive, or anything that spawns another agent."
when: Any command with side effects, anything taking more than ~30s, anything interactive, and any script the agent wrote.
keywords: [tmux-run, tmux, run, commands, visible, pane, instead, hidden, tool, calls, drive, long-running]
---
You run commands **where the user can see them**. The default tool-call shell is invisible: its
output lands in the transcript after the fact, interactive prompts hang with no way to answer, and a
long job looks identical to a stalled one. A tmux pane fixes all three — the user watches it happen,
can read the scrollback, can type into it, and can Ctrl-C it without touching the agent.

## The rule

Run it in the tmux side pane when the command **changes something, takes a while, or might ask a
question**. Read-only inspection that answers a question you were about to answer anyway stays in
the normal tool call — routing `git status` or a `grep` through tmux is noise, not transparency.

Use the side pane for:

- Installers, builds, migrations, deploys, `setup*.sh`, package managers.
- Clones, fetches, syncs — anything doing real network or disk work.
- Anything that may prompt: credentials, `gh auth login`, a confirmation, an editor.
- Any script **you just wrote**. The user should watch your code run the first time, not read your
  summary of how it went.
- Anything you expect to take more than ~30 seconds.
- Test suites and linters whose output the user will want to scroll back through.

Keep in the normal tool call:

- `ls`, `cat`, `grep`, `git status`, `git diff`, `git log`, `--help`, `--version`.
- Reading a file to decide what to do next.
- Anything whose entire value is the text you're about to quote back.

When in doubt, use the pane. Being watchable costs nothing.

Write the command so it doesn't prompt unnecessarily — check before escalating (`dpkg-query` before
`sudo apt install`, "is it already installed" before installing). A needless prompt is not just
noise: it buries the one prompt that does matter, and it turns a re-runnable script into one that
stalls waiting for a password it never needed.

## Long-running jobs — the whole workflow

A build, a clone, an install, a dev server. Minutes to hours. **You do not sit and
wait for these.** Start it beside you, say where it is, keep working, check back.

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

**3. Keep working.** Anything not depending on that job continues now. Do not stall
the conversation on it.

**4. Wait on real state, in the background.** Never block the turn:

```bash
# the pane's own process, not a text match
until [ -z "$(tmux list-panes -t "$job" -F '#{pane_pid}' 2>/dev/null)" ]; do
  sleep 20
done
```

Run that with `run_in_background: true` and you get a notification when it lands.

**Keeping a shell open defeats automatic pane cleanup.** Do not append `exec zsh` after a
one-shot command. For commands sent into an existing interactive shell, save the exit status and
then run `exit`; treat pane disappearance as completion and inspect saved logs/status files.

Other honest signals: a port (`ss -tln | grep :3000`), a file appearing, `du -sh`
growing, or the artifact itself — `bn tmux prune --help` succeeding only once the new
binary is installed. Pick one that becomes true only when the job really progressed.

**5. Read the saved outcome**, then report what it says — including failure. The pane closes when
the command exits, so inspect its log and recorded exit status rather than pane scrollback.

## Builds and test runs — capture the outcome, do not scrape for it

A build or a test suite is the long-running job you will run most, and it is the one you most want
to *quote* afterwards: the failing test name, the compiler error, the pass count. The pane is for
the user to watch; a log is what you read. Get both at once:

```bash
log=<scratchpad>/build.log
status_file=<scratchpad>/build.exit
tmux clear-history -t "$job"
tmux send-keys -t "$job" \
  "set -o pipefail; cargo build --release 2>&1 | tee $log; rc=\$?; printf '%s\n' \"\$rc\" > $status_file; exit \"\$rc\"" Enter
```

* **`tee` to the scratchpad**, never `| tail`: the pane still streams, and afterwards you read the
  log with `grep -E '^(error|test result)'` instead of paging through a capture.
* **The build's exit code, not `tee`'s.** Run the pipeline under `set -o pipefail`, save `$?` to
  the status file, and exit the shell. Read the log and status file after the pane closes.
* **Never start a build with the hidden tool call "because it is only a build".** A release build
  under load is ten minutes the user cannot see, cannot cancel, and cannot tell from a hang.
  `run_in_background` on the tool call hides it just as well; it is not a substitute for the pane.
* **bn itself:** from any bn worktree or pool slot, `bn build build-to-local` compiles that branch
  into the shared cache and installs it as the live `bn` — that, in the pane, is how a bn change is
  tried.

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
- **Stale scrollback from the PREVIOUS run.** The same trap with a delay fuse: you poll
  `capture-pane | grep "--- done ---"` for run #2, and it matches run #1's marker still
  sitting in the pane history. The wait returns instantly and you report a result the
  new run has not produced. Before polling a rerun, either `tmux clear-history -t
  <window>` first, or — better — wait on the process itself:
  ```sh
  pid=$(pgrep -f "<the real process>" | head -1)
  until ! kill -0 "$pid" 2>/dev/null; do sleep 15; done
  ```
- **Piping through `tail` or `less`.** `cmd | tail -40` buffers until exit — and a dev
  server never exits, so the window stays blank and the job looks hung. Never pipe a
  long job's output.
- **Chaining sleeps.** `sleep 60; check` is refused by the harness. Use an `until` loop
  or `run_in_background`.
- **Killing only the parent.** `kill` on a build's top process leaves grandchildren
  running, and the orphan blocks the next attempt. Reap the tree and confirm:

  ```bash
  pkill -f "<pattern>"; pgrep -af "<pattern>" || echo cleared
  ```

## How

**1. Find the pane beside you.** Your own pane is `$TMUX_PANE` — use it, and let tmux resolve the
window from it. Do **not** ask `display-message` without a `-t`: that answers for the session's
active pane, which is usually not yours.

```bash
here=$TMUX_PANE
pane=$(tmux list-panes -t "$here" -F '#{pane_id} #{pane_active}' | awk '$2==0 {print $1; exit}')
```

If there is no second pane, make one rather than stealing the user's:

```bash
[ -z "$pane" ] && pane=$(tmux split-window -d -t "$here" -h -P -F '#{pane_id}')
[ -n "$pane" ] || echo "tmux refused the split — say so rather than targeting something else"
```

Keep the `%id`, not the index: indexes renumber as panes come and go.

**2. Check it's idle before sending.** Never type into a pane that is mid-command or waiting on a
prompt — you will answer someone else's question.

```bash
tmux capture-pane -p -t "$session.$pane" -S -3
```

A shell prompt on the last line means idle. A running command, a `Username:`/`Password:` prompt, or
a pager means busy: split a fresh pane off your own (`tmux split-window -d -t "$TMUX_PANE"`), or
open a named window (`tmux new-window -d -t "<session>:" -n <name>` — keep the trailing colon) when
a third pane would be too narrow to read. Say which you did and why.

**If the command needs `sudo`, check the credential cache first:**

```bash
sudo -n true 2>/dev/null && echo cached || echo "will prompt"
```

An uncached `sudo` sits at a password prompt that **expires on its own** (`sudo: timed out`, exit 1),
and re-sending just burns another timeout — the command never runs, and it can look like it did if
you only read the last line. Either ask the user to run `sudo -v` in that pane first, or hand them
the command to run in-session as `! <command>`. Never re-send something that already timed out
without changing something about it.

**3. Send the command.**

```bash
tmux send-keys -t "$session.$pane" '<command>; rc=$?; printf "%s\n" "$rc" > <status-file>; exit "$rc"' Enter
```

Long or hard-to-quote commands: write a script to the scratchpad first, then send its path. Tee
output to a log if it must be inspected after the pane closes.

**4. Watch it, don't fire and forget.** Poll the pane while the command runs; report what actually
happened from its saved log and status after the pane closes.

```bash
tmux capture-pane -p -t "$session.$pane" -S -40
```

For anything running longer than a minute, follow **Long-running jobs** above: its own pane beside
yours, and a wait on real state. Do not chain sleeps (`sleep N; <cmd>` is blocked by the harness), and do not
wait by grepping the pane for a marker you also typed — the echoed command line matches it.

Tell the user which pane or window to look at. If the outcome must be read after completion, save
the full output to a log and the exit status to a file; the pane closes and its scrollback is gone.

**5. Get the exit code.** `send-keys` gives you the transcript, not the status. Save the status
before exiting the shell so the pane closes when the command finishes:

```bash
tmux send-keys -t "$session.$pane" '<command>; rc=$?; printf "%s\n" "$rc" > <status-file>; exit "$rc"' Enter
```

For a window you opened yourself, let it close on completion. Save required output and status to
the scratchpad first; do not keep a shell open just to preserve scrollback:

```bash
tmux new-window -d -t "$session" -n <name> '<command>; rc=$?; printf "%s\n" "$rc" > <status-file>; exit "$rc"'
```

**6. Clean up panes and windows you opened.** One-shot command panes close automatically when
their command exits. When a long-running job or service is no longer needed, stop it with its
documented shutdown command and verify it has stopped before closing its pane. For a foreground
command you started, Ctrl-C is appropriate; wait for it to exit. Then close a remaining idle pane
or a window you created:

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
  leaving child processes behind. Use the job's shutdown method, verify it stopped, then clean up
  the pane/window you opened.
- **Report the real outcome**, including failures and non-zero exits. Use the saved log and exit
  status; if either shows failure, report it.
- **Don't run in tmux and then also run it yourself** in a tool call. Pick one.
- **Say where it is.** For a job pane, "beside this one"; for a window, name it (`bigmono-clone`,
  `test-run`) so the user can find it.

## When there is no tmux

If `$TMUX` is unset, tmux isn't installed, or `tmux list-panes` fails, fall back to the normal tool
call and say once that you're doing so. Don't create a server just to run one command.
