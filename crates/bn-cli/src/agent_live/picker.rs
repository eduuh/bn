//! `bn fleet pick` — the interactive jump companion to the read-only `bn fleet` board.
//!
//! Split out of `agent_live.rs` for the 1000-line ratchet, along a seam that file already marked
//! with a banner of its own. The board *renders* the fleet; this *navigates* it, and the two share
//! only the view rows they both read.
//!
//! Reads tmux as the source of truth — no separate state store to drift: every live agent window
//! the fleet knows about, plus the orchestrator cockpit (the fleet/cockpit session, #345). fzf shows
//! the whole fleet with a live per-row pane preview; selecting a row jumps focus to that window
//! (`switch-client` inside tmux, `attach` from outside).
//!
//! The row-building core is a pure function over the fleet plus the tmux window list, so it stays
//! unit-testable without a tmux server.
use super::*;

/// How many trailing pane lines the `--preview` panel shows for the highlighted agent.
const PICK_PREVIEW_LINES: usize = 60;

/// One tmux window as the picker sees it: the server-global `window_id` (the stable jump/preview
/// handle), its raw-case `session`/`window` names, and `@bn_agent_since` (0 when undecorated).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TmuxWin {
    window_id: String,
    session: String,
    window: String,
    since: i64,
}

/// One picker row: the colored line fzf shows (field 1) plus the hidden `window_id`/`session`
/// columns its `--preview` and the post-close jump reference.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PickRow {
    display: String,
    window_id: String,
    session: String,
}

impl PickRow {
    /// `display \t window_id \t session` — the exact line fed to fzf; `{2}` is the preview/jump
    /// window id and `{3}` the session to switch/attach to.
    fn line(&self) -> String {
        format!("{}\t{}\t{}", self.display, self.window_id, self.session)
    }
}

/// A compact time-in-state badge: `Ns` / `Nm` / `Nh`. Empty when `since` is unset (`<= 0`); a
/// clock skew into the future clamps to `0s`. Mirrors `bn jump`'s `dur()`.
fn age_label(since: i64, now: i64) -> String {
    if since <= 0 {
        return String::new();
    }
    let d = (now - since).max(0);
    if d < 60 {
        format!("{d}s")
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else {
        format!("{}h", d / 3600)
    }
}

/// Read Plane A from tmux for the picker: one [`TmuxWin`] per window across every session, carrying
/// the `window_id` (so jump/preview are case- and rename-proof, unlike the lowercased `repo:branch`
/// key) alongside the raw names and `@bn_agent_since`.
pub(crate) fn read_pick_windows() -> Vec<TmuxWin> {
    let fmt = "#{window_id}\t#{session_name}\t#{window_name}\t#{@bn_agent_since}";
    let Some(out) = crate::proc::tmux_raw(&["list-windows", "-a", "-F", fmt]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 3 || f[0].is_empty() {
                return None;
            }
            Some(TmuxWin {
                window_id: f[0].to_string(),
                session: f[1].to_string(),
                window: f[2].to_string(),
                since: f.get(3).and_then(|s| s.parse().ok()).unwrap_or(0),
            })
        })
        .collect()
}

/// A fleet agent row: `<state glyph>  repo:branch  harness  age  tokens`. `w` supplies the live
/// window handle (for jump/preview) and its time-in-state; `v` supplies the ranked state, harness
/// and token burn — the same signals the board renders.
fn agent_pick_row(v: &LiveView, w: &TmuxWin, now: i64) -> PickRow {
    let (color, mark) = board_style(v);
    let harness = v.harness.as_deref().unwrap_or("?");
    let age = age_label(w.since, now);
    let age_col = if age.is_empty() {
        String::new()
    } else {
        format!("  \x1b[2m{age}\x1b[0m")
    };
    let tok = match v.tokens {
        Some(t) if t > 0 => format!("  \x1b[2m{} tok\x1b[0m", fmt_tokens(t)),
        _ => String::new(),
    };
    let display = format!(
        "\x1b[{color}m{mark}\x1b[0m  \x1b[1m{}\x1b[0m  \x1b[2m{harness}\x1b[0m{age_col}{tok}",
        v.target
    );
    PickRow {
        display,
        window_id: w.window_id.clone(),
        session: w.session.clone(),
    }
}

/// An orchestrator cockpit row (a cockpit-session window, #345): a distinct gear glyph so it
/// reads apart from the agents it drives, `session:window`, and its age.
fn orchestrator_pick_row(w: &TmuxWin, now: i64) -> PickRow {
    let age = age_label(w.since, now);
    let age_col = if age.is_empty() {
        String::new()
    } else {
        format!("  \x1b[2m{age}\x1b[0m")
    };
    let display = format!(
        "\x1b[1;36m\u{f085} orchestrator\x1b[0m  \x1b[1m{}:{}\x1b[0m{age_col}",
        w.session, w.window
    );
    PickRow {
        display,
        window_id: w.window_id.clone(),
        session: w.session.clone(),
    }
}

/// Build the ordered picker rows from the joined fleet + the live tmux window list. Pure over its
/// inputs → unit-testable without a tmux server.
///
/// 1. Every fleet agent that currently has a live window, ranked by who needs you (the board's
///    order). A fleet row whose window has since vanished (`has_window` was set but no live match)
///    is dropped — you can't jump to a gone window.
/// 2. Every orchestrator cockpit window (in the fleet/cockpit session) not already shown as an agent.
pub(crate) fn build_pick_rows(
    views: &[LiveView],
    wins: &[TmuxWin],
    now: i64,
    cockpit: &str,
) -> Vec<PickRow> {
    build_pick_rows_scoped(views, wins, now, false, cockpit)
}

/// A dispatched agent's worktree branch — `bn dispatch` / `bn orchestrate` name it `dispatch/<slug>`
/// (see `cmd::dispatch`). The `bn fleet pick --dispatched` scope keys off this prefix.
fn is_dispatch_branch(branch: &str) -> bool {
    branch.starts_with("dispatch/")
}

/// [`build_pick_rows`] with a `dispatched` scope: when set, keep only dispatched agent windows
/// (`dispatch/*` branches) and drop the orchestrator cockpit rows — the `bn fleet pick --dispatched`
/// view. `cockpit` is the resolved cockpit session name ([`wt::orchestrator_session`]). Pure over
/// its inputs → unit-testable without a tmux server.
fn build_pick_rows_scoped(
    views: &[LiveView],
    wins: &[TmuxWin],
    now: i64,
    dispatched: bool,
    cockpit: &str,
) -> Vec<PickRow> {
    let winmap: BTreeMap<String, &TmuxWin> = wins
        .iter()
        .map(|w| (key(&w.session, &w.window), w))
        .collect();

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut rows = Vec::new();

    // 1) Fleet agents with a live window, ranked (needs-you first), matched on the lowercased
    //    `repo:branch` key both the fleet target and the window key share. The `--dispatched` scope
    //    narrows this to `dispatch/*` branches.
    let mut agents: Vec<(&LiveView, &TmuxWin)> = views
        .iter()
        .filter(|v| !dispatched || is_dispatch_branch(&v.branch))
        .filter_map(|v| winmap.get(v.target.as_str()).map(|w| (v, *w)))
        .collect();
    agents.sort_by_key(|(v, _)| board_rank(v));
    for (v, w) in agents {
        rows.push(agent_pick_row(v, w, now));
        seen.insert(w.window_id.as_str());
    }

    // 2) Orchestrator cockpit windows not already surfaced as an agent row — omitted under the
    //    `--dispatched` scope, which is agents-only.
    if !dispatched {
        for w in wins.iter().filter(|w| {
            w.session.eq_ignore_ascii_case(cockpit) && !seen.contains(w.window_id.as_str())
        }) {
            rows.push(orchestrator_pick_row(w, now));
        }
    }

    rows
}

/// `bn fleet pick` — the interactive fzf picker over the whole live fleet + the orchestrator
/// cockpit. Each row previews a live `tmux capture-pane` of its window; selecting one jumps focus
/// there. Degrades to the read-only board when fzf is absent, and no-ops cleanly on an empty fleet.
pub fn run_pick(cfg: &Config, idle: i64, stuck: i64, dispatched: bool) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let views = collect(cfg, idle, stuck);
    let rows = build_pick_rows_scoped(
        &views,
        &read_pick_windows(),
        now,
        dispatched,
        crate::wt::orchestrator_session(),
    );
    if rows.is_empty() {
        println!(
            "{}",
            if dispatched {
                "No live dispatched agents to jump to."
            } else {
                "No live agents or orchestrator windows to jump to."
            }
        );
        return;
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let scope = if dispatched { " · dispatched" } else { "" };
    let header = format!(
        "fleet{scope} · {} window(s) · ↵ jump · live preview · ctrl-c cancel",
        rows.len()
    );

    // {2}=window_id (the preview/jump handle). Enter accepts the row (default bind → prints it &
    // closes); we jump below, once fzf has torn down, so `switch-client`/`attach` land in the real
    // client instead of the fzf popup.
    let child = Command::new("fzf")
        .arg("--ansi")
        .arg("--reverse")
        .arg("--with-nth=1")
        .arg("--delimiter=\t")
        .arg(format!(
            "--preview={bn} fleet pick --pick-stage preview {{2}}"
        ))
        .arg("--preview-window=right:60%:wrap")
        .arg(format!("--header={header}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(_) => {
            // No fzf on PATH — degrade to the read-only board (the same rows, non-interactive).
            run_board(cfg, idle, stuck, None, false);
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // The fleet is small (one line per live window), so a synchronous write then read of the
        // selection can't deadlock: fzf drains stdin at load and only writes stdout on close.
        let body = rows
            .iter()
            .map(PickRow::line)
            .collect::<Vec<_>>()
            .join("\n");
        let _ = stdin.write_all(body.as_bytes());
    }

    let Ok(out) = child.wait_with_output() else {
        return;
    };
    let sel = String::from_utf8_lossy(&out.stdout);
    let cols: Vec<&str> = sel.trim_end_matches('\n').split('\t').collect();
    let window_id = cols.get(1).copied().unwrap_or("");
    let session = cols.get(2).copied().unwrap_or("");
    if window_id.is_empty() {
        return; // cancelled (Esc / ctrl-c) — nothing selected.
    }
    pick_jump(session, window_id);
}

/// The `--pick-stage preview` callback: a live tail of the focused window's active pane, captured
/// fresh on every fzf selection change (so the preview is genuinely live, not a snapshot).
pub fn pick_stage(stage: FleetPickStage, window_id: Option<&str>) {
    match stage {
        FleetPickStage::Preview => {
            let Some(wid) = window_id.filter(|w| !w.is_empty()) else {
                return;
            };
            if let Some(cap) = crate::proc::tmux_raw(&["capture-pane", "-p", "-t", wid]) {
                let lines: Vec<&str> = cap.lines().collect();
                let start = lines.len().saturating_sub(PICK_PREVIEW_LINES);
                for l in &lines[start..] {
                    println!("{l}");
                }
            }
        }
    }
}

/// Jump focus to a window: inside tmux, switch the client to its session and select it; outside
/// tmux, make it the session's current window and attach the terminal. `window_id` is server-global
/// so both paths land exactly on the chosen window regardless of case or later renames.
fn pick_jump(session: &str, window_id: &str) {
    use crate::proc::tmux_run;
    if std::env::var_os("TMUX").is_some() {
        if !session.is_empty() {
            let _ = tmux_run(&["switch-client", "-t", session]);
        }
        let _ = tmux_run(&["select-window", "-t", window_id]);
    } else {
        // No client yet: set the target as its session's current window, then attach to it.
        let _ = tmux_run(&["select-window", "-t", window_id]);
        if !session.is_empty() {
            let _ = tmux_run(&["attach-session", "-t", session]);
        }
    }
}

/// The accelerator-key pool for the single-key jumper (#633), in press order: `1-9` (numeric is
/// fastest one-handed on a phone), then `a-z`. `q` is omitted — tmux's `display-menu` reserves it
/// (with Esc) to cancel, so binding it would be shadowed.
fn menu_accelerators() -> impl Iterator<Item = char> {
    "123456789abcdefghijklmnoprstuvwxyz".chars()
}

/// One `display-menu` entry: the accelerator `key`, the plain `name` shown, and the `session` +
/// `window_id` its select command jumps to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MenuEntry {
    pub(crate) key: char,
    pub(crate) name: String,
    pub(crate) session: String,
    pub(crate) window_id: String,
}

/// Strip ANSI SGR escape sequences (`\x1b[…m`) from `s` — the picker rows carry color codes for
/// fzf/`--ansi`, but `tmux display-menu` names are plain text (raw escapes render as garbage).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            for e in chars.by_ref() {
                if e.is_ascii_alphabetic() {
                    break; // SGR terminator (e.g. 'm')
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Build the ordered `display-menu` entries from the picker rows: assign each row the next
/// accelerator key (see [`menu_accelerators`]) and a plain name, preserving the needs-you-first
/// order. Rows beyond the accelerator pool are dropped (a fleet that large wants `bn fleet pick`).
/// Pure over its inputs → unit-testable without a tmux server.
pub(crate) fn build_menu_entries(rows: &[PickRow]) -> Vec<MenuEntry> {
    rows.iter()
        .zip(menu_accelerators())
        .map(|(r, key)| MenuEntry {
            key,
            name: strip_ansi(&r.display),
            session: r.session.clone(),
            window_id: r.window_id.clone(),
        })
        .collect()
}

/// The `tmux display-menu` argv for these entries: `-T <title>`, centered, then a `name key
/// command` triple per entry. Selecting an entry runs `switch-client -t <session> ; select-window
/// -t <window_id>` so focus lands on the chosen window. When `search_cmd` is set (#633 P2), a
/// trailing `/` entry is appended that runs it — the search-overlay hook that drops into the fzf
/// `bn fleet pick` inside a `display-popup`. Pure over its inputs → unit-testable.
fn menu_args(entries: &[MenuEntry], title: &str, search_cmd: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "display-menu".into(),
        "-T".into(),
        title.into(),
        "-x".into(),
        "C".into(),
        "-y".into(),
        "C".into(),
    ];
    for e in entries {
        let cmd = if e.session.is_empty() {
            format!("select-window -t {}", e.window_id)
        } else {
            format!(
                "switch-client -t {} ; select-window -t {}",
                e.session, e.window_id
            )
        };
        args.push(e.name.clone());
        args.push(e.key.to_string());
        args.push(cmd);
    }
    if let Some(cmd) = search_cmd {
        args.push("search /".into());
        args.push("/".into());
        args.push(cmd.into());
    }
    args
}

/// The `display-menu` command a `/` entry runs (#633 P2): open the fzf `bn fleet pick` inside a
/// centered `display-popup` so you can type a few chars (repo / branch / issue#) to filter the
/// fleet on a phone. `-E` closes the popup when the picker exits (after it has jumped). `bn` is the
/// already-shell-quoted path to this executable. Pure over its input → unit-testable.
fn search_overlay_cmd(bn: &str) -> String {
    format!("display-popup -w 90% -h 80% -E {bn} fleet pick")
}

/// How a `bn fleet jump <query>` resolves against the live picker rows.
#[derive(Debug, Clone, PartialEq)]
enum JumpResolution {
    /// No row matched the query — nothing to jump to.
    None,
    /// Exactly one row matched → jump straight to its window.
    One { session: String, window_id: String },
    /// Several rows matched → too ambiguous to auto-jump; carries their plain names to show.
    Many(Vec<String>),
}

/// Case-insensitively filter picker rows by a free-text `query` matched as a substring of each
/// row's plain (ANSI-stripped) display text — repo, branch, issue#, harness and state are all
/// searchable, so a numeric issue# is the fastest one-handed filter. An empty query returns every
/// row. Pure over its inputs → unit-testable without a tmux server.
fn filter_pick_rows<'a>(rows: &'a [PickRow], query: &str) -> Vec<&'a PickRow> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return rows.iter().collect();
    }
    rows.iter()
        .filter(|r| strip_ansi(&r.display).to_ascii_lowercase().contains(&q))
        .collect()
}

/// Resolve a `bn fleet jump <query>` to a single window: filter the rows, then require a unique
/// match. Zero matches → [`JumpResolution::None`]; exactly one → [`JumpResolution::One`] with its
/// session + window id; more than one → [`JumpResolution::Many`] carrying the candidate names so
/// the caller can list them instead of guessing. Pure over its inputs → unit-testable.
fn resolve_jump(rows: &[PickRow], query: &str) -> JumpResolution {
    let matches = filter_pick_rows(rows, query);
    match matches.as_slice() {
        [] => JumpResolution::None,
        [r] => JumpResolution::One {
            session: r.session.clone(),
            window_id: r.window_id.clone(),
        },
        many => JumpResolution::Many(many.iter().map(|r| strip_ansi(&r.display)).collect()),
    }
}

/// `bn fleet jump` — the single-key `display-menu` jumper (#633): a native tmux popup, one entry
/// per live agent window plus the orchestrator cockpit, each with an accelerator key + state badge,
/// and a trailing `/` search entry that drops into the fzf `bn fleet pick` in a `display-popup`
/// (#633 P2). Press the key → focus jumps to that window. Zero typing, big tap targets — the
/// mobile/one-handed companion to the fzf-based `bn fleet pick`. Falls back to the read board when
/// there's no tmux client to raise the menu in.
pub fn run_menu_jump(cfg: &Config, idle: i64, stuck: i64) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let views = collect(cfg, idle, stuck);
    let rows = build_pick_rows(
        &views,
        &read_pick_windows(),
        now,
        crate::wt::orchestrator_session(),
    );
    if rows.is_empty() {
        println!("No live agents or orchestrator windows to jump to.");
        return;
    }
    // display-menu needs a client to raise the popup in; outside tmux, degrade to the board.
    if std::env::var_os("TMUX").is_none() {
        println!("\x1b[2m(no tmux client — attach first; showing the board)\x1b[0m");
        run_board(cfg, idle, stuck, None, false);
        return;
    }
    let entries = build_menu_entries(&rows);
    let title = format!("fleet · {} · press a key or / to search", entries.len());
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let search = search_overlay_cmd(&bn);
    crate::proc::tmux_run(&menu_args(&entries, &title, Some(&search)));
}

/// `bn fleet jump <issue|repo>` — direct numeric/name jump (#633 P2): resolve `query` against the
/// live fleet + orchestrator rows and jump straight to the one window it names (no menu, no fzf).
/// A numeric issue# or a repo/branch fragment is the fastest one-handed jump on a phone. No match
/// or an ambiguous one prints the candidates instead of guessing. Reuses the same rows and
/// `window_id` mapping as the menu jumper, so behavior stays consistent.
pub fn run_direct_jump(cfg: &Config, idle: i64, stuck: i64, query: &str) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    let views = collect(cfg, idle, stuck);
    let rows = build_pick_rows(
        &views,
        &read_pick_windows(),
        now,
        crate::wt::orchestrator_session(),
    );
    if rows.is_empty() {
        println!("No live agents or orchestrator windows to jump to.");
        return;
    }
    match resolve_jump(&rows, query) {
        JumpResolution::One { session, window_id } => pick_jump(&session, &window_id),
        JumpResolution::None => {
            println!(
                "No fleet window matches '{query}'. Try `bn fleet jump` (menu) or `bn fleet pick`."
            );
        }
        JumpResolution::Many(names) => {
            println!("'{query}' matches {} windows — narrow it:", names.len());
            for n in &names {
                println!("  {n}");
            }
        }
    }
}
