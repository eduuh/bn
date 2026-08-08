//! Native tmux status-bar segments (Phase 1 of the `.sh`→Rust migration, #). These replace the
//! pure-logic shell scripts tmux invoked from `workflow/tmux.conf` via `#(...)`/hooks:
//!   - `bn tmux-git`        ← workflow/bin/bn-git-status.sh   (status-format[0] git segment)
//!   - `bn tmux-goal`       ← workflow/bin/bn-goal-status.sh  (status-format[0] focus segment)
//!   - `bn tmux-attn-count` ← workflow/bin/bn-attention-count.sh (global waiting/ready tally)
//!   - `bn tmux-done-badge` ← workflow/bin/tmux-done-badge.sh  (alert-silence hook)
//!
//! The parsing/formatting is factored into pure functions (unit-tested below) so the migration is
//! verifiable byte-for-byte against the scripts' output; the `emit_*` wrappers do only the I/O
//! (git call / note read / tmux list-windows / gh cache / notify).
use crate::*;
use std::path::PathBuf;

/// Whether a tmux lifecycle hook should start the managed service.
pub(crate) fn tmux_mcp_should_start(enabled: bool, listening: bool) -> bool {
    enabled && !listening
}

/// Render the configured tmux MCP segment as `bnweb:<port>` — green when the `bn serve` (bn-web)
/// port is listening, red otherwise. Disabled badges collapse entirely.
pub(crate) fn render_tmux_mcp_badge(enabled: bool, listening: bool, port: u16) -> Option<String> {
    enabled.then(|| {
        if listening {
            format!("#[fg=green]bnweb:{port}")
        } else {
            format!("#[fg=red]bnweb:{port}#[default]")
        }
    })
}

/// `bn tmux-mcp ensure` — idempotently ensure the managed bn-web/MCP service is reachable.
pub(crate) fn emit_tmux_mcp_ensure(cfg: &Config) {
    if tmux_mcp_should_start(
        cfg.web.tmux_mcp_autostart,
        crate::serve::port_listening(cfg.web.port),
    ) {
        crate::serve::run(None, false, true, false);
    }
}

/// `bn tmux-mcp badge` — emit a fast green/red health segment for the configured service port.
pub(crate) fn emit_tmux_mcp_badge(cfg: &Config) {
    if let Some(badge) = render_tmux_mcp_badge(
        cfg.web.tmux_mcp_badge,
        crate::serve::port_listening(cfg.web.port),
        cfg.web.port,
    ) {
        print!("{badge}");
    }
}

// ---- bn tmux-git : themed git segment ----------------------------------------------------------

/// Parsed `git status --porcelain=v2 --branch` state the segment needs.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct GitSeg {
    pub branch: String,
    pub ahead: i64,
    pub behind: i64,
    pub dirty: usize,
}

/// Parse porcelain-v2 `--branch` output into [`GitSeg`]. Header lines start with `#`; every other
/// non-empty line is a changed/untracked entry (counts toward `dirty`).
pub(crate) fn parse_git_status(status: &str) -> GitSeg {
    let mut seg = GitSeg::default();
    for line in status.lines() {
        if let Some(b) = line.strip_prefix("# branch.head ") {
            seg.branch = b.to_string();
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            let mut it = ab.split_whitespace();
            seg.ahead = it
                .next()
                .and_then(|a| a.trim_start_matches('+').parse().ok())
                .unwrap_or(0);
            seg.behind = it
                .next()
                .and_then(|b| b.trim_start_matches('-').parse().ok())
                .unwrap_or(0);
        } else if line.starts_with('#') || line.is_empty() {
            // other headers / blank — ignore
        } else {
            seg.dirty += 1;
        }
    }
    seg
}

/// Render the themed (Tokyo Night, fg-only) status fragment for a parsed [`GitSeg`], or `None`
/// outside a git repo (empty branch). A clean, up-to-date branch renders as just `⎇ <branch>`.
pub(crate) fn render_git_seg(seg: &GitSeg) -> Option<String> {
    if seg.branch.is_empty() {
        return None;
    }
    let branch = if seg.branch == "(detached)" {
        "detached"
    } else {
        &seg.branch
    };
    let mut out = format!("#[fg=#3b4261]\u{2502} #[fg=#7aa2f7]\u{2387} {branch}");
    if seg.ahead > 0 {
        out.push_str(&format!(" #[fg=#e0af68]\u{2191}{}", seg.ahead));
    }
    if seg.behind > 0 {
        out.push_str(&format!(" #[fg=#7dcfff]\u{2193}{}", seg.behind));
    }
    if seg.dirty > 0 {
        out.push_str(&format!(" #[fg=#f7768e]\u{25cf}{}", seg.dirty));
    } else {
        out.push_str(" #[fg=#9ece6a]\u{25cf}");
    }
    Some(out)
}

/// `bn tmux-git [path]` — emit the git segment for `path` (tmux passes `#{pane_current_path}`)
/// or cwd. Silent outside a git repo so the segment collapses.
pub(crate) fn emit_tmux_git(path: Option<String>) {
    let dir = path
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if !dir.is_dir() {
        return;
    }
    let Some(status) =
        crate::git::git_capture(&["status", "--porcelain=v2", "--branch"], Some(&dir), false)
    else {
        return;
    };
    if let Some(out) = render_git_seg(&parse_git_status(&status)) {
        print!("{out}");
    }
}

// ---- bn tmux-goal : focus (PR title or goal) segment -------------------------------------------

/// Truncate a focus label to `max` display chars, appending `…` when it overflows (matches the
/// script's `printf '%.*s' … "$label"…`).
pub(crate) fn truncate_label(label: &str, max: usize) -> String {
    if label.chars().count() > max {
        let kept: String = label.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}\u{2026}")
    } else {
        label.to_string()
    }
}

/// Render the focus segment: the linked PR title (preferred) or the branch goal, truncated. An
/// empty `label` renders the dim "set a goal" nudge.
pub(crate) fn render_goal_seg(label: &str) -> String {
    if label.is_empty() {
        return "#[fg=#565f89]\u{25c6} set a goal".to_string();
    }
    format!("#[fg=#c0caf5]{}", truncate_label(label, 80))
}

/// `bn tmux-goal [path]` — emit the focus segment (PR title if linked, else branch goal). Prefers
/// the PR title (the crystallized goal); the title needs a `gh` call so it is cached 5 min (the
/// status-interval is seconds — we must not hit the network each tick). Reads the note directly
/// (no self-shell + sed like the script did).
pub(crate) fn emit_tmux_goal(cfg: &Config, path: Option<String>) {
    let dir = path
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if !dir.is_dir() {
        return;
    }
    let Ok(ctx) = resolver::resolve(cfg, &dir) else {
        return;
    };
    let Ok(note) = bn_core::store::read(&cfg.note_file(&ctx)) else {
        return;
    };

    let mut label = String::new();
    if let Some(pr) = &note.front.pr {
        let pr_url = pr.display();
        if pr_url.starts_with("http") {
            label = pr_title_cached(&pr_url);
        }
    }
    if label.is_empty() {
        if let Some(goal) = bn_core::render::goal_text(&note.body) {
            label = goal;
        }
    }
    print!("{}", render_goal_seg(&label));
}

/// Resolve a PR's title, memoised on disk for 5 minutes. Returns the cached title on a `gh` miss
/// so a transient network failure doesn't blank the bar. Empty string when uncached and `gh`
/// yields nothing.
fn pr_title_cached(pr_url: &str) -> String {
    let cache_dir = std::env::temp_dir().join("bn-prtitle");
    let _ = std::fs::create_dir_all(&cache_dir);
    let key: String = pr_url
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let cache = cache_dir.join(&key);

    let fresh = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().map(|e| e.as_secs() < 300).unwrap_or(false))
        .unwrap_or(false);
    if fresh {
        return std::fs::read_to_string(&cache).unwrap_or_default();
    }

    match crate::gh::gh_capture(&["pr", "view", pr_url, "--json", "title", "-q", ".title"]) {
        Some(title) if !title.trim().is_empty() => {
            let title = title.trim().to_string();
            let _ = std::fs::write(&cache, &title);
            title
        }
        // gh failed / empty — fall back to any stale cache so the bar keeps its last title.
        _ => std::fs::read_to_string(&cache).unwrap_or_default(),
    }
}

// ---- bn tmux-attn-count : global waiting/ready tally -------------------------------------------

/// One `#{@bn_agent}\t#{@bn_done}\t#{@bn_agent_since}` row from `tmux list-windows -a`.
pub(crate) struct AttnRow<'a> {
    pub agent: &'a str,
    pub done: &'a str,
    pub since: &'a str,
}

/// Parse a tab-separated `list-windows` line, preserving empty fields (a `read`/split that treats
/// tab as whitespace would collapse them and misclassify a row whose `@bn_done` is empty).
pub(crate) fn parse_attn_row(line: &str) -> AttnRow<'_> {
    let mut it = line.splitn(3, '\t');
    AttnRow {
        agent: it.next().unwrap_or(""),
        done: it.next().unwrap_or(""),
        since: it.next().unwrap_or(""),
    }
}

/// Compute the themed tally fragment from `list-windows` lines and the current epoch `now`.
/// `waiting` = a Claude agent needs you (`@bn_agent==waiting`); `ready`/done = finished
/// (`@bn_agent==done` or the silence-watch `@bn_done==✓`). `working` agents don't need you and
/// aren't counted. The waiting badge carries the longest wait and flips amber→red past 15 min.
///
/// The pure count itself lives in [`attn_window_tally`] so `bn attn badge` (which folds these
/// windows into its single total) shares one implementation.
pub(crate) fn render_attn_count(lines: &str, now: i64) -> String {
    let (waiting, ready, wait) = attn_window_tally(lines, now);

    let mut s = String::new();
    if waiting > 0 {
        let color = if wait >= 900 { "red" } else { "colour214" };
        s.push_str(&format!("#[fg={color}]{waiting} waiting "));
        if wait >= 60 {
            let m = wait / 60;
            let dur = if m >= 60 {
                format!("{}h{:02}m", m / 60, m % 60)
            } else {
                format!("{m}m")
            };
            s.push_str(&format!("#[fg=colour243]{dur} "));
        }
        s.push_str("#[fg=#3b4261]\u{b7} ");
    }
    if ready > 0 {
        s.push_str(&format!("#[fg=green]{ready} ready #[fg=#3b4261]\u{b7} "));
    }
    s
}

/// Pure global tally of agent windows from `list-windows` lines + the current epoch `now`:
/// returns `(waiting, ready, longest_wait_secs)`. `waiting` = an agent needs you
/// (`@bn_agent==waiting`); `ready` = finished (`@bn_agent==done`, or the silence-watch
/// `@bn_done==✓`); `longest_wait_secs` = how long the oldest waiting window has waited (`0`
/// when nothing is waiting). Shared by [`render_attn_count`] and `bn attn badge`.
pub(crate) fn attn_window_tally(lines: &str, now: i64) -> (u32, u32, i64) {
    let mut waiting = 0u32;
    let mut ready = 0u32;
    let mut oldest: i64 = 0;
    for line in lines.lines() {
        let r = parse_attn_row(line);
        match r.agent {
            "waiting" => {
                waiting += 1;
                if let Ok(since) = r.since.parse::<i64>() {
                    if oldest == 0 || since < oldest {
                        oldest = since;
                    }
                }
            }
            "done" => ready += 1,
            "" if r.done == "\u{2713}" => ready += 1,
            _ => {}
        }
    }
    let wait = if oldest > 0 { now - oldest } else { 0 };
    (waiting, ready, wait)
}

/// `bn tmux-attn-count` — global tally of windows needing attention across ALL sessions, for the
/// status bar. Emits a styled fragment or nothing when all quiet (bar stays clean).
pub(crate) fn emit_tmux_attn_count() {
    let fmt = "#{@bn_agent}\t#{@bn_done}\t#{@bn_agent_since}";
    let Some(out) = crate::proc::tmux_raw(&["list-windows", "-a", "-F", fmt]) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    print!("{}", render_attn_count(&out, now));
}

// ---- bn tmux-done-badge : mark a watched window done on silence --------------------------------

/// `bn tmux-done-badge <window>` — invoked from the global `alert-silence` hook with the window id.
/// Marks the window done (green ✓), clears `monitor-silence` so it stops re-firing, and fires a
/// best-effort desktop notification. Never fails the hook on a missing notifier.
pub(crate) fn emit_tmux_done_badge(win: &str) {
    if win.is_empty() {
        return;
    }
    let name = crate::proc::tmux_raw(&["display-message", "-p", "-t", win, "#{window_name}"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "window".to_string());

    let _ = crate::proc::tmux_run(&["set-window-option", "-t", win, "@bn_done", "\u{2713}"]);
    let _ = crate::proc::tmux_run(&["set-window-option", "-t", win, "monitor-silence", "0"]);

    let msg = format!("{name} finished");
    // Best-effort desktop toast through the policy gate; a missing backend is swallowed so the
    // hook never fails. A completion duplicates the ✓ badge set just above, so under the default
    // bar the badge is the whole notification.
    let _ = crate::toast::toast(
        crate::toast::Class::Completion,
        &format!("tmux:{win}:silent"),
        "tmux",
        "tmux",
        &msg,
    );
}

// ---- bn tmux-cockpit : orchestration roll-up popup (prefix S) -----------------------------------

/// Human-readable "time in state" for an `@bn_agent_since` epoch string: `Ns` / `Nm` / `Nh`, or
/// empty when unparseable. Mirrors the cockpit script's `dur()`.
fn cockpit_dur(since: &str, now: i64) -> String {
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

/// Classify the in-flight windows from `tmux list-windows` (7 tab-separated fields:
/// session, index, name, monitor-silence, @bn_done, @bn_agent, @bn_agent_since) into the coloured
/// roll-up body. `@bn_agent` takes precedence over the silence signals. Returns the body plus the
/// count of in-flight rows (0 → the caller prints "nothing in flight").
pub(crate) fn cockpit_body(lines: &str, now: i64) -> (String, usize) {
    let mut out = String::new();
    let mut n = 0usize;
    for line in lines.lines() {
        let f: Vec<&str> = line.splitn(7, '\t').collect();
        if f.len() < 7 {
            continue;
        }
        let (session, index, name, sil, done, agent, since) =
            (f[0], f[1], f[2], f[3], f[4], f[5], f[6]);
        let ago = |s: &str| {
            let t = cockpit_dur(s, now);
            if t.is_empty() {
                String::new()
            } else {
                format!("  \x1b[2m{t}\x1b[0m")
            }
        };
        let row = |color: &str, label: &str, extra: &str| {
            format!("  \x1b[{color}m{label}\x1b[0m  {session}:{index}  {name}{extra}\n")
        };
        let line_out = match agent {
            "waiting" => row("1;33", "\u{f0f3} needs you", &ago(since)),
            "working" => row("33", "\u{f013} working", &ago(since)),
            "done" => row("32", "\u{f00c} done", &ago(since)),
            _ if done == "\u{2713}" => row("32", "\u{f00c} done", ""),
            _ if !sil.is_empty() && sil != "0" => row(
                "33",
                "\u{f06e} watching",
                &format!("  (quiet {sil}s = done)"),
            ),
            _ => continue,
        };
        out.push_str(&line_out);
        n += 1;
    }
    (out, n)
}

/// `bn tmux-cockpit` — orchestration roll-up popup (bound to prefix S). In-flight windows across
/// every session, then fleet health (`agent supervise`) and the bn cross-branch summary. Replaces
/// workflow/bin/tmux-cockpit.sh (the awk classifier is now `cockpit_body`).
pub(crate) fn emit_tmux_cockpit(cfg: &Config) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let fmt = "#{session_name}\t#{window_index}\t#{window_name}\t#{monitor-silence}\t#{@bn_done}\t#{@bn_agent}\t#{@bn_agent_since}";
    let inflight = crate::proc::tmux_raw(&["list-windows", "-a", "-F", fmt]).unwrap_or_default();

    println!("\x1b[1;36mIn-flight windows\x1b[0m");
    let (body, n) = cockpit_body(&inflight, now);
    if n == 0 {
        println!("  nothing in flight");
    } else {
        print!("{body}");
    }

    // Fleet health (the session-log view: active/idle/stuck) — the stuck detection the window
    // decorations can't show. One supervise pass; the emitter prints directly.
    println!("\n\x1b[1;36mFleet health\x1b[0m");
    crate::cmd::agent::emit_agent_supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
        false,
    );

    println!("\n\x1b[1;36mbn summary\x1b[0m");
    crate::cmd::recall::emit_summary(cfg, false);

    print!("\nPress Enter to close…");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}

// ---- bn tmux-help : keybinding cheat-sheet popup (prefix ?) -------------------------------------

/// `bn tmux-help` — the curated keybinding cheat-sheet popup (bound to prefix ?). Replaces
/// workflow/bin/tmux-help.sh; the content is embedded so it deploys with the binary.
pub(crate) fn emit_tmux_help() {
    const C: &str = "\x1b[1;36m"; // cyan header
    const K: &str = "\x1b[1;37m"; // key
    const D: &str = "\x1b[0;90m"; // dim
    const N: &str = "\x1b[0m";
    let row = |k: &str, desc: &str| println!("  {K}{k:<10}{N} {desc}");
    let hdr = |h: &str| println!("\n{C}{h}{N}");

    println!("{C}tmux cheat-sheet{N}  {D}prefix = C-Space{N}");

    hdr("Projects & sessions");
    row("C-o", "project / worktree picker (frecency + preview)");
    row("C-w", "picker scoped to Windows-fs repos");
    row("C-p", "jump to the planning / orchestration session");
    row("BSpace", "jump to the planning session (same as C-p)");
    row("t", "kill this session, switch to next");
    row(".", "rename session");

    hdr("Worktrees (windows)");
    row("o", "open worktree window (create from main if new)");
    row("W", "always open a NEW worktree window");
    row("x", "close this worktree — return its lease, drop its window");
    row("X", "kill this window (all panes); the worktree stays");
    row("D", "pick any worktree to delete");

    hdr("Agent cockpit");
    row("A", "spawn a Copilot agent on this worktree (auto-watched)");
    row(
        "m",
        &format!("watch this window — {C}\u{f00c}{N} when it goes quiet"),
    );
    row("S", "cockpit: in-flight windows + bn summary");
    row(
        "j",
        "jump-picker: switch to a window needing you (preview = its pane)",
    );
    row(
        "Tab",
        "fast-path: jump to the top window needing you (repeat to cycle)",
    );
    row(
        "f",
        "fleet picker: jump to ANY agent + orchestrator window (live preview)",
    );

    hdr("Branch notes (bn)");
    row("n", "open the branch note");
    row("a", "quick-add a todo");
    row("b", "quick-add a blocker");
    row("C-a", "attention queue (doctor + open todos)");
    row(
        "B",
        "briefings: staff cards (↵ read · ctrl-u ▲ · ctrl-d ▼ · m mark · ctrl-o link · ctrl-r reload)",
    );

    hdr("Windows");
    row(
        "w",
        &format!("new window      {D}r{N} rename     {D}1-5{N} select"),
    );
    row(
        "[ / ]",
        &format!("prev / next     {D}< / >{N} move    {D}C-BSpace{N} next"),
    );

    hdr("Panes");
    row("v / s", "split right / below");
    row(
        "← ↑ ↓ →",
        &format!("navigate    {D}S-arrows{N} resize    {D}z{N} zoom"),
    );
    row("! / @", "break-pane / join-pane");
    row("q", "kill pane (worktree + lease untouched — x is the close)");

    hdr("Tools & popups");
    row("e", &format!("floating nvim (toggle)    {D}g{N} lazygit"));
    row("Enter", "scratch shell in this worktree");
    row(
        "Space",
        &format!("copy-mode   {D}p{N} paste   {D}P{N} choose-buffer"),
    );
    row("R", "reload config   ? this help");

    print!("\n{D}Press Enter to close…{N}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}

// ---- bn tmux-note : open the current branch note in $EDITOR (popup, prefix n) -------------------

/// `bn tmux-note` — resolve the current dir's branch note and open it in the editor. Replaces
/// workflow/bin/tmux-note.sh. Execs the editor (so closing it closes the popup); errors to stderr
/// when the note dir can't be resolved.
pub(crate) fn emit_tmux_note(cfg: &Config) {
    let Ok(ctx) = resolver::resolve(cfg, &std::env::current_dir().unwrap_or_default()) else {
        eprintln!("bn: could not resolve branch note dir for this directory");
        eprintln!("    Make sure you're inside a git repo.");
        std::process::exit(1);
    };
    let note = cfg.note_file(&ctx);
    if let Some(parent) = note.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".to_string());
    let status = std::process::Command::new(&editor).arg(&note).status();
    std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(0));
}

/// Interactive single-line capture for the `bn add <section> --prompt` popup (prefix a/b). Prints
/// the "Add <section> (empty to cancel):" prompt, reads one line from the tty, and returns it
/// trimmed of the trailing newline. Empty (cancel) → `None`.
pub(crate) fn prompt_capture(section: &str) -> Option<String> {
    print!("Add {section} (empty to cancel): ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return None;
    }
    let text = buf.trim_end_matches(['\n', '\r']).to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}


