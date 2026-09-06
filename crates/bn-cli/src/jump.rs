//! Native `bn jump` — the prefix-`j` attention picker and the prefix-`Tab` fast-jump. Phase 2 of
//! the picker migration: ports `tmux-attention-all.sh` (the unified "everything that needs me"
//! popup) and `tmux-jump.sh --next` (jump straight to the top-ranked window).
//!
//! Like the `doctor` picker it is self-referencing — fzf's `start`/`ctrl-r` reloads and its
//! `--preview` re-invoke `bn jump --pick-stage <list|preview>`; the row list stacks two native
//! sources behind one schema (`display \t kind \t target \t title`):
//!   1. live tmux windows that need you — `waiting (bell) · done (check) · blocker (ban)` Nerd
//!      Font badges, ranked, target = the
//!      `window_id`; the preview is a live capture of that window's active pane.
//!   2. the cross-repo `bn next` queue (read from the warm cache), target = the action link.
//!
//! Extends the `bn wt` precedent (native fzf + native tmux dispatch); see `MIGRATION.md`.

use bn_core::next::{Action, ActionKind};
use bn_core::resolver::Config;
use bn_core::{glyph, style};
use std::io::Write;
use std::process::{Command, Stdio};

use crate::proc::{tmux, tmux_raw, tmux_run};

/// The hidden callback stages a `jump` fzf session re-invokes (`bn jump --pick-stage <s>`).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum JumpStage {
    /// Emit the unified row list (the `start`/`ctrl-r` reload feed).
    List,
    /// Render the detail panel for the focused row.
    Preview,
    /// Emit the `--session` picker's row list (its `ctrl-r` reload feed), so badges that changed
    /// while the popup was open refresh in place.
    SessionList,
}

/// fzf field separator. Field 1 is the colored display; 2..=4 are the hidden lookup columns.
const TAB: char = '\t';

/// One unified row: what fzf shows, plus the hidden `kind`/`target`/`title`/`id` columns its
/// bindings reference. `target` is a `window_id` for window rows and a link for `bn next` rows;
/// `id` is the underlying attention-item id (empty unless this row is a startable attn item).
struct Row {
    display: String,
    kind: String,
    target: String,
    title: String,
    id: String,
}

impl Row {
    /// `display \t kind \t target \t title \t id` — the exact line fed to fzf and re-emitted by the
    /// `list` stage, so a `reload` reproduces the same columns.
    fn line(&self) -> String {
        format!(
            "{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}",
            self.display, self.kind, self.target, self.title, self.id
        )
    }
}

/// A ranked tmux window needing you, before the rank/sortkey columns are dropped for fzf.
struct Win {
    /// Sort tier: 1 waiting · 2 done · 3 blocker (the `@bn_agent`-over-`@bn_done` precedence).
    rank: u8,
    /// `@bn_agent_since` (epoch secs), ascending → longest-waiting first within a tier.
    sortkey: i64,
    display: String,
    window_id: String,
    name: String,
}

/// Time-in-state badge: `Ns` / `Nm` / `Nh`. Empty when `since` isn't an epoch-seconds integer
/// (mirrors the bash `dur()`); a clock skew into the future clamps to `0s`.
fn dur(since: &str, now: i64) -> String {
    let Ok(s) = since.parse::<i64>() else {
        return String::new();
    };
    let d = (now - s).max(0);
    if d < 60 {
        format!("{d}s")
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else {
        format!("{}h", d / 3600)
    }
}

/// Classify one `list-windows` row into a ranked [`Win`], or `None` to drop it (the current
/// window, a `working` agent, or an idle window with nothing flagged). Pure over its columns —
/// the parsing/sorting around it lives in [`window_rows`].
fn window_row(cols: &[&str], cur: &str, now: i64) -> Option<Win> {
    let g = |i: usize| cols.get(i).copied().unwrap_or("");
    let (wid, sess, idx, name, agent, done, blk, since) =
        (g(0), g(1), g(2), g(3), g(4), g(5), g(6), g(7));
    if wid.is_empty() || wid == cur {
        return None;
    }
    let mut d = String::new();
    // Rows go down a pipe to `fzf --ansi`; the fzf-less fallback below flattens them again.
    // The trailing spaces pad the tag column, so they stay outside the painted run.
    let ink = style::Ink::always();
    let (rank, tag): (u8, String) = match agent {
        "waiting" => {
            d = dur(since, now);
            (1, ink.warn(&format!("{} needs you", glyph::BELL)))
        }
        // Same rank as `waiting`: an agent that is blocked, wants a decision, or has died is
        // exactly what prefix-Tab exists to take you to. These fell through to `None`, which made
        // a stopped agent *unreachable* from the one key meant to reach it.
        "blocked" => {
            d = dur(since, now);
            (
                1,
                format!("{}  ", ink.danger(&format!("{} blocked", glyph::BAN))),
            )
        }
        "needs-decision" => {
            d = dur(since, now);
            (
                1,
                format!("{}   ", ink.warn(&format!("{} decide", glyph::EYE))),
            )
        }
        "failed" => {
            d = dur(since, now);
            (
                1,
                format!("{}   ", ink.danger(&format!("{} failed", glyph::BOLT))),
            )
        }
        // Paused on a usage cap — it resumes on its own, so it is not something to jump to.
        "limited" => return None,
        "done" => {
            d = dur(since, now);
            (
                2,
                format!("{}     ", ink.ok(&format!("{} done", glyph::CHECK))),
            )
        }
        "working" => return None,
        "" => {
            if done == "✓" {
                (
                    2,
                    format!("{}     ", ink.ok(&format!("{} done", glyph::CHECK))),
                )
            } else if !blk.is_empty() {
                (
                    3,
                    format!("{}  ", ink.danger(&format!("{} blocker", glyph::BAN))),
                )
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let sortkey = since.parse::<i64>().unwrap_or(0);
    let dd = if d.is_empty() {
        String::new()
    } else {
        format!("  {}", ink.meta(&d))
    };
    let display = format!("{tag}  {}  {name}{dd}", ink.meta(&format!("{sess}:{idx}")));
    Some(Win {
        rank,
        sortkey,
        display,
        window_id: wid.to_string(),
        name: name.to_string(),
    })
}

/// Source 1 — every window across every session that needs you, ranked (waiting > done >
/// blocker, then longest-waiting first), the current window excluded.
fn window_rows(now: i64, cur: &str) -> Vec<Win> {
    let fmt = "#{window_id}\t#{session_name}\t#{window_index}\t#{window_name}\t#{@bn_agent}\t#{@bn_done}\t#{@bn_blockers}\t#{@bn_agent_since}";
    let Some(out) = tmux_raw(&["list-windows", "-a", "-F", fmt]) else {
        return Vec::new();
    };
    let mut wins: Vec<Win> = out
        .lines()
        .filter_map(|line| window_row(&line.split('\t').collect::<Vec<_>>(), cur, now))
        .collect();
    wins.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.sortkey.cmp(&b.sortkey)));
    wins
}

/// The serde `snake_case` name for an [`ActionKind`] — the hidden `kind` column the preview /
/// dispatch switch on (matches `bn-attention.sh`/`tmux-attention-all.sh`'s string cases).
fn kind_str(k: ActionKind) -> &'static str {
    match k {
        ActionKind::Wip => "wip",
        ActionKind::Cron => "cron",
        ActionKind::Attn => "attn",
        ActionKind::Meeting => "meeting",
        ActionKind::Email => "email",
        ActionKind::Mention => "mention",
        ActionKind::Review => "review",
        ActionKind::Pr => "pr",
        ActionKind::Goal => "goal",
        ActionKind::Blocked => "blocked",
    }
}

/// Map a `bn next` action to a display row, matching `tmux-attention-all.sh`'s `bn_rows` glyphs
/// and colors (its own simpler set, distinct from the `bn next` human view's glyphs).
fn next_row(a: &Action) -> Row {
    let ink = style::Ink::always();
    let (tone, mark) = match a.kind {
        ActionKind::Cron => (style::Tone::Danger, "⚠"),
        ActionKind::Attn => (style::Tone::Warn, "•"),
        ActionKind::Review => (style::Tone::Danger, glyph::BAN),
        ActionKind::Pr => (style::Tone::Danger, "●"),
        ActionKind::Blocked => (style::Tone::Warn, "○"),
        _ => (style::Tone::Accent, "◷"),
    };
    let tail = if a.detail.is_empty() {
        String::new()
    } else {
        format!("  {}", ink.meta(&a.detail))
    };
    Row {
        display: format!("{}{tail}", ink.tone(tone, &format!("{mark} {}", a.title))),
        kind: kind_str(a.kind).to_string(),
        target: a.link.clone().unwrap_or_default(),
        title: a.title.clone(),
        id: a.attn_id.clone().unwrap_or_default(),
    }
}

/// Source 2 — the cross-repo `bn next` queue (already most-pressing first). Reads the warm cache
/// (`--cached` semantics) so the popup opens instantly; on a miss, computes once and warms it.
/// Momentum-re-ranked at read time (#246), same as `bn next`, so the picker leads with actions
/// on the repo/branch you're punched into.
fn next_rows(cfg: &Config) -> Vec<Row> {
    use bn_core::clock::Clock;
    let clock = bn_core::clock::RealClock;
    let today = clock.today();
    let now = clock.now_epoch_secs();
    let actions = bn_core::next::cached_or_compute(cfg, &today, now);
    let actions = bn_core::next::apply_momentum(cfg, actions, &today, now);
    actions.iter().map(next_row).collect()
}

/// `bn jump` — the interactive fzf popup. Streams rows in on `start` so the frame draws at once
/// instead of blocking on the `bn next` walk; falls back to a plain list when fzf is absent.
pub fn run(cfg: &Config) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());

    // The header (and the "desk is clear" message) fills in on `load`, once fzf knows the count.
    // {2}=kind {3}=target {4}=title. Enter returns the row; we dispatch below. ctrl-r reloads.
    let hdr = r#"[ "$FZF_TOTAL_COUNT" -eq 0 ] && printf "\033[1;32m✓ nothing needs you — desk is clear\033[0m" || printf "%d item(s) · ↵ go/open · ctrl-r reload" "$FZF_TOTAL_COUNT""#;
    let child = Command::new("fzf")
        .arg("--reverse")
        .arg("--ansi")
        .arg("--delimiter=\t")
        .arg("--with-nth=1")
        .arg(format!(
            "--preview={bn} jump --pick-stage preview {{2}} {{3}} {{4}} {{5}}"
        ))
        .arg("--preview-window=right:55%:wrap")
        .arg(format!("--bind=start:reload({bn} jump --pick-stage list)"))
        .arg(format!("--bind=load:transform-header:{hdr}"))
        .arg(format!("--bind=ctrl-r:reload({bn} jump --pick-stage list)"))
        .arg("--header=loading…")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(_) => {
            // No fzf on PATH — degrade to the plain list (display column only), then hold.
            // The rows were painted for fzf, so this is stdout again and has to obey NO_COLOR.
            use bn_core::clock::Clock;
            let now = bn_core::clock::RealClock.now_epoch_secs();
            let cur = tmux(&["display-message", "-p", "#{window_id}"]).unwrap_or_default();
            for w in window_rows(now, &cur) {
                println!("{}", style::strip_unless_enabled(&w.display));
            }
            for r in next_rows(cfg) {
                println!("{}", style::strip_unless_enabled(&r.display));
            }
            crate::picker::hold();
            return;
        }
    };

    let Ok(out) = child.wait_with_output() else {
        return;
    };
    let sel = String::from_utf8_lossy(&out.stdout).into_owned();
    let cols: Vec<&str> = sel.trim_end_matches('\n').split(TAB).collect();
    let kind = cols.get(1).copied().unwrap_or("");
    let target = cols.get(2).copied().unwrap_or("");
    let title = cols.get(3).copied().unwrap_or("");
    let id = cols.get(4).copied().unwrap_or("");
    if kind.is_empty() {
        return;
    }
    match kind {
        "window" => goto(target),
        "cron" => {
            let job = title.strip_prefix("fix cron ").unwrap_or(title);
            let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
            let out = Command::new(exe).args(["cron", "show", job]).output();
            crate::picker::page(&out.map(|o| o.stdout).unwrap_or_default());
        }
        // A plain flagged item with a repo/branch: Enter STARTS it — open its worktree window and
        // mark it in-flight (fzf has already closed, so the switch lands in the normal client). If
        // it has no locator (a link-only item), `start_attn_item` returns None → fall back to the
        // link. Other attn kinds (review/meeting/email) keep their open-link behaviour.
        "attn" if !id.is_empty() => {
            // A pending plan-review opens the plan in an nvim popup (`bn review edit`) for
            // decision-in-place, not a browser link — see `bn review popup` (#651).
            if crate::cmd::plan_review::open_plan_review_popup(id) {
                return;
            }
            if crate::wt::start_attn_item(cfg, id).is_none() {
                open_link_or_notify(target);
            }
        }
        _ => open_link_or_notify(target),
    }
}

/// Open `target` as a URL, or flash a tmux message when the action carries no link — the default
/// Enter action for a `bn next` row that isn't a startable item.
fn open_link_or_notify(target: &str) {
    if !target.is_empty() {
        crate::open_url(target);
    } else {
        let _ = tmux_run(&["display-message", "no link on this action"]);
    }
}

/// `bn jump --next` — the prefix-`Tab` fast-jump: switch straight to the top-ranked window with
/// no picker. Repeated presses cycle (each jump makes the prior window current, dropping it out).
pub fn next() {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let cur = tmux(&["display-message", "-p", "#{window_id}"]).unwrap_or_default();
    match window_rows(now, &cur).first() {
        Some(w) => goto(&w.window_id),
        None => {
            let _ = tmux_run(&["display-message", "✓ nothing needs you"]);
        }
    }
}

/// Dispatch a hidden `--pick-stage` callback. `fields` are the focused row's hidden columns as
/// fzf passed them: `[kind, target, title]`.
pub fn stage(cfg: &Config, stage: JumpStage, fields: &[String]) {
    match stage {
        JumpStage::List => {
            use bn_core::clock::Clock;
            let now = bn_core::clock::RealClock.now_epoch_secs();
            let cur = tmux(&["display-message", "-p", "#{window_id}"]).unwrap_or_default();
            for w in window_rows(now, &cur) {
                let row = Row {
                    display: w.display,
                    kind: "window".to_string(),
                    target: w.window_id,
                    title: w.name,
                    id: String::new(),
                };
                println!("{}", row.line());
            }
            for r in next_rows(cfg) {
                println!("{}", r.line());
            }
        }
        JumpStage::Preview => {
            let f = |i: usize| fields.get(i).map(String::as_str).unwrap_or("");
            preview(f(0), f(1), f(2), f(3));
        }
        JumpStage::SessionList => {
            use bn_core::clock::Clock;
            let now = bn_core::clock::RealClock.now_epoch_secs();
            for w in session_rows(now) {
                println!("{}", w.line());
            }
        }
    }
}

/// Detail panel for the focused row (the `preview` stage). Window rows show a live tail of the
/// active pane; `bn next` rows show the action's link / hint. `id` is the attn item id (for the
/// startable-vs-link hint).
fn preview(kind: &str, target: &str, title: &str, id: &str) {
    if kind == "window" {
        if let Some(pane) = active_pane(target) {
            if let Some(cap) = tmux_raw(&["capture-pane", "-p", "-J", "-t", &pane]) {
                let lines: Vec<&str> = cap.lines().collect();
                let start = lines.len().saturating_sub(40);
                for l in &lines[start..] {
                    println!("{l}");
                }
            }
        }
        return;
    }
    println!("{}\n", style::Ink::always().head(title));
    match kind {
        "cron" => {
            let job = title.strip_prefix("fix cron ").unwrap_or(title);
            crate::emit_cron_show(job, false);
        }
        "review" => println!(
            "{}\n\n↵ open the PR in your browser",
            if target.is_empty() { "no link" } else { target }
        ),
        "pr" => println!(
            "{}\n\n↵ open your PR in your browser",
            if target.is_empty() { "no link" } else { target }
        ),
        "attn" => {
            // A flagged item carrying a repo/branch is startable — Enter opens its worktree window
            // and marks it in-flight; otherwise Enter opens its link (if any).
            let startable = (!id.is_empty())
                .then(|| bn_core::attention::get(id))
                .flatten()
                .filter(|it| it.repo.is_some() && it.branch.is_some());
            match startable {
                Some(it) => {
                    let (r, b) = (it.repo.unwrap_or_default(), it.branch.unwrap_or_default());
                    let mode = it.mode.as_deref().unwrap_or("manual");
                    println!("{r}/{b}  ·  mode: {mode}\n\n↵ start — open the worktree window, mark in-flight");
                    if let Some(p) = &it.prompt {
                        println!("\nprompt:\n{p}");
                    }
                }
                None if target.is_empty() => println!("raised item — no link"),
                None => println!("{target}\n\n↵ open link"),
            }
        }
        "goal" => {
            if target.is_empty() {
                println!("today's goal — no link");
            } else {
                println!("{target}\n\n↵ open link");
            }
        }
        "blocked" => println!("a branch note with open blockers\n\nresolve it, then `bn done`"),
        _ => println!("select an action"),
    }
}

/// The active pane's id for a window (`window_id`), for the capture-pane preview.
fn active_pane(wid: &str) -> Option<String> {
    let out = tmux_raw(&["list-panes", "-t", wid, "-F", "#{pane_active} #{pane_id}"])?;
    out.lines().find_map(|l| {
        let (active, pane) = l.split_once(' ')?;
        (active == "1").then(|| pane.to_string())
    })
}

/// Switch the calling client to a window (ids are server-global): resolve its session, switch the
/// client there, then select the window.
fn goto(wid: &str) {
    if let Some(sess) = tmux(&["display-message", "-p", "-t", wid, "#{session_name}"]) {
        let _ = tmux_run(&["switch-client", "-t", &sess]);
    }
    let _ = tmux_run(&["select-window", "-t", wid]);
}

// ---- bn jump --session : the this-session window picker (prefix `) -------------------------------
//
// The session-scoped member of the jump family: `prefix Tab` jumps to whatever needs you next,
// while this one lists *every* window of the session you are in — worktrees with nothing flagged
// included — because
// "switch to the branch I was on" is navigation, not triage. Windows carry the same badges the tab
// bar draws (`@bn_blockers` / `@bn_todos` / `@bn_agent`), so the picker reads like the bar it
// replaces, with a live pane preview the bar can't show.
//
// Row order is what makes a bare Enter useful: the window you came from first (so ↵ toggles back),
// then the rest in tab order, then the window you are already on — Enter can never be a no-op.

/// Field layout of the `--session` picker's `list-windows -F` read, kept beside [`session_win`]
/// which indexes it.
const SESSION_FMT: &str = "#{window_id}\t#{window_index}\t#{window_name}\t#{window_active}\t#{window_last_flag}\t#{window_panes}\t#{@bn_agent}\t#{@bn_done}\t#{@bn_todos}\t#{@bn_blockers}\t#{@bn_agent_since}";

/// One window of the current session as the picker sees it, before the sort columns are dropped.
#[derive(Debug, Clone, PartialEq)]
struct SessWin {
    /// Sort tier: 0 the last-used window · 1 every other window · 2 the current one.
    tier: u8,
    /// `window_index`, the tab-bar order within a tier.
    index: i64,
    display: String,
    window_id: String,
}

impl SessWin {
    /// `display \t window_id` — the line fed to fzf; `{2}` is the preview/jump handle.
    fn line(&self) -> String {
        format!("{}{TAB}{}", self.display, self.window_id)
    }
}

/// The state badge for a session row, mirroring the tab bar's precedence: a live `@bn_agent` state
/// wins over `@bn_done` (the `bind m` silence-watch fallback). Empty when the window has nothing to
/// report — most of them, so a quiet session reads as a plain list of branches.
fn sess_state(agent: &str, done: &str, since: &str, now: i64) -> String {
    let ink = style::Ink::always();
    let d = dur(since, now);
    let age = if d.is_empty() {
        String::new()
    } else {
        format!(" {}", ink.meta(&d))
    };
    match agent {
        "waiting" => format!("{}{age}", ink.warn(&format!("{} needs you", glyph::BELL))),
        "working" => ink.meta(&format!("{} working{age}", glyph::GEAR)),
        "done" => format!("{}{age}", ink.ok(&format!("{} done", glyph::CHECK))),
        // `@bn_done` is the watch-fallback tick; it carries no timestamp of its own.
        _ if !done.is_empty() => ink.ok(&format!("{} done", glyph::CHECK)),
        _ => String::new(),
    }
}

/// The note badges the tab bar carries: open blockers (red) then open todos (dim), each omitted
/// when its window option is unset — the same lazily-written `@bn_blockers` / `@bn_todos` values
/// `bn tmux sync` stamps.
fn sess_badges(todos: &str, blockers: &str) -> String {
    let ink = style::Ink::always();
    let mut s = String::new();
    if !blockers.is_empty() {
        s.push_str(&format!(
            "  {}",
            ink.danger(&format!("{} {blockers}", glyph::BAN))
        ));
    }
    if !todos.is_empty() {
        s.push_str(&format!(
            "  {}",
            ink.meta(&format!("{} {todos}", glyph::TASKS))
        ));
    }
    s
}

/// Classify one `list-windows` row into a [`SessWin`], or `None` for an unusable row. Pure over its
/// columns — the tmux read and the sort live in [`session_rows`].
fn session_win(cols: &[&str], now: i64) -> Option<SessWin> {
    let g = |i: usize| cols.get(i).copied().unwrap_or("");
    let (wid, idx, name, active, last, panes) = (g(0), g(1), g(2), g(3), g(4), g(5));
    let (agent, done, todos, blockers, since) = (g(6), g(7), g(8), g(9), g(10));
    if wid.is_empty() {
        return None;
    }
    // The marker column answers "where am I / where was I" without reading the tab bar.
    let ink = style::Ink::always();
    let (tier, mark) = if active == "1" {
        (2, ink.meta("\u{25cf}")) // ● you are here
    } else if last == "1" {
        (0, ink.accent("\u{21a9}")) // ↩ the window you came from
    } else {
        (1, " ".to_string())
    };
    // Only worth saying when it's true; a single-pane window is the norm.
    let split = match panes.parse::<i64>() {
        Ok(n) if n > 1 => format!("  {}", ink.meta(&format!("{n} panes"))),
        _ => String::new(),
    };
    let state = sess_state(agent, done, since, now);
    // A quiet window contributes no state, badges or split, so trim the separators they would have
    // been introduced by rather than leaving the row padded out with trailing blanks.
    let display = format!(
        "{mark} {} {} {state}{}{split}",
        ink.meta(&format!("{idx:>2}")),
        ink.head(&format!("{name:<22}")),
        sess_badges(todos, blockers)
    )
    .trim_end()
    .to_string();
    Some(SessWin {
        tier,
        index: idx.parse().unwrap_or(i64::MAX),
        display,
        window_id: wid.to_string(),
    })
}

/// Every window of the *current* session, ordered last-used → tab order → current. `list-windows`
/// without `-t` resolves the session the same way [`run`]'s `display-message` does, so this reads
/// the invoking client's session even from inside a `display-popup`.
fn session_rows(now: i64) -> Vec<SessWin> {
    let Some(out) = tmux_raw(&["list-windows", "-F", SESSION_FMT]) else {
        return Vec::new();
    };
    let mut wins: Vec<SessWin> = out
        .lines()
        .filter_map(|l| session_win(&l.split(TAB).collect::<Vec<_>>(), now))
        .collect();
    wins.sort_by_key(|w| (w.tier, w.index));
    wins
}

/// `bn jump --session` — the interactive fzf picker over this session's windows. Enter jumps to the
/// highlighted window; ctrl-r re-reads the badges. Degrades to a plain list when fzf is absent.
pub fn session() {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let rows = session_rows(now);
    if rows.is_empty() {
        println!("no windows here — is this a tmux session?");
        crate::picker::hold();
        return;
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let header = format!(
        "this session · {} window(s) · ↵ jump · ctrl-r reload",
        rows.len()
    );

    // {2}=window_id. The preview reuses the `window` branch of the shared preview stage (a live
    // capture of the window's active pane). Enter accepts the row and closes; we jump below, once
    // fzf has torn down, so `select-window` lands in the real client and not in the popup.
    let child = Command::new("fzf")
        .arg("--ansi")
        .arg("--reverse")
        .arg("--with-nth=1")
        .arg("--delimiter=\t")
        .arg(format!(
            "--preview={bn} jump --pick-stage preview window {{2}}"
        ))
        .arg("--preview-window=right:55%:wrap")
        .arg(format!(
            "--bind=ctrl-r:reload({bn} jump --pick-stage session-list)"
        ))
        .arg(format!("--header={header}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(_) => {
            // No fzf on PATH — degrade to the plain list (display column only), then hold.
            for w in &rows {
                println!("{}", style::strip_unless_enabled(&w.display));
            }
            crate::picker::hold();
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // One line per window in a single session, so a synchronous write then read of the
        // selection can't deadlock: fzf drains stdin at load and only writes stdout on close.
        let body = rows
            .iter()
            .map(SessWin::line)
            .collect::<Vec<_>>()
            .join("\n");
        let _ = stdin.write_all(body.as_bytes());
    }

    let Ok(out) = child.wait_with_output() else {
        return;
    };
    let sel = String::from_utf8_lossy(&out.stdout);
    let wid = sel
        .trim_end_matches('\n')
        .split(TAB)
        .nth(1)
        .unwrap_or_default();
    if wid.is_empty() {
        return; // cancelled (Esc / ctrl-c) — nothing selected.
    }
    goto(wid);
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    /// One `list-windows` row in [`SESSION_FMT`] order, so a test names only the columns it cares
    /// about and leaves the rest empty — which is what tmux emits for an unset `@bn_*` option.
    #[derive(Default)]
    struct R<'a> {
        wid: &'a str,
        idx: &'a str,
        name: &'a str,
        active: &'a str,
        last: &'a str,
        panes: &'a str,
    }

    impl R<'_> {
        fn win(&self, now: i64) -> Option<SessWin> {
            session_win(
                &[
                    self.wid,
                    self.idx,
                    self.name,
                    self.active,
                    self.last,
                    self.panes,
                    "",
                    "",
                    "",
                    "",
                    "",
                ],
                now,
            )
        }
    }

    #[test]
    fn every_window_is_listed_even_with_nothing_flagged() {
        // The whole point of the session scope: unlike `bn jump`'s needs-you list, a quiet
        // worktree window still shows up, because switching to it is the job.
        let w = R {
            wid: "@4",
            idx: "3",
            name: "feat-auth",
            panes: "1",
            ..Default::default()
        }
        .win(NOW)
        .expect("a quiet window is still a row");
        assert_eq!(w.window_id, "@4");
        assert!(w.display.contains("feat-auth"));
    }

    #[test]
    fn a_row_with_no_window_id_is_dropped() {
        assert!(R {
            idx: "3",
            name: "x",
            panes: "1",
            ..Default::default()
        }
        .win(NOW)
        .is_none());
    }

    #[test]
    fn order_is_last_used_then_tab_order_then_current() {
        // What makes a bare Enter useful: it toggles back to where you came from, and can never
        // land on the window you are already looking at.
        let mut wins: Vec<SessWin> = [
            R {
                wid: "@1",
                idx: "1",
                name: "main",
                panes: "1",
                ..Default::default()
            },
            R {
                wid: "@2",
                idx: "2",
                name: "here",
                active: "1",
                panes: "1",
                ..Default::default()
            },
            R {
                wid: "@3",
                idx: "3",
                name: "prev",
                last: "1",
                panes: "1",
                ..Default::default()
            },
            R {
                wid: "@4",
                idx: "4",
                name: "other",
                panes: "1",
                ..Default::default()
            },
        ]
        .iter()
        .filter_map(|r| r.win(NOW))
        .collect();
        wins.sort_by_key(|w| (w.tier, w.index));
        let ids: Vec<&str> = wins.iter().map(|w| w.window_id.as_str()).collect();
        assert_eq!(ids, ["@3", "@1", "@4", "@2"]);
    }

    #[test]
    fn a_live_agent_state_outranks_the_silence_watch_tick() {
        // Same precedence the tab bar draws: @bn_agent over @bn_done.
        let s = sess_state("waiting", "✓", &(NOW - 90).to_string(), NOW);
        assert!(s.contains("needs you"), "{s}");
        assert!(s.contains("1m"), "the wait should carry its age: {s}");
        assert!(sess_state("", "✓", "", NOW).contains("done"));
        assert!(sess_state("", "", "", NOW).is_empty());
    }

    #[test]
    fn note_badges_appear_only_when_set() {
        assert!(sess_badges("", "").is_empty());
        assert!(sess_badges("3", "").contains(bn_core::glyph::TASKS));
        assert!(!sess_badges("3", "").contains(bn_core::glyph::BAN));
        let both = sess_badges("3", "1");
        // Blockers lead, as in the tab bar. Compare the glyph positions, not the digits — the
        // colour escapes (`\x1b[31m`) contain digits of their own.
        assert!(
            both.find(bn_core::glyph::BAN) < both.find(bn_core::glyph::TASKS),
            "{both:?}"
        );
    }

    #[test]
    fn a_split_window_says_so_and_a_single_pane_stays_quiet() {
        let row = |panes| {
            R {
                wid: "@1",
                idx: "1",
                name: "w",
                panes,
                ..Default::default()
            }
            .win(NOW)
            .unwrap()
            .display
        };
        assert!(row("3").contains("3 panes"), "{}", row("3"));
        assert!(!row("1").contains("panes"), "{}", row("1"));
    }
}
