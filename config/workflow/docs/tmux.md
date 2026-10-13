# Tmux Keybindings

Prefix: **`C-Space`** (Ctrl+Space)

The workflow: one **session per repo**, one **window per worktree**. Sessions are named after the bare repo (e.g. `tracker`, `kube-homelab`); windows are named after the branch (e.g. `feat-auth`). Plain (non-bare) clones get a single `main` window; bare repos like `dotfiles` open a window per worktree.

## Status bar

A compact **one-row bar at the top**: session and window tabs on the left, command-run summary,
tunnels, git state and clock on the right. Pane-border labels appear only for split windows.
Segments collapse when they have nothing to say.

| Segment | Meaning |
|---------|---------|
| `build RUN 42s` | A wrapped command is still running; elapsed time is not a progress estimate. |
| `tests FAIL 37` | A wrapped command exited nonzero; logs remain accessible with `B`. |
| `build PASS` / `CANCEL` / `UNKNOWN` | Recorded success, signal cancellation, or a runner that disappeared before recording its outcome. |
| `!N` / `?N` / `+N` | Other retained failures, unknown outcomes, or additional running commands in this worktree. |
| `⎇ <branch> ↑n ↓m ●k` | Git: branch, commits ahead `↑` / behind `↓`, dirty file count `●` (faint `●` when clean). |

**Window tabs.** The active window has a blue pill, inactive ones are dim. Each carries its bn
state and, independently, a short command-run badge:

| Badge | Meaning |
|-------|---------|
| `!N` (red) | `N` open blockers on this branch's note. |
| `N` (dim) | `N` open todos. |
| `●` (amber) | agent **waiting** for your input. |
| `` (red) | agent **blocked** — stopped and cannot continue without you. |
| `` (amber) | agent wants a **decision**. |
| `` (red) | agent **failed**. |
| `` (dim) | agent **rate-limited** — paused on a usage cap; it resumes on its own. |
| `◌` (dim) | agent **working**. |
| `✓` (green) | agent **done** (or a watched build/test went quiet — see `m`). |
| `RUN` / `PASS` / `FAIL` / `CANCEL` / `UNKNOWN` | Worktree-scoped command results, based on the runner rather than pane silence. |

Agent/note badges and the window → worktree link (`@bn_repo`/`@bn_branch`) are refreshed by
`bn tmux reconcile`, which runs on every client attach and on the 5-minute cron. Window options do
not survive a tmux-resurrect restore, and the badges are otherwise written only to the window bn
happened to run in — so without the sweep a note edited by an agent in another window never reaches
that window's tab. Run it by hand to see its decisions:

```
bn tmux reconcile --dry-run
```

It stamps a window only where the window's name agrees with the branch its pane is actually on. A
window named after its worktree whose branch has since changed is **reported, not guessed** — the
identity is what every other surface trusts, so a confident wrong answer there is worse than a
blank one.

### Build/test runs

Run `bn cmd run -- --label build -- bn build` or wrap your test command the same way.
`prefix B` opens this worktree's run history: select a run to inspect its command, read the
retained log, or confirm a rerun. Reruns preserve argv/cwd but use the current environment and
new logs. `prefix c` still opens the general command picker.

Results survive returning to the shell, switching windows and restarting tmux. Every window
queries its active pane's worktree, so different worktrees do not share outcomes. A failure
remains prominent until a new run with the same label supersedes it; all earlier records remain
in the history. No popup or focus change occurs on completion.

`m` remains a legacy **quietness watch**, not proof that a build passed. Use the explicit
`RUN`/`PASS`/`FAIL` result for wrapped commands. An abruptly killed runner becomes `UNKNOWN`;
its child may still exist, so inspect before rerunning. Results do not yet detect source edits:
an old `PASS` does not verify today's changes. Records/logs are local and private; see
[`config/cmds/README.md`](../../cmds/README.md) for CLI details and retention.

## Which keys you actually use

tmux writes every bound key press to its message log (`key C-o: display-popup …`) and nowhere
else, and it forgets them on restart. The `tmux-keylog` job (part of the 5-minute `tick-5m` cron)
drains that log into `<state_dir>/tmux-keys.jsonl`. `message-limit 10000` in `tmux.conf` keeps
enough rows between runs. To see every curated binding with its press count, and the ones you
never press:

```
bn tmux keys              # last 7 days
bn tmux keys --since 30d
```

A `gap` in the header means the tmux server restarted or the log rolled over between runs, so the
counts are a floor.

The left pill shows the session name and **flips to reverse-video `PREFIX`** the moment you press `C-Space`, so you always know whether tmux is listening.

**Tab menu.** Right-click (or double-click) a tab for an action menu — *Open note · Lazygit · Rename · Watch · Kill* — acting on the clicked window. (Some terminals swallow right-click; double-click always works.)

> A couple of glyphs (`⎇ ◆ ●◌✓`) need a Nerd/powerline-patched font. If you see tofu boxes, swap them in `tmux.conf`.

## Sessions

| Key | Action |
|-----|--------|
| `C-o` | Project picker (fzf over `~/projects`). Creates or switches session. A bare repo's sub-picker includes windows, local worktrees, pool slots, and your open PRs missing from this laptop; selecting a `PR` row fetches and opens its branch. |
| `C-j` | Session tree (`choose-tree`). tmux's own picker, no fzf and no `bn` — the fallback that still switches sessions when PATH is bare or `bn` is mid-reinstall. `f` filters, `O` re-sorts, `x` kills the highlighted row, `Right` expands a session into its windows. |
| `t` | Kill current session, switch to next. |
| `.` | Rename current session. |
| `C-d` | Detach this client (leave tmux). `d` is the diff picker. |
| `C-p` / `BSpace` | Project / worktree picker without the frecency preview. |
| `C-w` | Project picker scoped to the Windows-filesystem repos. |

## Worktrees (as windows)

| Key | Action |
|-----|--------|
| `o` | **Go to or create worktree.** fzf list of existing worktrees, **most recently active first** with `main` last — so the default row is where you were actually working. Select one → opens as window. Type a new name + Enter → creates worktree from latest `main`, opens as window. |
| `x` | **Close this worktree.** Drops its window immediately, then reclaims the worktree in a background process: it hands the pool lease back, or runs `bn wt cleanup`'s gates for a conventional worktree, and reports the outcome a few seconds later as a status-line message. Prompts first if the branch has uncommitted or unpushed work, or a live agent working in the checkout — an agent mid-turn has a clean tree, and closing over it would kill it silently. Refuses `main`/`master` and the controller slot — on those, and on any non-worktree pane, it is just a pane close. **Never closes a session:** a worktree owning its session's last window leaves that window respawned as a bare shell. The expensive half runs in the background on purpose: a warm reset re-checks-out the tree and the cleanup gates make a `gh pr view` round-trip, neither of which a keystroke should wait on. |
| `X` | Kill this window and all its panes. Leaves the worktree and its lease alone — `x` is the integrated close. |
| `W` | Always open a **new** worktree window, even when one for the branch exists. |
| `D` | Pick any worktree to reclaim it + kill its window — the picker sibling of `x`, and the same reclaim: a pool slot hands its lease back, a conventional worktree goes through the merged-PR / clean-tree gates and has its note closed. It runs inline (it is already a popup, so nothing blocks), and refuses rather than forcing; `bn wt remove --force` is the override. |

In a non-bare session, `o` just opens/selects a `main` window at the repo path.

## Where agents run

One rule, enforced in `bn agent launch` — the chokepoint every entry point funnels through (the keybinds, `bn dispatch`, `bn fleet control start`, and the `claude`/`copilot` shell wrappers from `bn setup shell-init`):

| Worktree | May host |
|---|---|
| outside a git worktree | **nothing.** Every session, including planning, must be backed by a repository branch. |
| `worktree/<repo>/main` | **nothing.** It stays clean on `main`, matching the `pre-commit` hook that already refuses commits there — and it is not somewhere you go either: the pickers hide it, and `main-sync` hard-resets it onto the remote tip every 15 minutes. |
| `_pool/1` — the **main slot** | the `bn fleet control` orchestrator, and only it. It is also where *you* land when you pick "main": a detached checkout at the repo's base, so main-branch work has a home that isn't the main worktree. `bn fleet control start` leases this slot from anywhere in the repo. |
| `_pool/2…N`, branch worktrees | exactly **one** agent each. |

So:

- Typing `claude` (or `copilot`) in the main slot doesn't dead-end — it asks *what is this worktree for?*, then hands the answer to `bn dispatch`, which leases a warm pool slot, seeds that branch's note with the goal **and** an open todo, and starts the agent there.
- Typing either harness outside a git worktree refuses to start. Enter an existing repository or create one first.
- Repo-less attention items stay in the attention list until task enhancement or a human assigns a repo and branch; bn no longer opens planning scratch sessions for them.
- Typing it in a worktree that already has a live agent refuses, naming the holder: steer it with `bn agent send <repo>:<branch> "…"`, or free it with `bn agent kill <repo>:<branch>`.
- Typing it on a **bare terminal** (no surrounding tmux) opens the branch's window and attaches you to it, so the agent lands somewhere bn can steer, kill and show in the dashboard rather than in a pane nothing can address. Requires a real terminal and a running tmux server; `BN_AGENT_TMUX_REDIRECT=0` turns it off for one call.
- `wt pool status` shows slot 1 labelled `(controller)`; the configured pool size counts it, so `<repo> 4` means one controller plus three workers.
- `BN_AGENT_GUARD=0 claude …` bypasses the within-repo placement rules for one call — the repository requirement remains unconditional.

## Which sessions exist

The rule the placement table above states for *agents* holds for every session bn opens, including the ones it opens for itself: **a session is named after a repository**, and a window after a worktree. That is what `tat`, `bn jump`, the status bar and the dashboard all assume, so a session named anything else is invisible to every one of them.

Three exceptions, none of which is work:

| Session | What it is |
|---|---|
| `_bn_edit` | Storage — the holding pen for parked editor panes. Hidden from `C-j`, and any `_`-prefixed name is claiming the same private namespace. |
| `host:<alias>` | A `bn machine connect` attachment to another machine. The work it shows lives in that machine's repos. |
| `-L orch` server | The dispatch worker server, a separate tmux server entirely — not on this namespace. |

Scheduled jobs get windows, never sessions: a heavy cron (`pool-warm`, `keep-warm`) opens a detached window in the session of the repo it serves, which closes itself when the job succeeds. See `config/jobs/README.md`.

`bn tmux prune` reports sessions that name no repo (dry run; `--force` kills them), sparing anything attached, busy, or hosting an agent. It is never a cron. `bn tmux snapshot-filter` keeps orphans out of the tmux-resurrect snapshot so a killed one does not come back on the next restore — it is wired to resurrect's `post-save-layout` hook and kills nothing.

Every launch through this chokepoint also records `agent-session: <harness> <id>` on the branch note — the same line `bn dispatch` writes, and the thing that makes `bn agent send --resume` and the dashboard's Steer panel work. The one exception is `claude --continue`, where the harness picks its own id; those agents are still steerable through their tmux window.

A harness started outside the shell (an IDE extension, an absolute path, a terminal that never sourced `bn setup shell-init`) skips the wrapper. `bn next --stale` reports any lease that broke the rule after the fact, and warns while the wrappers are missing, inactive, or shadowed by a later `claude()` definition in your rc.

## Codespaces (remote worktrees)

A GitHub Codespace is treated as a **remote worktree** — the same `session = repo`, `window = branch` model, with the branch living in a codespace instead of on local disk. They surface in the `C-o` picker (needs `gh` on PATH, logged in with the `codespace` scope):

- A codespace whose repo **is** cloned locally appears inside that repo's `o`/`C-o` worktree picker as a `<branch> ☁` row, beside the local worktrees.
- A codespace whose repo is **not** cloned locally appears as a top-level `<repo>-<branch> (codespace)` row in `C-o`, like a loose session.

The local↔codespace match is by repo **basename**. When you cloned a repo under a different name than its codespace's repo (e.g. the codespace repo is `MyAppBridge` but you cloned `BigMono`), map them in a machine-local config (kept out of this repo) so the codespace nests under your local repo instead of a top-level row:

```
echo <codespace-repo-basename>=<local-repo> > ~/.config/wt/cs-repo-map   # e.g. MyAppBridge=BigMono
```

Selecting either ensures a local session named after the repo and opens/focuses a `<branch> ☁` window that `gh cs ssh`'s into the codespace and runs `tmux new-session -A -s <branch>` — attaching to (or creating) a per-branch tmux session **inside** the codespace. Codespaces don't ship `tmux`, so the attach installs it first when missing (`sudo apt-get install -y tmux`, Debian/Ubuntu base) — the first attach to a fresh codespace pays a one-time install, after that it's instant. Because the remote session is named after the branch, `bn --cat` resolves the *same* `<repo>/<branch>` note whether you're in the local worktree or the codespace.

**All** your codespaces list, running or not (`Available`, `Shutdown`, …) — state is shown per row, never filtered. `gh cs list` is cached ~60s (`$WF_CS_TTL`, cache at `$WF_CS_CACHE`) so the picker stays instant, and is called with `--limit ${WF_CS_LIMIT:-100}` so gh's default 30-item cap doesn't silently drop any. With `gh` absent or logged out, codespaces are simply omitted.

**Auth.** When `gh cs list` fails, the picker detects it (not "no codespaces") and shows a `⚠ codespaces: gh auth expired — Enter to fix` row; `tat --check` reports it too. Two causes, handled in order:

1. **Wrong active `gh` account.** If your codespaces live under a different account than your primary one, put that account's username in a machine-local config (kept out of this repo, like `~/.config/wt/win-root`):

   ```
   echo <gh-username> > ~/.config/wt/cs-account
   ```

   Then `tat --cs-switch` (or the `Enter to fix` row) switches the active account to it and the codespaces reappear — no browser needed.

2. **Lapsed `codespace` scope.** If the account is right but the scope expired, the fix falls back to the interactive refresh (`gh auth refresh -h github.com -s codespace`), opening the device login in a browser. Run it by hand any time:

   ```
   gh auth refresh -h github.com -s codespace
   ```

**Nesting caveat:** launching from inside local tmux puts the remote tmux in a window, so prefix keys collide. Give the codespace's tmux a different prefix, or attach a grouped session for an independent view: `tmux new-session -t <branch> -s <branch>-2`.

## Windows

| Key | Action |
|-----|--------|
| `w` | New window (same cwd). |
| `` ` `` | **Switch to any window in this session.** fzf over every window of the session you're in — quiet worktrees included, each with its tab badges (blockers · todos · agent state) and a live pane preview. Ordered **last-used first**, so a bare `↵` toggles back to the window you came from and never lands on the one you're already on. `ctrl-r` re-reads the badges. Type a few letters of the branch instead of counting tabs — this is what `1`–`5` can't do once a session holds more than five worktrees. |
| `r` | Rename window. |
| `T` | Label this window — a tab label that survives renames; empty clears it. |
| `1`–`5` | Jump to window 1–5. |
| `[` / `]` | Previous / next window. |
| `<` / `>` | Swap window left / right. |

The three jump keys differ by **scope**, not by mechanism:

| Key | Scope | Shows |
|-----|-------|-------|
| `` ` `` | this session | *every* window — navigation |
| `Tab` | all sessions | the single top-ranked window needing you, no popup |
| `f` | this session | the same picker as `` ` `` |
| `j` | all sessions + all repos | windows needing you (ranked by time-in-state, pane preview) over the `bn next` queue |
| `C-a` | all repos | the attention queue (cron failures, raised items, stale notes) |

## Panes

| Key | Action |
|-----|--------|
| `v` | Split right (vertical divider). |
| `s` | Split below (horizontal divider). |
| `←` `↓` `↑` `→` | Navigate between panes. |
| `S-←` `S-↓` `S-↑` `S-→` | Resize current pane by 5 cells. |
| `z` | Toggle zoom (fullscreen current pane). |
| `C-t` | Name this pane (empty clears). |
| `!` | Break this pane into its own window. |
| `@` | Pull another window's pane in beside this one. |
| `q` | Kill pane (with confirm). Leaves the worktree and its pool lease alone — `x` is the integrated close. |

## Popups

| Key | Action |
|-----|--------|
| `e` | **Floating read-only nvim on this worktree's changes vs `main`** (`bin/tmux-git-diff.sh`, `DiffviewOpen origin/main`; the latest commit when nothing differs). Agents call the same script to hand you a diff. |
| `C-g` | The same floating diff for a Codespace branch, over SSH (fzf host picker). |
| `C-e` | Show this worktree's persistent editor pane and land it on the branch diff (never parks) — for returning to a worktree cold. |
| `d` | **Worktree diff picker.** Every worktree with a checkout, badged with whether its editor is up (`●` showing · `○` parked), with `bn status --diff` as the live preview. `↵` switches to that worktree's window and opens Diffview in *its* nvim. `ctrl-r` reloads. |
| `n` | Branch note popup (`note.md` for current git context). |
| `g` | Lazygit popup. `q` closes. Says so instead of flashing shut when lazygit is not installed. |
| `E` | Open the newest pending plan review in nvim, then approve or request changes. |
| `Enter` | Scratch shell in this worktree; vanishes on exit. |
| `c` | Run one of your `bn cmd` commands (picker). |
| `B` | Inspect this worktree's recorded build/test runs, retained logs, or confirm a rerun. |
| `a` / `b` | Quick-add a todo / blocker to the branch note. |
| `l` | Links picker — open a saved `bn links` URL. |

## Agents

| Key | Action |
|-----|--------|
| `A` | New window on this worktree running `bn agent launch` (Copilot): resumes the worktree's recorded session, else starts fresh. The tab tracks the agent's state through `@bn_agent`. |
| `G` | The same with Claude (`bn agent launch claude`). |
| `S` | Cockpit popup: in-flight windows across sessions + a bn summary. |
| `j` | Jump picker — windows needing you over the `bn next` queue. |
| `Tab` | Jump straight to the top window needing you; repeat to cycle. |
| `m` | Watch this window: badge it ✓ when it goes quiet. |

## Text tools and pane logs

Keys below follow the `C-Space` prefix, except the selected-text actions, which
run inside copy mode after making a selection.

| Key | Action |
|-----|--------|
| `u` | **extrakto:** fuzzy-search this pane's retained scrollback. `Tab` inserts the selected text without submitting it; `Enter` copies it. `C-f` switches between words, paths, URLs and lines. |
| `F` | **fingers:** letter hints over visible paths, URLs, hashes and IDs. Press a hint to copy; a shifted hint pastes. `Esc` exits. |
| `M-o` (copy mode) | Open the selected URL or file through bn's platform-aware opener. |
| `M-e` (copy mode) | Open the selected file in a separate editor popup, not by typing a command into the shell or agent pane. |
| `M-s` (copy mode) | Search the selected text in the browser. |
| `Y` | Toggle output recording for this pane. The start/stop message gives the file path. |
| `C-y` | Save the visible pane text. |
| `H` | Save all retained scrollback for this pane (up to `history-limit`, currently 100,000 lines). |

Extraction and hint copies use tmux's OSC52 clipboard path, including remote
sessions; normal mouse and copy-mode copies still use the existing `bn-clip`
integration. `Tab` remains the attention jump, `P` remains the buffer picker,
and copy-mode `o` still switches selection ends.

Recording is **off by default**, never automatically started or restored. Logs
and text snapshots go to `~/.local/state/bn/tmux-logs/` on the machine running
tmux, not to the repository. The directory is private (`0700`), files are private
(`0600`), and repeated snapshots get unique names. Recording captures future pane
output; use `H` for earlier output. Other tools' existing pane pipes are not
replaced.

Output may contain credentials or other sensitive text. Stop recording after the
reproduction; there is no automatic retention or size limit. Snapshots are plain
text, not image screenshots.

Plugin sources and licenses are vendored without TPM. Fingers additionally needs
the pinned 2.7.1 binary: full installation downloads and verifies it, or run
`bash scripts/install-tmux-fingers.sh`. Official pinned binaries cover Linux
x86_64 and macOS arm64; other platforms need an upstream installation. Plugin
reloads never download or build dependencies.

Checks: `python3 scripts/check-tmux-plugins.py`,
`python3 scripts/check-tmux-logging.py`, and
`python3 scripts/check-install-tmux-fingers.py`.

## Copy Mode (vi)

Enter with **`C-Space Space`**.

**Mouse dragging always selects terminal text; releasing copies it to the system
clipboard.** This deliberately overrides in-application dragging, including in
Copilot and editors. Clicks and scrolling keep their existing behavior.

| Key | Action |
|-----|--------|
| `v` | Start selection. |
| `C-v` | Toggle rectangle selection. |
| `y` | Copy to system clipboard + exit. |
| `/` `?` | Search forward / backward. tmux's own bindings, deliberately not rebound: they carry `-T search`, so <kbd>↑</kbd> at the prompt recalls earlier searches. |
| `n` `N` | Next / previous search match. |
| `g` `G` | Jump to top / bottom. |
| `[` `]` | Jump to the previous / next **shell prompt** — i.e. to the start of a command's output. Needs OSC 133 marks from the shell (see the dotfiles `.zshrc`); without them these do nothing. |
| `C-u` `C-d` | Half-page up / down. |
| `q` or `Esc` | Exit copy mode. Scrolling back down to the live screen also exits on its own. |

Outside copy mode: `p` pastes the **system clipboard** (through `bn-clip`), `P` chooses a tmux buffer, `Space` enters copy mode.

Mouse-copy regression: `python3 scripts/check-tmux-mouse-copy.py`. It uses an
isolated tmux server and a simulated clipboard, leaving live sessions and the
system clipboard untouched.

## Config

| Key | Action |
|-----|--------|
| `R` | Reload `~/.tmux.conf`. |
| `?` | The cheat-sheet. **Generated** from `list-keys -N`, never hand-written — every binding carries its own `-N "Section · description"` note, so the sheet cannot drift from the config the way it used to. tmux's own notes come through too, which is how surviving stock bindings stay discoverable. |
| `C-d` | Detach this client. Nothing bound `detach-client` before; `d` is the diff picker. |

Removed on purpose, so muscle memory does not find them: `&` (stock kill-window — it
skipped the unsaved-work gate that `x` runs), `C-z` (suspend-client, which looks like a
hang), `"`/`%` (stock splits — they lose the worktree path; use `v`/`s`), and `C-r`
(tmux-resurrect's restore-over-live-session; `@continuum-restore` already restores at
server start).

---

# Nvim Keybindings

Leader: **`<Space>`**

Only the project-specific keys worth memorising — see `~/.config/nvim/lua/plugins/*.lua` for the full picture.

## Files & navigation

| Key | Action |
|-----|--------|
| `C-p` | Files (fzf-lua). |
| `<leader>ff` | Files (fzf-lua). |
| `<leader>fw` | Live grep. |
| `<leader>fo` | Old files / recent. |
| `<leader>fr` | Registers. |
| `<BS>` | Alternate buffer (`<C-^>`). |
| `C-s` | Save file. |

## Branch notes

| Key | Action |
|-----|--------|
| `<leader>nn` | Open `note.md` for current branch (or jump back via alternate buffer if already on it). |

## Plan artifacts

Plan mode stores Markdown plans in the bn note directory, not in the repo worktree:

```
<note-dir>/plans/active.md
<note-dir>/plans/agents/<session-id>.md
```

The branch note stays as an index/status file. The plan body lives in Markdown artifacts.

| Command | Action |
|---------|--------|
| `bn plan init` | Create `plans/active.md` and `plans/agents/`. |
| `bn plan status` | Show active plan path, status, and agent drafts. |
| `bn plan path --active` | Print/create the active plan path. |
| `bn plan path --agent <session-id>` | Print/create one agent draft plan. |
| `bn plan open` | Open `plans/active.md` in `$EDITOR` (`nvim` by default). |
| `bn plan promote <draft-or-session>` | Promote a draft to `plans/active.md`. |
| `bn plan migrate` | Convert legacy note todo/done-when sections into `plans/active.md`. |

Plan-mode agent launches start the agent first, then split the same tmux window vertically and open
the bn-owned plan `.md` beside the running agent. That keeps the worktree for code and the bn
system for planning artifacts, and it keeps the same plan usable across machines through
note-relative paths.

## Quickfix

| Key | Action |
|-----|--------|
| `;n` / `;p` | Next / previous quickfix item. |
| `<leader>ql` | Quickfix list (fzf). |

## LSP (when an LSP is attached)

| Key | Action |
|-----|--------|
| `<leader>rn` | Rename symbol. |
| `<leader>ca` | Code actions. |
| `<leader>gd` | Document diagnostics. |
| `<leader>gw` | Workspace diagnostics. |
| `<leader>wd` | Workspace diagnostics (alt). |
| `<leader>ws` | Workspace symbols. |
| `<leader>lr` | Restart LSP. |

## Git / Diffview

| Key | Action |
|-----|--------|
| `<leader>gc` | Git commits (fzf). |
| `<leader>gb` | Git branches (fzf). |
| `<leader>gs` | Git stash (fzf). |
| `<leader>gp` | Gitsigns preview hunk. |
| `<leader>gr` | Gitsigns reset hunk. |
| `<leader>vd` | Diffview open. |
| `<leader>vb` | Diffview branch history. |

## Trouble

| Key | Action |
|-----|--------|
| `<leader>xx` | Workspace diagnostics. |
| `<leader>xd` | Document diagnostics. |
| `<leader>xs` | Symbols. |
| `<leader>xq` | Quickfix. |
| `<leader>xl` | Loclist. |

## Bufferline

| Key | Action |
|-----|--------|
| `<leader>bp` | Pin buffer. |
| `<leader>bo` | Close other buffers. |

## UI

| Key | Action |
|-----|--------|
| `<leader>uu` | Toggle undotree. |

## Terminal

| Key | Action |
|-----|--------|
| `C-n` | Toggle floating terminal (toggleterm). |
| `Esc Esc` | Exit terminal-insert mode. |
| `<leader>tu` | Unlock outer tmux prefix (from inside a terminal). |
| `<leader>cy` | Claude Code (yolo — skips permission prompts). |

---

# `bn` (Branch Notes CLI)

`bn` is installed on your PATH by `install.sh` (`~/.bn/bin/bn`, legacy `~/.bin/bn`, or `~/.local/bin/bn`, depending on the layout / `BN_BIN_DIR`). Every git branch gets a note at `~/projects/worktree/personal-notes/branch-notes/branch-notes/<repo>/<branch>/note.md`. Managed through the CLI — not bound to tmux directly but the primary interface for session context.

## Core

| Command | Action |
|---------|--------|
| `bn` | Ensure note exists, print dir path. |
| `bn --cat` / `-c` | Print full note contents. |
| `bn cat --brief` | Lean SessionStart view: goal, blockers, open todos (cap 10), last 3 Progress, last 3 Decisions. |
| `bn --json` | Dump `note.yaml` as JSON for scripts / Claude skills. |
| `bn --edit` / `-e` | Open note in `$EDITOR`. |
| `bn --path` / `-p` | Print note dir path (no create). |

## Add to sections

| Command | Action |
|---------|--------|
| `bn add todo "text"` | Add todo. |
| `bn add blocker "text"` | Add blocker. |
| `bn add decision "text"` | Record a decision. |
| `bn add research "text"` | Add research item. |
| `bn add collab "text"` | Collaboration note. |
| `bn add ask "text"` | Question for user/team. |

## Lifecycle

| Command | Action |
|---------|--------|
| `bn done <id>` | Mark a todo done by stable id (e.g., `bn done t3`). |
| `bn done "text"` | Fall back to substring match — fails on ambiguity. |
| `bn note log "text"` | Append timestamped line to Progress (used by SessionEnd hook). |
| `bn note close` | Close current branch's note. |
| `bn note reopen` | Reopen a closed note. |
| `bn note prune` | Close notes whose worktrees are gone. |
| `bn note reset` | Remove non-main worktrees + close notes. |
| `bn note archive` | Archive old closed notes. |
| `bn note migrate [--dry-run]` | Convert legacy md-only notes to split md + yaml. |

## Views

| Command | Action |
|---------|--------|
| `bn list --summary` / `s` | Dashboard: open todos + blockers across branches. |
| `bn status` / `st` | One-line status for current branch. |
| `bn list --todos` / `t` | Open todos across all branches. |
| `bn list` / `l` | Active notes (`--all` for closed too). |
| `bn list --worktrees` / `w` | All active worktrees with detail. |
| `bn search <text>` | Search across all notes. |
| `bn note stale [--days N]` | Notes with no recent activity. |

## Build

| Command | Action |
|---------|--------|
| `bn build` / `b [name]` | Run a repo-scoped script (default: `build`). |

## Links

| Command | Action |
|---------|--------|
| `bn pr [url]` | Link or open PR for current branch. |
| `bn pr --wi <id>` | Link or open work item. |
| `bn note files` / `f` | Investigation file management. |

---

# The workflow in one page

1. **Start a session** — `C-Space C-o` → pick a repo. `tat` creates or switches.
2. **Branch off main** — `C-Space o`, type `feat/foo`. `bn wt open` fetches, creates the worktree from latest main, opens it as a window named `feat-foo`.
3. **Work** — `C-Space e` for nvim, `C-Space g` for lazygit, `C-Space n` for the branch note. Inside nvim use `<Space>nn` to jump to the note.
4. **Context-switch** — `C-Space [` / `]` between worktree windows in the same session. `C-Space C-o` to switch to a different repo.
5. **Done with a branch** — `C-Space X`, confirm, worktree is removed and window closed.
6. **Done for the day** — `C-Space t` kills the session and moves to the next.
