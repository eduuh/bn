# tmux-workflow

The tmux + worktree layer of this repo: the project/worktree picker, note + edit
popups, agent cockpit, and status bar. `install.sh` (full mode) links it onto
`~/.config/bn/repo`, which the tmux config resolves its popup scripts through, so it
works wherever the repo is cloned. Full keybinding reference: [`docs/tmux.md`](docs/tmux.md).

## Runtime

`bn tat` — the project/worktree picker — is now a **native** subcommand of the `bn`
binary (`crates/bn-cli/src/tat.rs`): it spawns fzf and drives tmux directly, the same
way `bn wt` does. It runs identically on Linux, macOS, Windows Git Bash, and WSL —
discovery, the fzf preview, session kill, codespaces, and the frecency sort are all
there (`bn tat --pick-stage list|preview|kill` are the internal fzf callbacks), with
`bn tat --root win` scoping to the Windows-filesystem repos. It replaced the
cross-platform bash `bin/tat`/`bin/tatw` + `lib/codespaces.sh` at migration.

Inside a bare repository, the same picker lists live windows, local worktrees, pool
slots, and **your open remote PRs that do not yet exist on this laptop**. Remote PR
discovery is cached briefly; selecting a `PR` row fetches its source branch and
opens it through the normal worktree path (a warm lease for pooled repositories,
otherwise a conventional worktree). `bn wt remote` remains the batch and
non-interactive equivalent.

A codespace is treated as a remote worktree of the same `session = repo`,
`window = branch` model: it shows up in the picker (inside its repo's worktree list
if cloned locally, else as a top-level `(codespace)` row), and selecting it opens a
`<branch> ☁` window that `gh cs ssh`'s into the codespace and attaches its
per-branch tmux session — so the same `bn` `<repo>/<branch>` note serves both the
local worktree and the codespace. Needs `gh`; degrades silently without it. See
[`docs/tmux.md`](docs/tmux.md#codespaces-remote-worktrees).

The status-bar segments and popups are now **native `bn` subcommands** (`bn tmux git`,
`tmux-goal`, `tmux-attn-count`, `tmux-done-badge`, `tmux-cockpit`, `tmux-help`,
`tmux-note`, `tmux-keys`) — see `crates/bn-cli/src/cmd/tmux_status.rs` and `tmux_keys.rs`; quick-capture is `bn add
<section> --prompt` and `prefix e` inlines `nvim .`. (Worktree open/create/remove moved
to native `bn wt`; `wt-clone` → `bn wt clone`, `wf-open` → `bn open`.)

The OS-branching adapter layer (`lib/adapters.sh`) and the bash worktree primitives
(`lib/worktree.sh`) were removed once their last consumers (`wt-clone`, `wf-open`) went
native — `bn tat`/`bn wt` do their own path normalization and bare+worktree logic in Rust.

## Layout

- `tmux.conf` — the canonical tmux config; consuming dotfiles symlink `~/.tmux.conf`
  through to it. OS-specific bits (shell, clipboard) are guarded inline.
- the project/worktree picker is now native `bn tat` (and `bn tat --root win` for the
  Windows-filesystem repos, the old `tatw`) — see `crates/bn-cli/src/tat.rs`. The
  cross-platform bash `bin/tat`/`bin/tatw` were retired at migration.
- `bin/` — the `tmux-agent.sh` front-end, self-locating via `${0:A:h}`.
  (Status segments and popups moved to native `bn tmux-*`; worktree open/create/remove to
  `bn wt` — `wt-clone` → `bn wt clone`; `wf-open` → `bn open`; the resume-or-fresh `agent`
  launcher → `bn agent launch`; the old `tmux-wt*.sh` /
  `tmux-{note,edit,help,cockpit}.sh` / `bn-*.sh` were retired.)
- `bn agent launch` (native, was `bin/agent`) — launch an AI coding agent in the current dir,
  resuming this dir's recorded session if one exists else starting fresh (copilot by default;
  claude opens in yolo mode). The single home of the resume-or-fresh rule; `tmux-agent.sh`
  (`A`/`G`) and `bn dispatch` both call it, and you can run it directly:
  `bn agent launch` (copilot) / `bn agent launch claude`.
- `lib/tmux-lib.sh` — vendored shared library the zsh helpers source.
- the repo's top-level `install.sh` (full mode) links this checkout onto
  `~/.config/bn/repo` and owns `~/.tmux.conf` (→ `config/workflow/tmux.conf`). Run
  `./install.sh --core` to skip this layer.

## Worktree lifecycle hooks

When `ensure_worktree` creates a **new** worktree (not when it reuses an existing one),
it runs a `post-create` hook so a fresh branch is ready to work in without manual setup —
copying gitignored files (`.env`), installing deps, etc. The zsh `config/scripts/tmux-lib.zsh`
primitive and native `bn wt` both honor it.

Hooks are plain executables, resolved in order (both run if both exist):

| Location | Scope |
|----------|-------|
| `~/.config/bn/hooks/post-create` | every repo (personal) |
| `<repo>/.bn/hooks/post-create` | that repo only (committable) |

The hook runs with cwd set to the new worktree and these vars in its environment:
`BN_WORKTREE_PATH`, `BN_BRANCH`, `BN_REPO`, `BN_MAIN_PATH` (the repo's main worktree,
for copying shared/gitignored files from). A non-zero hook is reported but never fails
worktree creation. See `config/hooks/post-create` for a starting point.

## Key bindings (from the consuming dotfiles `.tmux.conf`)

| Key | Helper                     |
|-----|----------------------------|
| C-o | `bn tat` (native)               |
| C-w | `bn tat --root win` (native; Windows repos) |
| o/W | `bn wt open` / `bn wt open --new` (native) |
| D   | `bn wt remove` (native; removes worktree + kills its window) |
| n   | `bn tmux note` (native)    |
| e   | `tmux-git-diff.sh` (floating nvim on tracked changes vs. main) |
| C-g | `tmux-codespace-diff.sh` (floating remote nvim over SSH) |
| A   | `bin/tmux-agent.sh` (spawn Copilot agent window — the default) |
| G   | `bin/tmux-agent.sh … claude` (spawn Claude agent window) |
| S   | `bn tmux cockpit` (native; orchestration roll-up) |
| j   | `bn jump` (native; everything that needs you: windows + `bn next` queue) |
| Tab | `bn jump --next` (native; jump to top window needing you; repeat to cycle) |
| m   | watch this window (silence → ✓) |

`prefix C-g` expects nvim and Diffview installed in the Codespace; it prompts for the SSH alias
and remote checkout path, then compares the tracked worktree with the merge base of `origin/main`.
The helper can also be run from a local tmux pane to open the same popup after a Codespace agent
finishes.

## What the top row carries

The bar is one row. The left side holds the session pill and the tabs; the right side is the
dashboard, pushed to the far edge: the git segment (`⎇ <branch>`, ahead/behind, dirty count)
and the clock. The todo/blocker tally and the waiting-agent bells left the bar; `bn jump`
(`prefix f`) still shows them.

It used to be two rows, with the goal (or the linked PR's title) on the dashboard row. Once the
tabs shrank to numbers they fit beside the dashboard, and the goal was dropped rather than keep
a second row for it. `bn tmux goal` still prints it, and it is in the note itself.

Inactive tabs show the window **number** plus attention badges; the active tab adds its name
(`@bn_branch`, else the window name, cut to 20 columns). A name on every tab cost 20-30 columns
per window and overflowed the strip long before the windows ran out. When tabs do overflow,
amber `<` / `>` markers mark the hidden side (click them to step a window) and the strip scrolls
to keep the active tab visible. `prefix f` (or ``prefix ` ``, both `bn jump --session`) finds a
window by name.

Attention (`bn attn badge`) and the running cron job (`bn cron running`) are now the header of
`bn tat` (`prefix C-o`), painted on open and on `ctrl-r`. The commands themselves still work.

## Live agent activity in the status bar

Each window's tab badges the Claude Code states that need you, so a glance at the bar (or the
`prefix S` cockpit) tells you which of several parallel-agent windows wants you. Busy states
(`working`, `limited`) are not drawn:

| Glyph | State | Meaning |
|-------|-------|---------|
| `●` amber | `waiting` | Agent needs your input/permission |
| `✓` green  | `done`    | Agent finished its turn |

`✓` is also raised by the **terminal bell** (`window_bell_flag`) and by the
`prefix m` silence watch, so a window running something that is not a hooked agent — a
build, a test run, a non-Claude harness — still reports when it finishes.

Agents waiting in *other* sessions are not on the bar (a session is a repo, so the tabs only
list this repo's worktrees); `prefix Tab` / `prefix j` reach them.

The dot is set by `bn tmux agent <state>` (a per-window `@bn_agent` option, I/O-free,
no-op outside tmux) and is the source of truth for Claude windows. It takes precedence
over `@bn_done` — the silence-watch fallback (`bind m` / `bn tmux done-badge`) that
badges a `✓` on any non-Claude job (build/test) once its output goes quiet. A pane that exits
badges at once instead: `pane-died` passes its exit status, so a failed job gets a red mark rather
than a quiet `✓`.

Wire `@bn_agent` to Claude Code by adding these hooks to `~/.claude/settings.json`;
they inherit `$TMUX_PANE` from the agent's pane, so the decoration lands on the right
window:

```json
"UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "$HOME/.config/bn/bn tmux agent working" }] }],
"Notification":     [{ "hooks": [{ "type": "command", "command": "$HOME/.config/bn/bn tmux agent waiting" }] }],
"Stop":             [{ "hooks": [{ "type": "command", "command": "$HOME/.config/bn/bn tmux agent done" }] }]
```

The **GitHub Copilot CLI** has its own hook system (`~/.copilot/hooks/*.json`, all merged)
with the same lifecycle, so it decorates windows identically — drop a `bn-agent.json` in
that dir mapping Copilot's events onto the same three states:

```json
{
  "version": 1,
  "hooks": {
    "userPromptSubmitted": [{ "type": "command", "bash": "$HOME/.config/bn/bn tmux agent working", "timeoutSec": 5 }],
    "permissionRequest":   [{ "type": "command", "bash": "$HOME/.config/bn/bn tmux agent waiting", "timeoutSec": 5 }],
    "notification":        [{ "type": "command", "bash": "$HOME/.config/bn/bn tmux agent waiting", "timeoutSec": 5 }],
    "agentStop":           [{ "type": "command", "bash": "$HOME/.config/bn/bn tmux agent done",    "timeoutSec": 5 }]
  }
}
```

Copilot also writes a per-session `~/.copilot/session-state/<id>/events.jsonl` carrying the
same transitions (`assistant.turn_start`/`turn_end`, `permission.requested`). We deliberately
**don't** tail it for this signal: a hook runs inside the agent's pane and inherits
`$TMUX_PANE`, so the decoration lands on the right window with no daemon — the JSONL has no
pane/tty to route by. (That stream is the right source for richer telemetry, not for this.)

### Ambient attention count

The top-right of the status bar (just before the clock) shows **one number** — the total
of everything that wants you, server-wide: raised attention items (cron failures + urgent /
other, via `bn attn badge`) **plus** live agent windows (`waiting` needing input + `done`/
ready) across every session, trailed by a bell glyph. It goes **red** when anything is
urgent (an urgent item, a cron failure, or a wait ignored 15+ minutes) and **amber**
otherwise, and stays empty (clean bar) when all is quiet. `working` agents aren't counted —
they don't need you. It's produced entirely by `bn attn badge` from `#(...)` in
`status-format` (the native window tally — once `bn tmux-attn-count` — was folded into it),
and it's the ambient nudge toward the attention cockpit.

### Attention picker (`prefix j`)

The count tells you *something* needs you; `prefix j` takes you *there*. `bn jump` opens an
fzf popup stacking every window that needs you (`● waiting` > `! blocked` > `✓ done`) plus
the cross-repo `bn next` queue, each with a live `capture-pane` preview; ↵ switches focus
(`switch-client`) straight to the highlighted window. `working` agents are left out — they
don't need you yet.

`prefix Tab` is the no-popup fast-path: it jumps straight to the top-ranked window
needing you. Because it excludes the current window, pressing it repeatedly **cycles**
through everyone waiting — the window you land on drops out of the next pick.
