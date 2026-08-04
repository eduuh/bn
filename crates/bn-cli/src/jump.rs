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
use std::process::{Command, Stdio};

use crate::proc::{tmux, tmux_raw, tmux_run};

/// The hidden callback stages a `jump` fzf session re-invokes (`bn jump --pick-stage <s>`).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum JumpStage {
    /// Emit the unified row list (the `start`/`ctrl-r` reload feed).
    List,
    /// Render the detail panel for the focused row.
    Preview,
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
    let (rank, tag): (u8, &str) = match agent {
        "waiting" => {
            d = dur(since, now);
            (1, "\x1b[1;33m\u{f0f3} needs you\x1b[0m")
        }
        "done" => {
            d = dur(since, now);
            (2, "\x1b[32m\u{f00c} done\x1b[0m     ")
        }
        "working" => return None,
        "" => {
            if done == "✓" {
                (2, "\x1b[32m\u{f00c} done\x1b[0m     ")
            } else if !blk.is_empty() {
                (3, "\x1b[31m\u{f05e} blocker\x1b[0m  ")
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
        format!("  \x1b[2m{d}\x1b[0m")
    };
    let display = format!("{tag}  \x1b[2m{sess}:{idx}\x1b[0m  {name}{dd}");
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
    let (color, glyph) = match a.kind {
        ActionKind::Cron => ("1;31", "⚠"),
        ActionKind::Attn => ("33", "•"),
        ActionKind::Review => ("1;31", bn_core::glyph::BAN),
        ActionKind::Pr => ("31", "●"),
        ActionKind::Blocked => ("33", "○"),
        _ => ("36", "◷"),
    };
    let tail = if a.detail.is_empty() {
        String::new()
    } else {
        format!("  \x1b[2m{}\x1b[0m", a.detail)
    };
    Row {
        display: format!("\x1b[{color}m{glyph} {}\x1b[0m{tail}", a.title),
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
            use bn_core::clock::Clock;
            let now = bn_core::clock::RealClock.now_epoch_secs();
            let cur = tmux(&["display-message", "-p", "#{window_id}"]).unwrap_or_default();
            for w in window_rows(now, &cur) {
                println!("{}", w.display);
            }
            for r in next_rows(cfg) {
                println!("{}", r.display);
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
    }
}

/// Detail panel for the focused row (the `preview` stage). Window rows show a live tail of the
/// active pane; `bn next` rows show the action's link / hint. `id` is the attn item id (for the
/// startable-vs-link hint).
fn preview(kind: &str, target: &str, title: &str, id: &str) {
    if kind == "window" {
        if let Some(pane) = active_pane(target) {
            if let Some(cap) = tmux_raw(&["capture-pane", "-p", "-t", &pane]) {
                let lines: Vec<&str> = cap.lines().collect();
                let start = lines.len().saturating_sub(40);
                for l in &lines[start..] {
                    println!("{l}");
                }
            }
        }
        return;
    }
    println!("\x1b[1;36m{title}\x1b[0m\n");
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


