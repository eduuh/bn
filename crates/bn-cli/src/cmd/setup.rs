//! `bn glyphs` and the tmux statusline integration.
use crate::*;

/// `bn glyphs` — render the whole icon vocabulary with names + codepoints, then a mock of the
/// one-row tmux status bar. A quick font check (tofu means your terminal font lacks the
/// glyph) and the canonical reference when syncing the tmux.conf / shell / nvim mirrors.
pub(crate) fn emit_glyphs() {
    use bn_core::glyph as g;
    let rows: &[(&str, &str, &str)] = &[
        (g::BELL, "BELL", "agent/item waiting on you"),
        (g::CHECK, "CHECK", "done"),
        (g::BAN, "BAN", "blocked / blocker / review holding you"),
        (g::GEAR, "GEAR", "working / auto mode"),
        (g::EYE, "EYE", "WAITING ON YOU section"),
        (g::CALENDAR, "CALENDAR", "TODAY / meetings"),
        (g::BOLT, "BOLT", "BREAKING section"),
        (g::TASKS, "TASKS", "YOUR WORK section"),
        (g::ENVELOPE, "ENVELOPE", "email"),
        (g::COMMENT, "COMMENT", "Teams/chat mention"),
        (g::PULL_REQUEST, "PULL_REQUEST", "a PR"),
        (g::TARGET, "TARGET", "focus block"),
        (g::AWAY, "AWAY", "afk mode"),
        (g::PENCIL, "PENCIL", "open todos count"),
        (g::CLOCK, "CLOCK", "SLA / wait time / cron"),
    ];
    println!("bn icon vocabulary — Nerd Font PUA, tinted by ANSI color\n");
    for (glyph, name, meaning) in rows {
        let cp = glyph.chars().next().map(|c| c as u32).unwrap_or(0);
        println!(
            "  {glyph}  {name:<13} U+{cp:04X}  {}",
            style::meta(meaning)
        );
    }
    println!("\nstatus-row mock (how the top bar reads):");
    // A picture of tmux's own status bar rather than bn's output vocabulary, so it interleaves
    // short runs and needs one thing the palette deliberately has no verb for: reverse video,
    // which is how tmux marks the active window. Both come off the same `ink`, so the mock still
    // goes plain under NO_COLOR like every other row.
    let ink = style::Ink::auto();
    let (r, y, gr, d) = (
        ink.open(style::Tone::Danger),
        ink.open(style::Tone::Warn),
        ink.open(style::Tone::Ok),
        ink.open(style::Tone::Meta),
    );
    let n = ink.close();
    let inv = if ink.is_on() { "\x1b[7m" } else { "" };
    println!(
        "  {inv} bn {n}  1   2 {r}{}1{n} {d}{}2{n} {y}{}{n}   3 {gr}{}{n}   {d}│ ⎇ glyph-module {gr}●{n} {d}│{n} {y}{}3 {}1{n} {d}│{n} {y}{}{n} {d}│ 14:07 Sat 05{n}",
        g::BAN,
        g::PENCIL,
        g::BELL,
        g::CHECK,
        g::PENCIL,
        g::BAN,
        g::BELL,
    );
    println!(
        "\n  tabs are the window NUMBER + badges — no branch name; the git segment already names\n  \
         the focused branch. The goal, attention, queue, cron and bn-web left the bar; the badges\n  \
         went to `bn tat`'s header."
    );
    println!(
        "\ntofu above? your terminal font isn't a Nerd Font — mirrors: see bn-core/src/glyph.rs"
    );
}

/// Status badge for the tmux status bar. Single source of truth: resolves repo/branch +
/// work-classification through bn-core (no zsh re-implementation). Silent when there's no
/// resolvable note or it's closed, so the bar stays clean on non-repo dirs.
pub(crate) fn emit_tmux_status(cfg: &Config, path: Option<String>) {
    let dir = path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let Ok(ctx) = resolver::resolve(cfg, &dir) else {
        return;
    };
    let Ok(note) = bn_core::store::read(&cfg.note_file(&ctx)) else {
        return;
    };
    if note.front.status == "closed" {
        return;
    }
    let todos = note.front.todos.iter().filter(|t| t.is_open_todo()).count();
    let blockers = note.front.blockers.len();
    let mut out = String::new();
    if todos > 0 {
        out.push_str(&format!(" \u{f040}{todos}"));
    }
    if blockers > 0 {
        out.push_str(&format!(" \u{f05e}{blockers}"));
    }
    if !out.is_empty() {
        print!("{out}");
    }
}

/// Push the current note's counts into per-window tmux options so `window-status-format`
/// can decorate without spawning bn each refresh. No-op outside tmux (`$TMUX` unset — e.g.
/// native Windows). Counts are stored EMPTY when zero so tmux `#{?@bn_*,…}` conditionals
/// (which test emptiness, where "0" would read as truthy) stay quiet on a clean note.
pub(crate) fn sync_tmux(cfg: &Config, ctx: &Resolved) {
    if std::env::var_os("TMUX").is_none() {
        return;
    }
    // This decorates the window the process is running in, but `ctx` can be any note — `bn done
    // --repo x --branch y` resolves elsewhere and would otherwise stamp *this* window with that
    // note's counts. Once the window carries its own identity (see `bn tmux-reconcile`), we can
    // tell the two apart and decline; a window with no identity yet behaves exactly as before.
    let (win_repo, win_branch) = (
        read_tmux_window_option("@bn_repo"),
        read_tmux_window_option("@bn_branch"),
    );
    if !win_repo.is_empty()
        && !win_branch.is_empty()
        && (win_repo != ctx.repo || win_branch != ctx.branch)
    {
        return;
    }
    let (todos, blockers) = match bn_core::store::read(&cfg.note_file(ctx)) {
        Ok(note) => (
            note.front.todos.iter().filter(|t| t.is_open_todo()).count(),
            note.front.blockers.len(),
        ),
        Err(_) => (0, 0),
    };
    set_tmux_window_option("@bn_todos", &badge_count(todos));
    set_tmux_window_option("@bn_blockers", &badge_count(blockers));
}

/// A count as tmux stores it: EMPTY when zero, so `#{?@bn_todos,…}` (which tests emptiness, and
/// would read a literal "0" as truthy) stays quiet on a clean note.
pub(crate) fn badge_count(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        n.to_string()
    }
}

/// Set `@bn_agent` on the calling pane's window so `window-status-format` can show live
/// Claude Code activity (working / waiting / done). Fed by CC hooks; `clear` (and any
/// unrecognized value) blanks it so `#{?@bn_agent,…}` conditionals go quiet. No-op outside
/// tmux. I/O-free — no note read, so the hooks it backs stay fast.
///
/// Also stamps `@bn_agent_since` (unix epoch) with when the window entered this state — but
/// only when the state actually changes, so repeated hook fires (several Notification events
/// while waiting) don't reset how long it's been waiting. The jump-picker reads it to show
/// "waiting 4m" and to rank the longest-waiting agent first.
pub(crate) fn set_tmux_agent(state: &str) {
    if std::env::var_os("TMUX").is_none() {
        return;
    }
    let value = normalize_agent_state(state);
    let changed = read_tmux_window_option("@bn_agent") != value;
    set_tmux_window_option("@bn_agent", value);
    if changed {
        let since = if value.is_empty() {
            String::new()
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default()
        };
        set_tmux_window_option("@bn_agent_since", &since);
    }
}

/// The `@bn_agent` vocabulary, and the single place that decides what is a legal value.
///
/// `working`/`waiting`/`done` are what the harness hooks fire. The rest are escalation states the
/// fleet classifier produces and, until now, could not store: `blocked` and `needs-decision` mean
/// an agent is stopped on a human, `limited` means it hit a usage cap, and `failed` means it died.
/// `failed` in particular was being rejected here while `bn agent done --failed` passed it, so a
/// crashed agent's badge was silently *cleared* — its window then looked identical to an idle one.
/// Anything unrecognized still blanks the badge, which is how callers clear it.
pub(crate) fn normalize_agent_state(state: &str) -> &str {
    match state {
        "working" | "waiting" | "done" | "blocked" | "needs-decision" | "limited" | "failed" => {
            state
        }
        _ => "", // clear / unknown
    }
}

/// `tmux set-option -w [-t <target>] <name> <value>` — sets a window-scoped user option.
/// `target` names the window; `None` falls back to the calling pane's window (`$TMUX_PANE`),
/// which is what the hook-driven callers want. Best-effort; failures are ignored.
pub(crate) fn set_tmux_window_option_on(target: Option<&str>, name: &str, value: &str) {
    let mut cmd = crate::proc::tmux_command(&["set-option", "-w"]);
    match target {
        Some(t) => {
            cmd.arg("-t").arg(t);
        }
        None => {
            if let Some(pane) = std::env::var_os("TMUX_PANE") {
                cmd.arg("-t").arg(pane);
            }
        }
    }
    cmd.arg(name).arg(value);
    let _ = cmd.status();
}

/// Set a window-scoped user option on the calling pane's window.
pub(crate) fn set_tmux_window_option(name: &str, value: &str) {
    set_tmux_window_option_on(None, name, value);
}

/// Read a window-scoped user option for the calling pane's window (`$TMUX_PANE`); empty
/// string if unset or tmux errors. `-q` stays quiet on a missing option, `-v` prints the
/// bare value.
pub(crate) fn read_tmux_window_option(name: &str) -> String {
    let mut cmd = crate::proc::tmux_command(&["show-options", "-wqv"]);
    if let Some(pane) = std::env::var_os("TMUX_PANE") {
        cmd.arg("-t").arg(pane);
    }
    cmd.arg(name);
    match cmd.output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}
