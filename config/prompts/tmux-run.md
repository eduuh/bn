---
name: tmux-run
description: Run commands in a visible tmux pane instead of hidden tool calls, and drive long-running work — builds, clones, installs, dev servers — in a named window you observe while continuing to work. Use for any command with side effects, anything taking more than ~30s, anything interactive, and any script the agent wrote.
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
wait for these.** Start it, hand the user a window name, keep working, check back.

```
 1. own named window ──▶ 2. tell the user ──▶ 3. keep working
                                                    │
                        6. report ◀── 5. read it ◀── 4. wait on REAL state
```

**1. Its own named window**, never the shared side pane. It runs for a long time; the
pane is needed for other things, and a name is how the user finds it later.

```bash
session=$(tmux display-message -p '#S')
tmux new-window -d -t "$session" -n bigmono-build \
  'bn build build-web-app; echo "--- exited $? ---"; exec zsh'
```

`exec zsh` keeps the window alive after it finishes so the output is still readable.

**2. Say where it is, in one line** — "building in the `bigmono-build` window, ~11 min" —
and move on. Do not narrate progress you have not checked.

**3. Keep working.** Anything not depending on that job continues now. Do not stall
the conversation on it.

**4. Wait on real state, in the background.** Never block the turn:

```bash
# best: the PID. Nothing else can match it.
until ! kill -0 "$pid" 2>/dev/null; do sleep 20; done
```

Run that with `run_in_background: true` and you get a notification when it lands.

Other honest signals: a port (`ss -tln | grep :3000`), a file appearing, `du -sh`
growing. Pick one that only becomes true when the job really progressed.

**5. Read the outcome from the window**, then report what it says — including
failure. Grep the whole scrollback (`-S -400`), not the last screen: an error that
scrolled away still happened.

## Four ways this goes wrong

Each of these produced a confident, wrong status report:

- **Matching command text.** `pgrep -f "clone --depth 1000"` and
  `capture-pane | grep "fast exited"` both match **your own command line** — the pgrep
  pattern, and the echoed `echo "--- fast exited ---"` before it ever runs. Result: "it
  is still running" when it finished, and "it finished" when it had not started.
  **Match on a PID, a port, or a file. Not on text you also typed.**
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

**1. Find the side pane.** The session is the one you're in; the side pane is the non-active pane of
the current window.

```bash
session=$(tmux display-message -p '#S')
pane=$(tmux list-panes -t "$session" -F '#{pane_index} #{pane_active}' | awk '$2==0 {print $1; exit}')
```

If there is no second pane, make one rather than stealing the user's:

```bash
[ -z "$pane" ] && pane=$(tmux split-window -d -t "$session" -h -P -F '#{pane_index}')
```

**2. Check it's idle before sending.** Never type into a pane that is mid-command or waiting on a
prompt — you will answer someone else's question.

```bash
tmux capture-pane -p -t "$session.$pane" -S -3
```

A shell prompt on the last line means idle. A running command, a `Username:`/`Password:` prompt, or
a pager means busy: open a **new window** instead (`tmux new-window -d -t "$session" -n <name>`) and
say why you did.

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
tmux send-keys -t "$session.$pane" '<command>' Enter
```

Long or hard-to-quote commands: write a script to the scratchpad first, then send its path. That
also gives the user something to read before it runs.

**4. Watch it, don't fire and forget.** Poll the pane; report what actually happened.

```bash
tmux capture-pane -p -t "$session.$pane" -S -40
```

For anything running longer than a minute, follow **Long-running jobs** above: a named window and a
wait on real state. Do not chain sleeps (`sleep N; <cmd>` is blocked by the harness), and do not
wait by grepping the pane for a marker you also typed — the echoed command line matches it.

Tell the user which pane or window to look at. Grep the scrollback for the outcome (`-S -400`)
rather than assuming success — a command that scrolled its error off the last screen still failed,
and a summary line can say "no failures" while errors sit 200 lines up.

**5. Get the exit code.** `send-keys` gives you the transcript, not the status. When the outcome
isn't obvious from the output, make the command state it:

```bash
tmux send-keys -t "$session.$pane" '<command>; echo "--- exited $? ---"' Enter
```

For a window you opened yourself, keep it alive so the user can read it afterwards:

```bash
tmux new-window -d -t "$session" -n <name> '<command>; echo "--- exited $? ---"; exec zsh'
```

## Rules that keep this honest

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
- **Closing a window kills its job.** A named window keeps a long job alive across your turns, but
  closing it takes the job with it. Say so before the user tidies up a window still doing work.
- **Report the real outcome**, including failures and non-zero exits. The pane is the evidence; if
  it says the command failed, say so.
- **Don't run in tmux and then also run it yourself** in a tool call. Pick one.
- **Name the window** for anything long-running (`bigmono-clone`, `test-run`) so the user can find it.

## When there is no tmux

If `$TMUX` is unset, tmux isn't installed, or `tmux list-panes` fails, fall back to the normal tool
call and say once that you're doing so. Don't create a server just to run one command.
