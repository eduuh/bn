//! Native tmux status-bar segments (Phase 1 of the `.sh`→Rust migration, #). These replace the
//! pure-logic shell scripts tmux invoked from `workflow/tmux.conf` via `#(...)`/hooks:
//!   - `bn tmux-git`        ← workflow/bin/bn-git-status.sh   (status-format[0] git segment)
//!   - `bn tmux-goal`       ← workflow/bin/bn-goal-status.sh  (focus segment; no longer on the bar)
//!   - `bn tmux-attn-count` ← workflow/bin/bn-attention-count.sh (global waiting/ready tally)
//!   - `bn tmux-done-badge` ← workflow/bin/tmux-done-badge.sh  (now in `cmd/tmux_done.rs`)
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
        cfg.web_enabled_here() && cfg.web.tmux_mcp_autostart,
        crate::serve::port_listening(cfg.web.port),
    ) {
        crate::serve::run(cfg, None, false, true, false);
    }
}

/// `bn tmux-mcp badge` — emit a fast green/red health segment for the configured service port.
pub(crate) fn emit_tmux_mcp_badge(cfg: &Config) {
    if let Some(badge) = render_tmux_mcp_badge(
        cfg.web_enabled_here() && cfg.web.tmux_mcp_badge,
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
    // `--no-optional-locks`: this is a read-only display segment that reruns every
    // status-interval, and a plain `git status` takes a write lock to refresh the index —
    // racing the automated commits agents make in the same worktree. (The documented
    // workaround was to switch the whole dashboard off with `status-interval 0` during heavy
    // git work.) Both `-C` and this are git-global flags, so the leading `-C <dir>` that
    // `git_capture` prepends stays valid. Cost: a stat-dirty-but-identical file keeps showing
    // in `dirty` until some other git command refreshes the index — a cosmetic overcount on a
    // counter that is only ever a count.
    let Some(status) = crate::git::git_capture(
        &["--no-optional-locks", "status", "--porcelain=v2", "--branch"],
        Some(&dir),
        false,
    ) else {
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
/// empty `label` renders the dim "set a goal" nudge. A `stale` label — a PR title served from an
/// expired cache because `gh` could not be reached — renders dim, so a frozen title is
/// distinguishable from a current one without spending any width on a marker.
pub(crate) fn render_goal_seg(label: &str, stale: bool) -> String {
    if label.is_empty() {
        return "#[fg=#565f89]\u{25c6} set a goal".to_string();
    }
    let fg = if stale { "#565f89" } else { "#c0caf5" };
    format!("#[fg={fg}]{}", truncate_label(label, 80))
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
    let mut stale = false;
    if let Some(pr) = &note.front.pr {
        let pr_url = pr.display();
        if pr_url.starts_with("http") {
            (label, stale) = pr_title_cached(&pr_url);
        }
    }
    if label.is_empty() {
        // Falling back to the note's own goal — a local read, never stale.
        stale = false;
        if let Some(goal) = bn_core::render::goal_text(&note.body) {
            label = goal;
        }
    }
    print!("{}", render_goal_seg(&label, stale));
}

/// How long a fetched PR title stays fresh.
const PR_TITLE_TTL_SECS: u64 = 300;
/// First backoff window after a failed `gh` call, doubling to [`PR_TITLE_BACKOFF_MAX_SECS`].
const PR_TITLE_BACKOFF_MIN_SECS: u64 = 30;
/// Backoff ceiling — capped at the TTL so the title self-heals within one normal interval.
const PR_TITLE_BACKOFF_MAX_SECS: u64 = 300;
/// Wall-clock deadline for the status bar's `gh` call. The bar reruns every few seconds; a
/// lookup that has not answered in this long is not going to answer usefully.
const GH_STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Grow the backoff window after another failure: first miss waits
/// [`PR_TITLE_BACKOFF_MIN_SECS`], each subsequent one doubles up to [`PR_TITLE_BACKOFF_MAX_SECS`].
pub(crate) fn next_backoff(prev_secs: u64) -> u64 {
    if prev_secs == 0 {
        PR_TITLE_BACKOFF_MIN_SECS
    } else {
        prev_secs
            .saturating_mul(2)
            .clamp(PR_TITLE_BACKOFF_MIN_SECS, PR_TITLE_BACKOFF_MAX_SECS)
    }
}

/// Whether the status bar should spend a `gh` call this tick. `cache_age` is the age of the
/// stored title (`None` when never fetched); `miss` is `(age, backoff_window)` of the last failed
/// attempt. A fresh title needs nothing; a recent failure must not be retried until its window
/// has elapsed. Pure so the backoff is testable without a network or a clock.
pub(crate) fn should_refetch(cache_age: Option<u64>, miss: Option<(u64, u64)>) -> bool {
    if cache_age.is_some_and(|age| age < PR_TITLE_TTL_SECS) {
        return false;
    }
    !matches!(miss, Some((age, window)) if age < window)
}

/// Age of `path` in whole seconds, or `None` when it does not exist / has no usable mtime.
fn age_secs(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()?
        .elapsed()
        .ok()
        .map(|e| e.as_secs())
}

/// Resolve a PR's title, memoised on disk for [`PR_TITLE_TTL_SECS`]. Returns
/// `(title, stale)` — `stale` when the value came from an expired cache the bar could not
/// refresh, so the caller can render it dimmed rather than silently showing a frozen title.
///
/// The failure path used to return the stale title *without touching any file*, so the freshness
/// test failed again on the very next tick: offline, on a VPN flap or under a `gh` rate limit
/// this spawned `gh pr view` every few seconds forever, once per PR-linked worktree. A miss now
/// records itself in a sibling `<key>.miss` marker holding the current backoff window, so
/// failures cost one call per window instead of one per tick. The call is bounded too — an
/// unbounded `gh` on a status segment stalls the whole row.
fn pr_title_cached(pr_url: &str) -> (String, bool) {
    let cache_dir = std::env::temp_dir().join("bn-prtitle");
    let _ = std::fs::create_dir_all(&cache_dir);
    let key: String = pr_url
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let cache = cache_dir.join(&key);
    let miss_marker = cache_dir.join(format!("{key}.miss"));

    let cache_age = age_secs(&cache);
    let miss = age_secs(&miss_marker).map(|age| {
        let window = std::fs::read_to_string(&miss_marker)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(PR_TITLE_BACKOFF_MIN_SECS);
        (age, window)
    });

    let cached = || std::fs::read_to_string(&cache).unwrap_or_default();
    if !should_refetch(cache_age, miss) {
        // Either the title is still fresh, or we are inside a backoff window and must not spend
        // another call. Fresh reads render normally; a held-back one is stale by definition.
        let fresh = cache_age.is_some_and(|age| age < PR_TITLE_TTL_SECS);
        return (cached(), !fresh);
    }

    match crate::gh::gh_capture_bounded(
        &["pr", "view", pr_url, "--json", "title", "-q", ".title"],
        GH_STATUS_TIMEOUT,
    ) {
        Some(title) if !title.trim().is_empty() => {
            let title = title.trim().to_string();
            let _ = std::fs::write(&cache, &title);
            let _ = std::fs::remove_file(&miss_marker);
            (title, false)
        }
        // gh failed / empty — keep the last title, and record the miss so the next tick backs
        // off instead of firing again immediately.
        _ => {
            let window = next_backoff(miss.map(|(_, w)| w).unwrap_or(0));
            let _ = std::fs::write(&miss_marker, window.to_string());
            (cached(), true)
        }
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
    let t = attn_window_tally(lines, now);

    let mut s = String::new();
    if t.waiting > 0 {
        let color = wait_color(t.wait);
        s.push_str(&format!("#[fg={color}]{} waiting ", t.waiting));
        if let Some(dur) = fmt_wait(t.wait) {
            s.push_str(&format!("#[fg=colour243]{dur} "));
        }
        s.push_str("#[fg=#3b4261]\u{b7} ");
    }
    if t.ready > 0 {
        s.push_str(&format!("#[fg=green]{} ready #[fg=#3b4261]\u{b7} ", t.ready));
    }
    s
}

/// A wait crosses from "an agent is thinking" to "you are the bottleneck" at 15 minutes.
const WAIT_URGENT_SECS: i64 = 900;

/// Colour for a wait of `secs`: amber until [`WAIT_URGENT_SECS`], red past it.
pub(crate) fn wait_color(secs: i64) -> &'static str {
    if secs >= WAIT_URGENT_SECS {
        "red"
    } else {
        "colour214"
    }
}

/// Format a wait as `45m` / `2h15m`, or `None` under a minute (too short to be worth the width).
pub(crate) fn fmt_wait(secs: i64) -> Option<String> {
    (secs >= 60).then(|| {
        let m = secs / 60;
        if m >= 60 {
            format!("{}h{:02}m", m / 60, m % 60)
        } else {
            format!("{m}m")
        }
    })
}

/// Global tally of agent windows across every session.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentTally {
    /// An agent needs you (`@bn_agent==waiting`).
    pub waiting: u32,
    /// Finished (`@bn_agent==done`, or the silence-watch `@bn_done==✓`).
    pub ready: u32,
    /// Running and needing nothing (`@bn_agent==working`).
    pub working: u32,
    /// How long the oldest waiting window has waited, `0` when nothing is waiting.
    pub wait: i64,
}

/// Pure global tally of agent windows from `list-windows` lines + the current epoch `now`.
/// Shared by [`render_attn_count`], [`render_attn_badge`] and `bn attn badge`.
///
/// `working` is counted even though it needs nothing from you: the fall-through arm used to drop
/// those rows entirely, which made "three agents are busy" and "the machine is idle" render
/// identically on the bar.
pub(crate) fn attn_window_tally(lines: &str, now: i64) -> AgentTally {
    let mut t = AgentTally::default();
    let mut oldest: i64 = 0;
    for line in lines.lines() {
        let r = parse_attn_row(line);
        match r.agent {
            // `blocked`, `needs-decision` and `failed` all mean the same thing to you as `waiting`
            // does — an agent has stopped and cannot continue without you — so they belong in the
            // same lane and the same wait clock. They used to hit the fall-through arm and vanish.
            "waiting" | "blocked" | "needs-decision" | "failed" => {
                t.waiting += 1;
                if let Ok(since) = r.since.parse::<i64>() {
                    if oldest == 0 || since < oldest {
                        oldest = since;
                    }
                }
            }
            "done" => t.ready += 1,
            // `limited` is waiting on a usage cap, not on you: busy, not actionable.
            "working" | "limited" => t.working += 1,
            "" if r.done == "\u{2713}" => t.ready += 1,
            _ => {}
        }
    }
    t.wait = if oldest > 0 { now - oldest } else { 0 };
    t
}

/// Render the status-bar attention badge as up to five lanes, each omitted when zero, in
/// descending urgency: undeliverable-notification warning, urgent (cron failures + urgent
/// attention items), raised (everything else open), an agent waiting on you with the longest
/// wait, and agents merely working. `None` when there is nothing at all to say, so the bar
/// stays clean.
///
/// This replaced a single summed integer. `urgent + other + waiting + ready` collapsed four
/// unrelated conditions into one red number, so the only available response was "open a popup
/// and read" — and the wait duration, which is the whole reason a wait matters, was computed and
/// then dropped. Each lane now maps to a different action: red bolt = go fix cron, bell =
/// someone raised something, clock = an agent has been blocked on you this long (prefix-Tab
/// jumps to it), dim gear = the machine is busy, leave it alone.
pub(crate) fn render_attn_badge(
    b: &bn_core::doctor::Badge,
    t: &AgentTally,
    dead_channel: bool,
) -> Option<String> {
    if b.urgent == 0 && b.other == 0 && t.waiting == 0 && t.ready == 0 && t.working == 0 {
        return None;
    }
    let mut s = String::new();
    if dead_channel {
        // Every notification today was undeliverable: the push channel is not merely quiet, it
        // is broken, and nothing else on the bar distinguishes those two.
        s.push_str(&format!("#[fg=red]{}dead ", bn_core::glyph::BELL_SLASH));
    }
    if b.urgent > 0 {
        s.push_str(&format!("#[fg=red]{}{} ", b.urgent, bn_core::glyph::BOLT));
    }
    if b.other > 0 {
        s.push_str(&format!(
            "#[fg=colour214]{}{} ",
            b.other,
            bn_core::glyph::BELL
        ));
    }
    if t.waiting > 0 {
        s.push_str(&format!(
            "#[fg={}]{}{}",
            wait_color(t.wait),
            t.waiting,
            bn_core::glyph::CLOCK
        ));
        match fmt_wait(t.wait) {
            Some(dur) => s.push_str(&format!("{dur} ")),
            None => s.push(' '),
        }
    }
    if t.ready > 0 {
        s.push_str(&format!(
            "#[fg=#9ece6a]{}{} ",
            t.ready,
            bn_core::glyph::CHECK
        ));
    }
    if t.working > 0 {
        s.push_str(&format!(
            "#[fg=#565f89]{}{} ",
            t.working,
            bn_core::glyph::GEAR
        ));
    }
    s.push_str("#[fg=#3b4261]\u{b7} ");
    Some(s)
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
                format!("  {}", style::meta(&t))
            }
        };
        let row = |tone: style::Tone, label: &str, extra: &str| {
            let tag = style::tone(tone, label);
            format!("  {tag}  {session}:{index}  {name}{extra}\n")
        };
        let line_out = match agent {
            "waiting" => row(style::Tone::Warn, "\u{f0f3} needs you", &ago(since)),
            // These three hit the `_ => continue` below and dropped out of the cockpit entirely —
            // the roll-up claimed nothing was in flight while an agent sat blocked.
            "blocked" => row(style::Tone::Danger, "\u{f05e} blocked", &ago(since)),
            "needs-decision" => row(style::Tone::Warn, "\u{f06e} decide", &ago(since)),
            "failed" => row(style::Tone::Danger, "\u{f0e7} failed", &ago(since)),
            "working" => row(style::Tone::Warn, "\u{f013} working", &ago(since)),
            "limited" => row(style::Tone::Meta, "\u{f017} rate-limited", &ago(since)),
            "done" => row(style::Tone::Ok, "\u{f00c} done", &ago(since)),
            _ if done == "\u{2713}" => row(style::Tone::Ok, "\u{f00c} done", ""),
            _ if !sil.is_empty() && sil != "0" => row(
                style::Tone::Warn,
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

    println!("{}", style::head("In-flight windows"));
    let (body, n) = cockpit_body(&inflight, now);
    if n == 0 {
        println!("  nothing in flight");
    } else {
        print!("{body}");
    }

    // Fleet health (the session-log view: active/idle/stuck) — the stuck detection the window
    // decorations can't show. One supervise pass; the emitter prints directly.
    println!("\n{}", style::head("Fleet health"));
    crate::cmd::agent::emit_agent_supervise(
        cfg,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
        false,
    );

    println!("\n{}", style::head("bn summary"));
    crate::cmd::recall::emit_summary(cfg, false);

    print!("\nPress Enter to close…");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}

// ---- bn tmux-help : keybinding cheat-sheet popup (prefix ?) -------------------------------------

/// `bn tmux-help` — the keybinding cheat-sheet popup (bound to prefix ?).
///
/// Generated from the live key table, never hand-written. This used to be ~95 lines of hardcoded
/// rows, and it had drifted in both directions: it advertised `prefix B` (a briefings popup whose
/// binding and backing command were both deleted in #1297) while omitting `C-e`, `E`, `G` and `d`,
/// which had been bound for months. A cheat-sheet maintained separately from the thing it
/// describes is a cheat-sheet that lies.
///
/// The section comes from the binding's own `-N` note, as `Section · description` — so tmux.conf
/// is the single source of truth for both the key and how it is filed. Bindings tmux ships its own
/// notes for (`$` rename-session, `#` list-buffers …) have no `·` and land under "Stock tmux",
/// which is how the surviving defaults became discoverable at all.
pub(crate) fn emit_tmux_help() {
    let ink = style::Ink::auto();
    let hd = ink.open(style::Tone::Head);
    let key = ink.open(style::Tone::Accent);
    let d = ink.open(style::Tone::Meta);
    let n = ink.close();

    println!("{hd}tmux cheat-sheet{n}  {d}prefix = C-Space{n}");

    for (table, label) in [("prefix", ""), ("copy-mode-vi", " (in copy mode)")] {
        let Some(raw) = crate::proc::tmux_raw(&noted_keys_args(table)) else {
            eprintln!("bn: tmux is not reachable — no key table to read");
            return;
        };
        for (section, rows) in noted_key_sections(&raw, label) {
            println!("\n{hd}{section}{n}");
            for (k, desc) in rows {
                println!("  {key}{k:<10}{n} {desc}");
            }
        }
    }

    print!("\n{d}Press Enter to close…{n}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}

/// `list-keys` argv for one table's noted bindings. `-P ""` is load-bearing: tmux 3.7 prints the
/// prefix key in front of every row by default (`C-Space Tab  Agents · …`), in the copy-mode table
/// too, and a parser that splits on the first space then files `C-Space` as every key and the real
/// key as part of the section name. An empty prefix leaves one leading space, which
/// [`parse_noted_key`] trims, and older tmux prints the same shape.
pub(crate) fn noted_keys_args(table: &str) -> [&str; 6] {
    ["list-keys", "-N", "-P", "", "-T", table]
}

/// One `list-keys -N` row as `(key, note)`. The key never contains whitespace, so the first run of
/// spaces after it is the split. A row without a note is a binding nobody described: `None`.
pub(crate) fn parse_noted_key(line: &str) -> Option<(&str, &str)> {
    let (key, note) = line.trim_start().split_once(char::is_whitespace)?;
    let note = note.trim();
    (!note.is_empty()).then_some((key, note))
}

/// Group a table's noted bindings by the `Section · description` in their `-N` note, as
/// `(section, [(key, description)])`. Curated sections come first, in the order tmux.conf
/// introduces them; notes without a `·` (tmux's own) land under "Stock tmux", last, because they
/// are reference material rather than the daily loop. `label` suffixes every section name.
pub(crate) fn noted_key_sections(raw: &str, label: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for (k, note) in raw.lines().filter_map(parse_noted_key) {
        let (section, desc) = match note.split_once('·') {
            Some((s, rest)) => (format!("{}{label}", s.trim()), rest.trim()),
            None => (format!("Stock tmux{label}"), note),
        };
        let entry = (k.to_string(), desc.to_string());
        match sections.iter_mut().find(|(s, _)| *s == section) {
            Some((_, rows)) => rows.push(entry),
            None => sections.push((section, vec![entry])),
        }
    }
    sections.sort_by_key(|(s, _)| s.starts_with("Stock tmux"));
    sections
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

#[cfg(test)]
mod machine_scope_tests {
    use super::*;

    #[test]
    fn disabled_web_neither_starts_nor_renders_a_badge() {
        assert!(!tmux_mcp_should_start(false, false));
        assert_eq!(render_tmux_mcp_badge(false, true, 7770), None);
    }
}

#[cfg(test)]
mod attn_badge_tests {
    use super::*;
    use bn_core::doctor::Badge;

    /// `#{@bn_agent}\t#{@bn_done}\t#{@bn_agent_since}`, the shape `tmux list-windows -a` emits.
    fn row(agent: &str, since: i64) -> String {
        format!("{agent}\t\t{since}")
    }

    fn badge(urgent: usize, other: usize) -> Badge {
        Badge { urgent, other }
    }

    /// The regression the old fall-through arm caused: a working agent was matched by `_ => {}`
    /// and discarded, so a busy machine tallied identically to an idle one.
    #[test]
    fn a_working_agent_is_counted_rather_than_dropped() {
        let lines = [row("working", 0), row("working", 0), row("waiting", 500)].join("\n");
        let t = attn_window_tally(&lines, 1_000);
        assert_eq!(t.working, 2);
        assert_eq!(t.waiting, 1);
        assert_eq!(t.wait, 500);
    }

    /// The whole point of widening the vocabulary: a stopped agent is the state that most needs a
    /// human, and it used to hit `_ => {}` and be counted as nothing at all. All three escalation
    /// states share the `waiting` lane and its wait clock.
    #[test]
    fn a_blocked_or_failed_agent_counts_as_needing_you() {
        let lines = [
            row("blocked", 400),
            row("needs-decision", 600),
            row("failed", 200),
        ]
        .join("\n");
        let t = attn_window_tally(&lines, 1_000);
        assert_eq!(t.waiting, 3);
        assert_eq!(t.wait, 800, "the oldest of the three sets the clock");
    }

    /// `limited` is stalled on a usage cap, not on you — busy, not actionable, so it belongs with
    /// `working` rather than raising the badge.
    #[test]
    fn a_rate_limited_agent_counts_as_working_not_waiting() {
        let t = attn_window_tally(&row("limited", 0), 1_000);
        assert_eq!((t.waiting, t.working), (0, 1));
    }

    /// The silence-watch fallback (`@bn_done == ✓` on a window with no `@bn_agent`) still counts
    /// as ready — behaviour the lane split must not disturb.
    #[test]
    fn the_silence_watch_fallback_still_counts_as_ready() {
        let t = attn_window_tally("\t\u{2713}\t", 1_000);
        assert_eq!(t.ready, 1);
        assert_eq!(t.working, 0);
    }

    /// The longest wait wins, not the most recent one — the bar should report the agent that has
    /// been blocked on you the longest.
    #[test]
    fn the_wait_reported_is_the_oldest_of_several() {
        let lines = [row("waiting", 900), row("waiting", 100)].join("\n");
        assert_eq!(attn_window_tally(&lines, 1_000).wait, 900);
    }

    #[test]
    fn a_quiet_cockpit_renders_no_badge_at_all() {
        let t = AgentTally::default();
        assert_eq!(render_attn_badge(&badge(0, 0), &t, false), None);
    }

    /// Ready windows alone are enough to render — they were part of the old summed total, so
    /// dropping them would be a silent regression.
    #[test]
    fn ready_windows_alone_still_render() {
        let t = AgentTally {
            ready: 1,
            ..Default::default()
        };
        let out = render_attn_badge(&badge(0, 0), &t, false).expect("ready is worth saying");
        assert!(out.contains(bn_core::glyph::CHECK), "{out}");
    }

    /// The whole point of the change: four conditions that used to sum into one red integer now
    /// render as four separately-coloured lanes, in descending urgency, with the wait duration
    /// that was previously computed and thrown away.
    #[test]
    fn every_lane_renders_in_descending_urgency_with_the_wait_duration() {
        let t = AgentTally {
            waiting: 1,
            ready: 0,
            working: 3,
            wait: 8_100, // 2h15m
        };
        let out = render_attn_badge(&badge(6, 3), &t, false).expect("plenty to say");
        let expected = format!(
            "#[fg=red]6{} #[fg=colour214]3{} #[fg=red]1{}2h15m #[fg=#565f89]3{} #[fg=#3b4261]\u{b7} ",
            bn_core::glyph::BOLT,
            bn_core::glyph::BELL,
            bn_core::glyph::CLOCK,
            bn_core::glyph::GEAR,
        );
        assert_eq!(out, expected);
    }

    /// A wait is amber while an agent is merely thinking and red once you are plainly the
    /// bottleneck; the boundary is 15 minutes.
    #[test]
    fn a_wait_turns_red_at_fifteen_minutes() {
        assert_eq!(wait_color(899), "colour214");
        assert_eq!(wait_color(900), "red");
    }

    /// Sub-minute waits are not worth the width; beyond that the format switches at an hour.
    #[test]
    fn a_wait_is_formatted_only_once_it_is_worth_reading() {
        assert_eq!(fmt_wait(59), None);
        assert_eq!(fmt_wait(60).as_deref(), Some("1m"));
        assert_eq!(fmt_wait(2_700).as_deref(), Some("45m"));
        assert_eq!(fmt_wait(8_100).as_deref(), Some("2h15m"));
    }

    /// A broken push channel is announced ahead of everything else, and says nothing at all when
    /// the backend is healthy.
    #[test]
    fn a_dead_notification_channel_is_announced_first() {
        let t = AgentTally {
            working: 1,
            ..Default::default()
        };
        let dead = render_attn_badge(&badge(0, 0), &t, true).expect("something to say");
        assert!(
            dead.starts_with(&format!("#[fg=red]{}dead ", bn_core::glyph::BELL_SLASH)),
            "{dead}"
        );
        let healthy = render_attn_badge(&badge(0, 0), &t, false).expect("something to say");
        assert!(!healthy.contains("dead"), "{healthy}");
    }

    /// `bn tmux-attn-count` is a separate, unchanged surface — pin its output so the AgentTally
    /// refactor cannot quietly alter it.
    #[test]
    fn the_standalone_attn_count_segment_is_unchanged_by_the_refactor() {
        let lines = [row("waiting", 100), row("done", 0), row("working", 0)].join("\n");
        assert_eq!(
            render_attn_count(&lines, 8_200),
            "#[fg=red]1 waiting #[fg=colour243]2h15m #[fg=#3b4261]\u{b7} \
             #[fg=green]1 ready #[fg=#3b4261]\u{b7} "
        );
    }
}

#[cfg(test)]
mod pr_title_backoff_tests {
    use super::*;

    /// A fresh title needs no call; an expired one does.
    #[test]
    fn a_fresh_title_is_not_refetched_and_an_expired_one_is() {
        assert!(!should_refetch(Some(10), None));
        assert!(should_refetch(Some(PR_TITLE_TTL_SECS), None));
        assert!(should_refetch(None, None));
    }

    /// The bug this fixes: the failure path left no trace, so an expired cache re-fired `gh` on
    /// every tick. Inside a recorded backoff window the bar must spend nothing.
    #[test]
    fn a_recent_failure_suppresses_the_next_call_until_its_window_elapses() {
        assert!(!should_refetch(Some(9_999), Some((5, 30))));
        assert!(should_refetch(Some(9_999), Some((30, 30))));
    }

    #[test]
    fn the_backoff_window_doubles_from_thirty_seconds_up_to_the_ttl() {
        assert_eq!(next_backoff(0), 30);
        assert_eq!(next_backoff(30), 60);
        assert_eq!(next_backoff(60), 120);
        assert_eq!(next_backoff(240), PR_TITLE_BACKOFF_MAX_SECS);
        assert_eq!(next_backoff(PR_TITLE_BACKOFF_MAX_SECS), PR_TITLE_BACKOFF_MAX_SECS);
    }

    /// A title the bar could not refresh renders dim, so a frozen title is distinguishable from a
    /// current one — but an absent goal keeps its own distinct nudge.
    #[test]
    fn a_stale_title_renders_dim_and_a_fresh_one_bright() {
        assert_eq!(render_goal_seg("ship it", false), "#[fg=#c0caf5]ship it");
        assert_eq!(render_goal_seg("ship it", true), "#[fg=#565f89]ship it");
        assert_eq!(render_goal_seg("", true), "#[fg=#565f89]\u{25c6} set a goal");
    }
}

#[cfg(test)]
mod tmux_help_tests {
    use super::*;

    /// tmux 3.7c `list-keys -N -P "" -T prefix`, as captured on the machine that found the bug: one
    /// leading space where the prefix used to be, a curated note, and one of tmux's own.
    const EMPTY_PREFIX: &str = " Tab     Agents · jump straight to the top window needing you\n \
                                Enter   Tools · scratch shell in this worktree\n \
                                #       List all paste buffers\n";

    /// The key-first shape older tmux prints for the same table.
    const KEY_FIRST: &str = "Tab     Agents · jump straight to the top window needing you\n\
                             Enter   Tools · scratch shell in this worktree\n\
                             #       List all paste buffers\n";

    /// The regression: without `-P ""`, 3.7c's prefix column made `C-Space` every row's key.
    #[test]
    fn the_argv_asks_tmux_for_no_prefix_column() {
        assert_eq!(noted_keys_args("prefix"), ["list-keys", "-N", "-P", "", "-T", "prefix"]);
    }

    #[test]
    fn both_row_shapes_file_the_real_key_under_its_section() {
        for raw in [EMPTY_PREFIX, KEY_FIRST] {
            assert_eq!(
                noted_key_sections(raw, ""),
                vec![
                    (
                        "Agents".to_string(),
                        vec![("Tab".to_string(), "jump straight to the top window needing you".to_string())]
                    ),
                    ("Tools".to_string(), vec![("Enter".to_string(), "scratch shell in this worktree".to_string())]),
                    ("Stock tmux".to_string(), vec![("#".to_string(), "List all paste buffers".to_string())]),
                ]
            );
        }
    }

    #[test]
    fn a_row_without_a_note_is_skipped_and_the_label_suffixes_sections() {
        assert_eq!(parse_noted_key(" G   "), None);
        assert_eq!(parse_noted_key(""), None);
        let sections = noted_key_sections(" G   Copy mode · bottom of history\n", " (in copy mode)");
        assert_eq!(sections[0].0, "Copy mode (in copy mode)");
        assert_eq!(sections[0].1, vec![("G".to_string(), "bottom of history".to_string())]);
    }
}
