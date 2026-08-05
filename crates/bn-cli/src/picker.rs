//! Native interactive pickers — the fzf-driven attention popups that used to live as shell
//! scripts under `workflow/bin`. Phase 1 ports `bn-attention.sh` (prefix `C-a`) to
//! `bn doctor --pick`.
//!
//! Mirrors the self-referencing shape of the bash original: the same binary renders the row
//! list and the preview panel and performs the per-key actions, and fzf's key bindings just
//! re-invoke `bn` with a hidden `--pick-stage`. No attention logic lives here — every row and
//! action is an existing native `bn` surface (`doctor`, `attn ack/snooze`, `cron show`).
//!
//! Extends the `bn wt` precedent (native fzf + native dispatch); see `MIGRATION.md`.

use bn_core::doctor::{self, Finding};
use bn_core::resolver::Config;
use bn_core::util::{add_days, now_epoch_secs, today};
use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};

/// The hidden callback stages a `--pick` fzf session re-invokes (`bn doctor --pick-stage <s>`).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum PickStage {
    /// Print the row list (fzf `reload` after an action, or the initial feed). Served from the
    /// short-lived snapshot cache so a reload is instant; a cache miss recomputes and warms it.
    List,
    /// Force a live recompute + rewrite of the snapshot, then print (ctrl-r's "refresh now").
    Refresh,
    /// Render the detail panel for the focused row.
    Preview,
    /// Ack the focused raised item (attn rows only).
    Ack,
    /// Snooze the focused raised item one day (attn rows only).
    Snooze,
    /// Remove the focused raised item — dismiss it as cancelled/mistaken (attn rows only).
    Remove,
    /// Plan the focused raised item — promote it to a branch todo, then resolve it (attn rows only).
    Plan,
    /// Start the focused raised item — open its worktree/planning window, launch its agent, and
    /// mark it in-flight.
    Start,
    /// Open the focused row — its link, the note in `$EDITOR`, or `cron show`.
    Open,
}

/// fzf field separator. Field 1 is the colored display; 2..=6 are hidden lookup columns.
const TAB: char = '\t';

/// A single attention row: what fzf shows, plus the hidden columns its bindings reference.
struct Row {
    display: String,
    kind: &'static str,
    id: String,
    job: String,
    link: String,
    path: String,
}

impl Row {
    /// `display \t kind \t id \t job \t link \t path` — the exact line fed to fzf and re-emitted
    /// by the `list` stage, so a `reload` reproduces the same columns.
    fn line(&self) -> String {
        format!(
            "{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}",
            self.display, self.kind, self.id, self.job, self.link, self.path
        )
    }
}

/// Map a `doctor` finding to a display row, matching `bn-attention.sh`'s `list()` colors.
fn row_for(f: &Finding) -> Row {
    match f {
        Finding::Cron(c) => Row {
            display: format!(
                "\x1b[1;31m⚠ cron {}\x1b[0m  \x1b[2m{} fail\x1b[0m",
                c.job, c.failures
            ),
            kind: "cron",
            id: String::new(),
            job: c.job.clone(),
            link: String::new(),
            path: String::new(),
        },
        Finding::Staff(s) => Row {
            // A missed staff run is a hard miss — red, the same weight as a failing cron.
            display: format!(
                "\x1b[1;31m⚠ staff {}\x1b[0m  \x1b[2mmissed {}\x1b[0m",
                s.agent, s.on_calendar
            ),
            kind: "staff",
            id: String::new(),
            job: String::new(),
            link: String::new(),
            path: String::new(),
        },
        Finding::Attn(a) => {
            let (color, tag) = match a.severity.as_str() {
                "urgent" => ("1;31", "⚠"),
                "warn" => ("33", "•"),
                _ => ("2", "•"),
            };
            let src = if a.source.is_empty() {
                String::new()
            } else {
                format!("  \x1b[2m({})\x1b[0m", a.source)
            };
            Row {
                display: format!("\x1b[{color}m{tag} {}\x1b[0m{src}", a.title),
                kind: "attn",
                id: a.id.clone(),
                job: String::new(),
                link: a.link.clone().unwrap_or_default(),
                path: String::new(),
            }
        }
        Finding::Note(n) => Row {
            display: format!(
                "\x1b[36m{}/{}\x1b[0m  \x1b[2m{}\x1b[0m",
                n.repo,
                n.branch,
                n.flags.join(" ")
            ),
            kind: "note",
            id: String::new(),
            job: String::new(),
            link: String::new(),
            path: n.path.clone(),
        },
        Finding::Todo(t) => {
            let (color, tag) = if t.overdue {
                ("1;31", "⚠")
            } else {
                ("33", "•")
            };
            Row {
                display: format!(
                    "\x1b[{color}m{tag} {}\x1b[0m  \x1b[2mtodo {}\x1b[0m",
                    t.summary, t.id
                ),
                kind: "todo",
                id: t.id.clone(),
                job: String::new(),
                link: String::new(),
                path: String::new(),
            }
        }
        Finding::Inbox(i) => Row {
            display: format!(
                "\x1b[33m• {}\x1b[0m  \x1b[2min inbox {}d — bn q drain\x1b[0m",
                i.text, i.idle_days
            ),
            kind: "inbox",
            id: i.id.clone(),
            job: String::new(),
            link: String::new(),
            path: String::new(),
        },
        Finding::Mcp(m) => Row {
            display: format!(
                "\x1b[33m• mcp {}\x1b[0m  \x1b[2m'{}' {} — {}\x1b[0m",
                m.server, m.command, m.problem, m.fix
            ),
            kind: "mcp",
            id: String::new(),
            job: String::new(),
            link: String::new(),
            path: String::new(),
        },
        Finding::StaleStaff(s) => Row {
            display: format!(
                "\x1b[33m• staff idle {}d\x1b[0m  \x1b[2mno card since {} — home box may be down\x1b[0m",
                s.idle_days, s.last_card
            ),
            kind: "stale-staff",
            id: String::new(),
            job: String::new(),
            link: String::new(),
            path: String::new(),
        },
        Finding::Backlog(b) => Row {
            display: format!(
                "\x1b[33m• queued work with no task\x1b[0m  \x1b[2m{} entry(s) predate the task invariant — `bn orchestrate status` to see them\x1b[0m",
                b.untasked
            ),
            kind: "backlog-untasked",
            id: String::new(),
            job: String::new(),
            link: String::new(),
            path: String::new(),
        },
        Finding::NotesLeak(l) => Row {
            display: format!(
                "\x1b[33m• personal notes leak\x1b[0m  \x1b[2m{} foreign machine(s) under {} — repoint [personal].notes_root\x1b[0m",
                l.foreign_machines.len(),
                l.personal_root
            ),
            kind: "notes-leak",
            id: String::new(),
            job: String::new(),
            link: String::new(),
            path: String::new(),
        },
        Finding::ShellInit(s) => Row {
            display: format!(
                "\x1b[33m• {} wrappers\x1b[0m  \x1b[2m{} — {}\x1b[0m",
                s.shell, s.problem, s.fix
            ),
            kind: "shell-init",
            id: String::new(),
            job: String::new(),
            link: String::new(),
            // The rc file is the thing to open, so the picker's edit action lands in the right place.
            path: s.rc_path.clone(),
        },
    }
}

/// Header line: total + urgent tally, mirroring the status-bar badge (cron + urgent attn).
fn header(findings: &[Finding]) -> String {
    let urgent = findings
        .iter()
        .filter(|f| {
            matches!(f, Finding::Cron(_))
                || matches!(f, Finding::Staff(_))
                || matches!(f, Finding::Attn(a) if a.severity == "urgent")
        })
        .count();
    format!(
        "attention · {} items ({urgent} urgent) · ↵ open · ctrl-g start agent · ctrl-a ack · ctrl-s snooze · ctrl-x remove · ctrl-p plan · ctrl-r refresh",
        findings.len()
    )
}

/// `bn doctor --pick` — the interactive fzf popup. Falls back to a plain `doctor` dump when fzf
/// is absent, and holds the popup open (Enter) on the empty / no-fzf paths so the message reads.
pub fn run(cfg: &Config) {
    let findings =
        doctor::cached_or_compute(cfg, &today(), doctor::DEFAULT_STALE_DAYS, now_epoch_secs());
    if findings.is_empty() {
        println!("\x1b[1;32m✓ all clear — nothing needs attention\x1b[0m\n");
        hold();
        return;
    }

    let rows: Vec<Row> = findings.iter().map(row_for).collect();
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let head = header(&findings);

    // {2}=kind {3}=id {4}=job {5}=link {6}=path. ctrl-a/ctrl-s act then reload in place; ctrl-r
    // reloads; enter opens (execute keeps the picker up, like the bash original — Esc closes).
    // esc:abort is bound explicitly so Escape always tears the popup down, even after an
    // enter:execute redraw and regardless of any FZF_DEFAULT_OPTS that might rebind esc.
    let mut child = match Command::new("fzf")
        .arg("--ansi")
        .arg("--bind=esc:abort")
        .arg("--reverse")
        .arg("--with-nth=1")
        .arg("--delimiter=\t")
        .arg(format!(
            "--preview={bn} doctor --pick-stage preview {{2}} {{3}} {{4}} {{5}} {{6}}"
        ))
        .arg("--preview-window=right:55%:wrap")
        .arg(format!("--header={head}"))
        .arg(format!(
            "--bind=ctrl-a:execute-silent({bn} doctor --pick-stage ack {{2}} {{3}})+reload({bn} doctor --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-s:execute-silent({bn} doctor --pick-stage snooze {{2}} {{3}})+reload({bn} doctor --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-x:execute-silent({bn} doctor --pick-stage remove {{2}} {{3}})+reload({bn} doctor --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-p:execute-silent({bn} doctor --pick-stage plan {{2}} {{3}})+reload({bn} doctor --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-g:execute-silent({bn} doctor --pick-stage start {{2}} {{3}})+reload({bn} doctor --pick-stage list)"
        ))
        .arg(format!("--bind=ctrl-r:reload({bn} doctor --pick-stage refresh)"))
        .arg(format!(
            "--bind=enter:execute({bn} doctor --pick-stage open {{2}} {{3}} {{4}} {{5}} {{6}})"
        ))
        .stdin(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            // No fzf on PATH — degrade to the plain list, like the bash fallback.
            crate::emit_doctor(cfg, 7, false, false);
            hold();
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let body = rows.iter().map(Row::line).collect::<Vec<_>>().join("\n");
        let _ = stdin.write_all(body.as_bytes());
    }
    let _ = child.wait();
}

/// Dispatch a hidden `--pick-stage` callback. `fields` are the row's hidden columns as fzf
/// passed them: `[kind, id, job, link, path]` (preview/open) or `[kind, id]`
/// (ack/snooze/remove/plan).
pub fn stage(cfg: &Config, stage: PickStage, fields: &[String]) {
    let f = |i: usize| fields.get(i).map(String::as_str).unwrap_or("");
    let (kind, id, job, link, path) = (f(0), f(1), f(2), f(3), f(4));
    match stage {
        PickStage::List => {
            for finding in doctor::cached_or_compute(
                cfg,
                &today(),
                doctor::DEFAULT_STALE_DAYS,
                now_epoch_secs(),
            ) {
                println!("{}", row_for(&finding).line());
            }
        }
        PickStage::Refresh => {
            for finding in doctor::recompute_and_cache(
                cfg,
                &today(),
                doctor::DEFAULT_STALE_DAYS,
                now_epoch_secs(),
            ) {
                println!("{}", row_for(&finding).line());
            }
        }
        PickStage::Preview => preview(kind, id, job, link, path),
        PickStage::Ack => {
            if kind == "attn" && !id.is_empty() {
                let _ = bn_core::attention::ack(id);
                cache_drop_row(kind, id);
            }
        }
        PickStage::Snooze => {
            if kind == "attn" && !id.is_empty() {
                if let Some(until) = add_days(&today(), 1) {
                    let _ = bn_core::attention::snooze(id, &until);
                    cache_drop_row(kind, id);
                }
            }
        }
        PickStage::Remove => {
            if kind == "attn" && !id.is_empty() {
                let _ = bn_core::attention::remove(id);
                cache_drop_row(kind, id);
            }
        }
        PickStage::Plan => {
            if kind == "attn" && !id.is_empty() {
                let cwd = std::env::current_dir().unwrap_or_default();
                let _ = bn_core::attention::plan_item(cfg, &cwd, id, "todo", &today());
                cache_drop_row(kind, id);
            }
        }
        PickStage::Start => {
            if kind == "attn" && !id.is_empty() {
                crate::emit_attn_start(cfg, id, true, false);
                // `start` opens a worktree/agent but leaves the item open, so trimming would be
                // wrong — invalidate instead and let the reload recompute the true list.
                let _ = doctor::clear_cache();
            }
        }
        PickStage::Open => open(kind, id, job, link, path),
    }
}

/// Trim a just-acted attention row from the cached snapshot so the reload serves the updated list
/// without a full recompute — ack/snooze/remove/plan each drop the item from today's list, so the
/// trimmed cache matches what a live recompute would produce. A cache miss is a no-op: the
/// reload's `cached_or_compute` then recomputes (and warms) the truth.
fn cache_drop_row(kind: &str, id: &str) {
    if let Some(mut findings) = doctor::read_cache() {
        findings.retain(|f| {
            let r = row_for(f);
            !(r.kind == kind && r.id.as_str() == id)
        });
        let _ = doctor::write_cache(&findings, doctor::DEFAULT_STALE_DAYS);
    }
}

/// Detail panel for the focused row (the `preview` stage). Args mirror the row's hidden columns.
fn preview(kind: &str, id: &str, job: &str, link: &str, path: &str) {
    match kind {
        "cron" => {
            println!("\x1b[1;36mcron {job}\x1b[0m\n");
            crate::emit_cron_show(job, false);
        }
        "attn" => {
            println!("\x1b[1;36mraised item {id}\x1b[0m");
            // A plan-review row (no link) opens the plan in an nvim popup, not a browser (#651).
            if link.is_empty() && crate::cmd::plan_review::plan_review_id_for_attn(id).is_some() {
                println!(
                    "\n↵ open the plan in an nvim popup · ctrl-a ack · ctrl-s snooze 1d · ctrl-x remove · ctrl-p plan"
                );
                return;
            }
            if !link.is_empty() {
                println!("{link}");
            }
            println!(
                "\n↵ open link · ctrl-g start agent · ctrl-a ack · ctrl-s snooze 1d · ctrl-x remove · ctrl-p plan"
            );
        }
        "note" => match std::fs::read_to_string(path) {
            Ok(body) => {
                for l in body.lines().take(60) {
                    println!("{l}");
                }
            }
            Err(_) => println!("{path}"),
        },
        _ => println!("select an item"),
    }
}

/// Act on the focused row (the `open` stage): its link, the note in `$EDITOR`, or `cron show`. A
/// plan-review row opens the plan in an nvim popup (`bn review edit`) for decision-in-place instead
/// of a browser link (#651).
fn open(kind: &str, id: &str, job: &str, link: &str, path: &str) {
    match kind {
        "attn" => {
            if !id.is_empty() && crate::cmd::plan_review::open_plan_review_popup(id) {
                return;
            }
            if !link.is_empty() {
                crate::open_url(link);
            }
        }
        "note" => {
            if std::path::Path::new(path).is_file() {
                let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
                let _ = Command::new(editor).arg(path).status();
            }
        }
        "cron" => {
            let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
            let out = Command::new(exe).args(["cron", "show", job]).output();
            page(&out.map(|o| o.stdout).unwrap_or_default());
        }
        _ => {}
    }
}

/// Pipe bytes through `$PAGER` (default `less -R`); on a missing pager, print and hold.
/// Shared with the `jump` picker (`bn jump`'s cron dispatch).
pub(crate) fn page(bytes: &[u8]) {
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less -R".to_string());
    let mut parts = pager.split_whitespace();
    let prog = parts.next().unwrap_or("less");
    let mut cmd = Command::new(prog);
    cmd.args(parts).stdin(Stdio::piped());
    if let Ok(mut child) = cmd.spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(bytes);
        }
        let _ = child.wait();
    } else {
        let _ = std::io::stdout().write_all(bytes);
        hold();
    }
}

/// Keep an interactive popup open until the user dismisses it, so a terminal message stays
/// readable before the `-E` popup tears down. Any of Enter, Escape, or EOF (Ctrl-D) closes it.
/// No-op when stdin isn't a tty (scripted / piped). Shared with `jump`.
pub(crate) fn hold() {
    if !std::io::stdin().is_terminal() {
        return;
    }
    print!("Press Enter or Esc to close…");
    let _ = std::io::stdout().flush();
    #[cfg(unix)]
    {
        // Read raw single bytes so a lone Escape (0x1b) — which never arrives in canonical,
        // line-buffered mode — dismisses the popup, matching fzf's esc:abort. Enter (\n/\r)
        // and EOF (read returns 0) close it too.
        if hold_raw() {
            return;
        }
    }
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}

/// A key that should dismiss `hold()`'s "press a key to close" prompt: Enter (`\n`/`\r`) or
/// Escape (`0x1b`). EOF is handled separately by the read returning 0.
#[cfg(unix)]
fn is_dismiss_byte(b: u8) -> bool {
    matches!(b, b'\n' | b'\r' | 0x1b)
}

/// Put the tty in raw mode and block until the user presses Enter, Escape, or sends EOF.
/// Returns `true` once it has handled the keypress (and restored the terminal), or `false`
/// if raw mode couldn't be entered so the caller falls back to line-buffered `read_line`.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "raw terminal input requires the libc termios and read APIs"
)]
fn hold_raw() -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    // SAFETY: termios is a plain C struct we fully initialise via tcgetattr before reading it.
    let mut orig: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut orig) } != 0 {
        return false;
    }
    let mut raw = orig;
    raw.c_lflag &= !(libc::ICANON | libc::ECHO);
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return false;
    }
    let mut byte = [0u8; 1];
    loop {
        let n = unsafe { libc::read(fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };
        if n <= 0 {
            break; // EOF or error
        }
        // Enter (\n \r) or Escape (0x1b) dismiss the popup.
        if is_dismiss_byte(byte[0]) {
            break;
        }
    }
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &orig) };
    println!();
    true
}
