# Tmux Keybindings

Prefix: **`C-Space`** (Ctrl+Space)

The workflow: one **session per repo**, one **window per worktree**. Sessions are named after the bare repo (e.g. `tracker`, `kube-homelab`); windows are named after the branch (e.g. `feat-auth`). Plain (non-bare) clones get a single `main` window; bare repos like `dotfiles` open a window per worktree.

## Status bar

A near-monochrome bar that uses **both ends** of the screen. tmux has only one status line, so the bottom is the real status line (window tabs) and the top is drawn on the pane-border line — its rule is painted black so on a dark terminal only the text shows, like a second bar. Everything is derived from live state; segments collapse when they have nothing to say.

**Top — the context dashboard** (per pane, so splits self-document):

| Segment | Meaning |
|---------|---------|
| `N⚠` | Raised attention — failing cron jobs + open `bn attn` items (`bn doctor` mirror). Urgent in red. |
| `N● N✓` | Server-wide count of windows needing you — `●` waiting on input, `✓` done. |
| `◆ <goal>` | The branch note's **goal** — what you're working on here, a faded headline (truncated after 80 chars). |
| `⎇ <branch> ↑n ↓m ●k` | Git: branch, commits ahead `↑` / behind `↓`, dirty file count `●` (faint `●` when clean). |
| `⟳ <job>` / `⟳ <job>+N` | bn cron jobs running on this machine right now — the oldest by name, plus a count of the rest. Silent when idle, and when `[machines.<host>].cron` is off. |
| `<session> · HH:MM Day DD` | Session name and clock, pushed to the right edge. |

**Bottom — window tabs.** Active window is **white-bold**, inactive ones dim. Each carries its bn state as trailing badges:

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

Badges and the window → worktree link (`@bn_repo`/`@bn_branch`) are refreshed by
`bn tmux-reconcile`, which runs on every client attach and on the 5-minute cron. Window options do
not survive a tmux-resurrect restore, and the badges are otherwise written only to the window bn
happened to run in — so without the sweep a note edited by an agent in another window never reaches
that window's tab. Run it by hand to see its decisions:

```
bn tmux-reconcile --dry-run
```

It stamps a window only where the window's name agrees with the branch its pane is actually on. A
window named after its worktree whose branch has since changed is **reported, not guessed** — the
identity is what every other surface trusts, so a confident wrong answer there is worse than a
blank one.

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
| `d` | Detach client (built-in). |

## Worktrees (as windows)

| Key | Action |
|-----|--------|
| `o` | **Go to or create worktree.** fzf list of existing worktrees, **most recently active first** with `main` last — so the default row is where you were actually working. Select one → opens as window. Type a new name + Enter → creates worktree from latest `main`, opens as window. |
| `x` | **Close this worktree.** Drops its window immediately and files the reclaim — hand the pool lease back, or run `bn wt cleanup`'s gates for a conventional worktree — on the machine queue as a `cleanup` entry, which runs when the machine has a moment (`bn queue list --class maintenance`). Prompts first if the branch has uncommitted or unpushed work, or a live agent working in the checkout — an agent mid-turn has a clean tree, and closing over it would kill it silently. Refuses `main`/`master` and the controller slot — on those, and on any non-worktree pane, it is just a pane close. **Never closes a session:** a worktree owning its session's last window leaves that window respawned as a bare shell. The expensive half is deferred on purpose: a warm reset re-checks-out the tree and the cleanup gates make a `gh pr view` round-trip, neither of which a keystroke should wait on. `bn wt close --reclaim` runs it now instead. |
| `X` | Kill this window and all its panes. Leaves the worktree and its lease alone — `x` is the integrated close. |
| `D` | Pick any worktree to reclaim it + kill its window — the picker sibling of `x`, and the same reclaim: a pool slot hands its lease back, a conventional worktree goes through the merged-PR / clean-tree gates and has its note closed. It runs inline (it is already a popup, so nothing blocks), and refuses rather than forcing; `bn wt remove --force` is the override. |

In a non-bare session, `o` just opens/selects a `main` window at the repo path.

## Where agents run

One rule, enforced in `bn agent launch` — the chokepoint every entry point funnels through (the keybinds, `bn dispatch`, `bn control start`, and the `claude`/`copilot` shell wrappers from `bn shell-init`):

| Worktree | May host |
|---|---|
| outside a git worktree | **nothing.** Every session, including planning, must be backed by a repository branch. |
| `worktree/<repo>/main` | **nothing.** It stays clean on `main`, matching the `pre-commit` hook that already refuses commits there — and it is not somewhere you go either: the pickers hide it, and `main-sync` hard-resets it onto the remote tip every 15 minutes. |
| `_pool/1` — the **main slot** | the `bn control` orchestrator, and only it. It is also where *you* land when you pick "main": a detached checkout at the repo's base, so main-branch work has a home that isn't the main worktree. `bn control start` leases this slot from anywhere in the repo. |
| `_pool/2…N`, branch worktrees | exactly **one** agent each. |

So:

- Typing `claude` (or `copilot`) in the main slot doesn't dead-end — it asks *what is this worktree for?*, then hands the answer to `bn dispatch`, which leases a warm pool slot, seeds that branch's note with the goal **and** an open todo, and starts the agent there.
- Typing either harness outside a git worktree refuses to start. Enter an existing repository or create one first.
- Repo-less attention items remain queued until task enhancement or a human assigns a repo and branch; bn no longer opens planning scratch sessions for them.
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
| `host:<alias>` | A `bn connect` attachment to another machine. The work it shows lives in that machine's repos. |
| `-L orch` server | The dispatch worker server, a separate tmux server entirely — not on this namespace. |

Scheduled jobs get windows, never sessions: a heavy cron (`pool-warm`, `keep-warm`, `nightly-release`) opens a detached window in the session of the repo it serves, which closes itself when the job succeeds. See `config/jobs/README.md`.

`bn tmux-prune` reports sessions that name no repo (dry run; `--force` kills them), sparing anything attached, busy, or hosting an agent. It is never a cron. `bn tmux-snapshot-filter` keeps orphans out of the tmux-resurrect snapshot so a killed one does not come back on the next restore — it is wired to resurrect's `post-save-layout` hook and kills nothing.

Every launch through this chokepoint also records `agent-session: <harness> <id>` on the branch note — the same line `bn dispatch` writes, and the thing that makes `bn agent send --resume` and the dashboard's Steer panel work. The one exception is `claude --continue`, where the harness picks its own id; those agents are still steerable through their tmux window.

A harness started outside the shell (an IDE extension, an absolute path, a terminal that never sourced `bn shell-init`) skips the wrapper. `bn doctor` reports any lease that broke the rule after the fact, and warns while the wrappers are missing, inactive, or shadowed by a later `claude()` definition in your rc.

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
| `1`–`5` | Jump to window 1–5. |
| `[` / `]` | Previous / next window. |
| `<` / `>` | Swap window left / right. |

The three jump keys differ by **scope**, not by mechanism:

| Key | Scope | Shows |
|-----|-------|-------|
| `` ` `` | this session | *every* window — navigation |
| `Tab` | all sessions | the single top-ranked window needing you, no popup |
| `f` | all sessions | every live agent window + the orchestrator cockpit |
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
| `q` | Kill pane (with confirm). Leaves the worktree and its pool lease alone — `x` is the integrated close. |

## Popups

| Key | Action |
|-----|--------|
| `e` | **Toggle this worktree's editor.** One persistent nvim *per worktree*, as a pane rather than a popup. First press splits it in at 90% of the window, collapsing the command pane to a strip rather than killing it (it often holds a running agent); second press parks the pane in the detached `_bn_edit` session — nvim keeps running, the shell/agent gets its full width back; third press joins it back with buffers, undo, marks and LSP intact. Editors in different worktrees are independent and never evict each other. `<leader>eh` inside nvim parks it without reaching for the prefix. If nvim dies, the next press detects it, clears up and starts a fresh one. |
| `C-e` | Force the editor **visible** and land it on this branch's diff (never parks) — for returning to a worktree cold. |
| `d` | **Worktree diff picker.** Every worktree with a checkout, badged with whether its editor is up (`●` showing · `○` parked), with `bn diff` as the live preview. `↵` switches to that worktree's window and opens Diffview in *its* nvim. `ctrl-r` reloads. |
| `n` | Branch note popup (`note.md` for current git context). |
| `g` | Lazygit popup. `q` closes. |

## Copy Mode (vi)

Enter with **`C-Space Space`**.

| Key | Action |
|-----|--------|
| `v` | Start selection. |
| `C-v` | Toggle rectangle selection. |
| `y` | Copy to system clipboard + exit. |
| `/` `?` | Search forward / backward. |
| `n` `N` | Next / previous search match. |
| `g` `G` | Jump to top / bottom. |
| `C-u` `C-d` | Half-page up / down. |
| `q` or `Esc` | Exit copy mode. |

Outside copy mode: `p` paste-buffer, `P` choose-buffer.

## Config

| Key | Action |
|-----|--------|
| `R` | Reload `~/.tmux.conf`. |

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
| `bn brief` | Lean SessionStart view: goal, blockers, open todos (cap 10), last 3 Progress, last 3 Decisions. |
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
| `bn log-progress "text"` | Append timestamped line to Progress (used by SessionEnd hook). |
| `bn close` | Close current branch's note. |
| `bn reopen` | Reopen a closed note. |
| `bn prune` | Close notes whose worktrees are gone. |
| `bn reset` | Remove non-main worktrees + close notes. |
| `bn archive` | Archive old closed notes. |
| `bn migrate [--dry-run]` | Convert legacy md-only notes to split md + yaml. |

## Views

| Command | Action |
|---------|--------|
| `bn summary` / `s` | Dashboard: open todos + blockers across branches. |
| `bn status` / `st` | One-line status for current branch. |
| `bn todo` / `t` | Open todos across all branches. |
| `bn list` / `l` | Active notes (`--all` for closed too). |
| `bn worktrees` / `w` | All active worktrees with detail. |
| `bn search <text>` | Search across all notes. |
| `bn stale [--days N]` | Notes with no recent activity. |

## Build

| Command | Action |
|---------|--------|
| `bn build` / `b [name]` | Run a repo-scoped script (default: `build`). |

## Links

| Command | Action |
|---------|--------|
| `bn pr [url]` | Link or open PR for current branch. |
| `bn link <id>` | Link or open work item. |
| `bn files` / `f` | Investigation file management. |

---

# The workflow in one page

1. **Start a session** — `C-Space C-o` → pick a repo. `tat` creates or switches.
2. **Branch off main** — `C-Space o`, type `feat/foo`. `bn wt open` fetches, creates the worktree from latest main, opens it as a window named `feat-foo`.
3. **Work** — `C-Space e` for nvim, `C-Space g` for lazygit, `C-Space n` for the branch note. Inside nvim use `<Space>nn` to jump to the note.
4. **Context-switch** — `C-Space [` / `]` between worktree windows in the same session. `C-Space C-o` to switch to a different repo.
5. **Done with a branch** — `C-Space X`, confirm, worktree is removed and window closed.
6. **Done for the day** — `C-Space t` kills the session and moves to the next.
