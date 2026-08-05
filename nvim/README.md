# lazybn — your bn world, lazygit-style

A Neovim cockpit over the `bn` engine. One keystroke opens a full-screen, keyboard-driven
board that answers *"what's going on right now?"* — the ranked next-action feed, the
attention list, your standup (today and the forward look at tomorrow), and the cron jobs
running across your repos (and the information they gathered) — without leaving the editor.
List panels on the left, a live **detail pane** on the right, contextual actions per panel.

## Use it

`<leader>n` (or `:LazyBn`, or the `:Bn` alias) opens the cockpit:

```
 ┌─ NEXT ────────────────────┬──────────────────────────────────┐
 │ 1 ⛔ Review PR #40         │  pr-observe                      │
 │ 2 ◷ Set up SAW box        │  scope:    global                │
 ├─ ATTENTION ───────────────┤  schedule: *-*-* *:07/30:00      │
 │ ⚠ cron build-main failing │  last run: 3m ago (exit 0)       │
 │ ⛔ bigmono/x  blockers:1       │  runs:     27 · 20 failures      │
 ├─ STANDUP ─────────────────┤  ── last output ──               │
 │ Today                     │  pr-observe: tracking 2 open PRs │
 │   ◷ Set up SAW box        │                                  │
 ├─ TOMORROW ────────────────┤     (the detail pane reflects    │
 │ Carrying forward          │      the focused row)            │
 │   ○ Set up SAW box        │                                  │
 ├─ CRON ────────────────────┤                                  │
 │ ✓ pr-observe   global …   │                                  │
 │ ✗ build-main   repo   …   │                                  │
 ├───────────────────────────┴──────────────────────────────────┤
 │ e edit script   R run now   x unregister   ⇥ panel   q quit   │
 └──────────────────────────────────────────────────────────────┘
```

- **NEXT** — the "what do I do now?" list (`bn next`): one ranked action feed folding cron
  failures, raised attention items, PRs awaiting your review, your stale in-flight PRs, today's
  open goals, and blocked notes — highest-priority first. `↵` opens the focused action's link.
- **ATTENTION** — the `bn doctor` attention list, most-pressing first: failing cron jobs,
  raised attention items, and notes that are stale, blocked, or goal-less. `↵` opens the note
  or link; `A` acks an attention item.
- **STANDUP** — what you'd say at standup today (`bn standup`): yesterday's done and undone,
  today's plan, and any open blockers. `↵` opens a row's link.
- **TOMORROW** — the forward look at tomorrow's standup: today's still-open goals (they carry
  forward) plus scheduled follow-ups due ahead. Shares the `bn standup` read.
- **CRON** — every scheduled/scripted job across repos with its health; the detail pane
  shows what the selected job last gathered (its `output_tail`).

### Keys

Global: `<Tab>`/`<S-Tab>` or `1`–`N` move between panels · `j`/`k` within · `^d`/`^u` scroll
the detail pane (for long notes and cron output) · `/` filter the focused panel (lazygit-style;
`<Esc>` clears it) · `?` toggle the keys cheatsheet · `r` refresh all · `q` close (`<Esc>` also
closes when no filter is active; restores your previous layout). The footer always shows the
focused panel's contextual keys, plus an `updated Ns ago` staleness clock on the right.

| Panel | Keys |
|-------|------|
| NEXT | `↵` open the action's link · `y` yank url · `r` reload |
| ATTENTION | `↵` open note/link · `A` ack item · `r` reload |
| STANDUP | `↵` open link · `y` yank url · `r` reload |
| TOMORROW | `↵` open link · `y` yank url · `r` reload |
| CRON | `e`/`↵` edit script · `R` run now · `x` unregister |

## Capture & recall

Two surfaces that work from *any* buffer, without opening the cockpit:

- **Quick capture** — `:BnAdd` picks a section then prompts for one line and writes it to the
  current repo/branch note via `bn add`; the direct binds skip the picker: `<leader>nt` todo ·
  `<leader>nb` blocker · `<leader>nd` decision. No-ops gracefully outside a git repo.
- **`:BnSearch <query>`** — a picker over `bn search` (full-text across every note and design
  doc, active and archived). Uses Telescope when installed (snippet entries + file preview),
  else `vim.ui.select` (which fzf-lua / dressing transparently take over). `<CR>` opens the
  matching note at the hit line.

The `<leader>n…` capture binds, like `<leader>n`, sit behind the `keymaps` flag; `:BnAdd` and
`:BnSearch` are always registered.

## Install (lazy.nvim)

```lua
{
  dir = vim.fn.expand("~/.config/bn/repo/nvim"),  -- install.sh anchors this to the checkout (~/.bn/repo)
  name = "lazybn",
  event = "VeryLazy",
  config = function() require("bn").setup() end,
}
```

`setup()` binds `<leader>n`; pass `{ keymaps = false }` to bind your own (`require("bn").world`),
or `{ cmd = "/path/to/bn" }` to pin a specific binary. `require("bn").status.component()` is
a cached string for a statusline (lualine et al).

### Statusline: the top `bn next` action

`status.component()` shows the per-branch counts (`bn bn/main ✓3 !0`). Your `#1` next action is
the most useful "keep me on top" signal, and the statusline is where you look constantly — so
two getters surface it, each throttled by its own TTL cache (a statusline redraws on every
cursor move, so neither spawns a subprocess storm):

- `status.next_component()` — just the top `bn next` item: its kind glyph + title
  (e.g. `⛔ review eduuh/bn#95`). Empty when the desk is clear. Drop it into your own section.
- `status.component()` with `next` enabled — appends the top action to the counts, falling back
  to the counts alone when nothing's queued, and to the empty string when there's no note:

  ```
  bn bn/main ✓3 !0   ▸ ⛔ review eduuh/bn#95
  ```

Enable the appended form via `setup`, or call `status.setup({ next = true })` directly:

```lua
require("bn").setup({ status = { next = true } })
```

Composing the standalone getter into lualine:

```lua
require("lualine").setup({
  sections = {
    lualine_x = {
      { function() return require("bn").status.component() end },
      { function() return require("bn").status.next_component() end },
    },
  },
})
```

Glyphs mirror the NEXT panel: `⚠` cron · bell attn · ban review · pull-request pr · `◷` goal ·
`○` blocked (Nerd Font icons, see `crates/bn-core/src/glyph.rs`).
The board is **live**: returning to its tabpage re-reads every panel (so a cron job that just
finished or a PR that just merged shows without a manual `r`), and the footer carries an
`updated Ns ago` clock. A low-frequency background poll is available but **off by default** —
opt in with `require("bn.ui").poll_interval = 30000` (milliseconds); it reuses the same async
fetch path, so it's just a periodic refresh.

## How it works

Async and JSON-driven. Every panel reads its surface (`bn next` / `bn doctor` / `bn standup`
/ `bn cron`, and `bn cron show <job>` for the detail pane) concurrently through `bn.exec`
(`vim.system`), unwraps the `{ ok, command, data, error }` envelope, and renders once it
lands. Mutations (`bn attn ack`, `bn run`, `bn cron unregister`, …) go through the same
path, then re-read just that panel. Paths come straight from the engine — nothing is
parsed or reconstructed in lua.

Layout lives in `lua/bn/ui.lua`; each panel is a small module in `lua/bn/panels/` with a
uniform `fetch` / `render` / `detail` / `keys` shape, so adding a panel is a single file.
