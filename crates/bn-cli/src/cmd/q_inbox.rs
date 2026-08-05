//! `bn q` — quick-capture inbox + focus-block drain.
use crate::*;

/// A one-line tally of a drained queue by kind, e.g. "2 PRs to review, 1 goal, 3 captures".
pub(crate) fn drain_summary(queue: &[bn_core::next::Action], captures: usize) -> String {
    use bn_core::next::ActionKind;
    let count = |k: ActionKind| queue.iter().filter(|a| a.kind == k).count();
    let mut parts = Vec::new();
    let reviews = count(ActionKind::Review);
    if reviews > 0 {
        parts.push(format!("{reviews} PR{} to review", plural(reviews)));
    }
    let prs = count(ActionKind::Pr);
    if prs > 0 {
        parts.push(format!("{prs} PR{} to chase", plural(prs)));
    }
    let goals = count(ActionKind::Goal);
    if goals > 0 {
        parts.push(format!("{goals} goal{}", plural(goals)));
    }
    let wip = count(ActionKind::Wip);
    if wip > 0 {
        parts.push("WIP guard".to_string());
    }
    if captures > 0 {
        parts.push(format!("{captures} capture{}", plural(captures)));
    }
    if parts.is_empty() {
        format!("{} item{}", queue.len(), plural(queue.len()))
    } else {
        parts.join(", ")
    }
}

/// `bn q "<text>"` — bank a thought in the global inbox, stamped with the time and a
/// git-free repo/branch hint. The fast path: one file write, no subprocess, works anywhere.
pub(crate) fn emit_q_capture(cfg: &Config, text: &str, as_json: bool) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let hint = resolver::resolve_hint(cfg, &cwd);
    // `HH:MM` tail of the "YYYY-MM-DD HH:MM" stamp.
    let at = bn_core::util::now_stamp()
        .split(' ')
        .nth(1)
        .map(str::to_string);
    match bn_core::inbox::capture(cfg, text, hint.as_ref(), &today(), at) {
        Ok(o) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success("q", payload::q_capture_data(&o)).to_json()
                );
            } else {
                match (&o.repo, &o.branch) {
                    (Some(r), Some(b)) => println!("⚡ banked {} ({r}/{b})", o.id),
                    _ => println!("⚡ banked {}", o.id),
                }
            }
        }
        Err(e) => emit_err("q", &e, as_json),
    }
}

/// Bare `bn q` — list the inbox.
pub(crate) fn emit_q_list(cfg: &Config, as_json: bool) {
    let items = bn_core::inbox::read(cfg);
    if as_json {
        println!(
            "{}",
            Envelope::success("q", payload::q_list_data(&items)).to_json()
        );
        return;
    }
    if items.is_empty() {
        println!("Inbox empty ✓");
        return;
    }
    for i in &items {
        println!("{}", q_line(i));
    }
    println!(
        "({} capture{} — bn q drain to triage)",
        items.len(),
        plural(items.len())
    );
}

/// One inbox line: `q1  check attn ingest  (bn/feat-x · 2026-07-03 09:41)`.
pub(crate) fn q_line(i: &bn_core::inbox::InboxItem) -> String {
    let mut meta = Vec::new();
    if let (Some(r), Some(b)) = (&i.repo, &i.branch) {
        meta.push(format!("{r}/{b}"));
    }
    let mut when = i.created.clone();
    if let Some(at) = &i.at {
        when.push(' ');
        when.push_str(at);
    }
    meta.push(when);
    format!("{:<4} {}  ({})", i.id, i.text, meta.join(" · "))
}

/// `bn q drain [--auto]` — triage the inbox. `--auto` routes only the unambiguous captures;
/// without it each item is walked interactively (needs a tty).
pub(crate) fn emit_q_drain(cfg: &Config, auto: bool, as_json: bool) {
    use std::io::IsTerminal;
    if auto {
        match bn_core::inbox::drain_auto(cfg, &today()) {
            Ok(out) => {
                if as_json {
                    println!(
                        "{}",
                        Envelope::success("q_drain", payload::q_drain_data(&out.routed, out.left))
                            .to_json()
                    );
                    return;
                }
                for r in &out.routed {
                    println!("⚡ {} → {}", r.text, r.dest);
                }
                match out.left {
                    0 => println!("Inbox empty ✓"),
                    n => println!("({n} capture{} left — bn q drain to triage)", plural(n)),
                }
            }
            Err(e) => emit_err("q_drain", &e, as_json),
        }
        return;
    }
    if as_json {
        fail("bn q drain: interactive triage has no --json; use --auto");
    }
    if !std::io::stdin().is_terminal() {
        fail("bn q drain: not a tty — use bn q drain --auto");
    }
    interactive_drain(cfg);
}

/// Per-item triage loop. Each sink write lands before the capture leaves the inbox, so
/// quitting (or ctrl-C) mid-drain loses nothing.
pub(crate) fn interactive_drain(cfg: &Config) {
    use std::io::Write;
    let items = bn_core::inbox::read(cfg);
    if items.is_empty() {
        println!("Inbox empty ✓");
        return;
    }
    let today = today();
    let mut routed = 0usize;
    'items: for item in &items {
        println!("\n{}", q_line(item));
        let hint = match (&item.repo, &item.branch) {
            (Some(r), Some(b)) => Some(Resolved {
                repo: r.clone(),
                branch: b.clone(),
            }),
            _ => None,
        };
        let prompt = match &hint {
            Some(h) => format!(
                "  [t]odo {}/{}  [g]oal today  [a]ttn  [r]esearch  [d]rop  [s]kip  [q]uit > ",
                h.repo, h.branch
            ),
            None => "  [t]odo …  [g]oal today  [a]ttn  [r]esearch …  [d]rop  [s]kip  [q]uit > "
                .to_string(),
        };
        loop {
            print!("{prompt}");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
                break 'items; // EOF — stop triaging, keep what's left
            }
            use bn_core::inbox::Route;
            let to = match line.trim().chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('t') => match drain_target(cfg, &hint) {
                    Some(ctx) => Route::Todo(ctx),
                    None => continue,
                },
                Some('r') => match drain_target(cfg, &hint) {
                    Some(ctx) => Route::Research(ctx),
                    None => continue,
                },
                Some('g') => Route::Goal,
                Some('a') => Route::Attn,
                Some('d') => Route::Drop,
                Some('s') => continue 'items,
                Some('q') => break 'items,
                _ => continue,
            };
            match bn_core::inbox::route(cfg, &item.id, &to, &today) {
                Ok(r) => {
                    println!("  → {}", r.dest);
                    routed += 1;
                }
                Err(e) => println!("  ! {}", e.message()),
            }
            continue 'items;
        }
    }
    let left = bn_core::inbox::read(cfg).len();
    match (routed, left) {
        (_, 0) => println!("\nInbox empty ✓"),
        (n, l) => println!("\n({n} routed, {l} left)"),
    }
}

/// The branch-note target for an interactive todo/research route: the capture's hint, or an
/// explicit `repo branch` read from the prompt when there is none. `None` re-prompts.
pub(crate) fn drain_target(cfg: &Config, hint: &Option<Resolved>) -> Option<Resolved> {
    use std::io::Write;
    if let Some(h) = hint {
        return Some(h.clone());
    }
    print!("  repo branch > ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (Some(repo), Some(branch)) = (parts.next(), parts.next()) else {
        println!("  need: <repo> <branch>");
        return None;
    };
    resolver::resolve_target(
        cfg,
        std::path::Path::new("."),
        Some(repo.to_string()),
        Some(branch.to_string()),
    )
    .ok()
}

pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Render a duration in seconds as a compact human label (`90m` → `1h30m`, `45m`, `2h`).
pub(crate) fn fmt_dur(secs: i64) -> String {
    let mins = secs / 60;
    let (h, m) = (mins / 60, mins % 60);
    match (h, m) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h{m}m"),
    }
}
