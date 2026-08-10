//! `bn staff` — plan/code/gather/board/feed for the staff agent roster.
use crate::*;

/// Build the headless dispatch command for a staff run: a fresh (non-resume) `-p <prompt>` turn in
/// the trusted-autonomy mode each harness needs to run unattended. A configured `model` is passed
/// through as `--model` so the roster's per-agent tier is honored, but only when it is a valid id
/// for the harness — an empty or harness-invalid id (e.g. bn's dotted `claude-opus-4.8` to Claude
/// Code) is omitted so the harness uses its own default instead of parking on a model error (#963).
/// `None` for an unknown harness. stdout carries the model's reply (the would-be briefing card) for
/// both harnesses.
pub(crate) fn staff_dispatch_command(
    harness: &str,
    prompt: &str,
    model: Option<&str>,
    session_id: Option<&str>,
) -> Option<std::process::Command> {
    let mut c = match harness {
        "copilot" => {
            let mut c = std::process::Command::new("copilot");
            c.args(["-p", prompt]);
            c.args(bn_core::mcp::Harness::Copilot.yolo_args());
            c
        }
        "claude" => {
            let mut c = std::process::Command::new("claude");
            c.args(["-p", prompt]);
            c.args(bn_core::mcp::Harness::Claude.yolo_args());
            c
        }
        _ => return None,
    };
    // Only pass `--model` when the configured id is one this harness accepts; an id it rejects
    // (e.g. bn's dotted claude-opus-4.8 handed to Claude Code) would park the agent on "model does
    // not exist", so omit it and let the harness use its own default (bn#963).
    if let Some(m) =
        model.and_then(|m| bn_core::mcp::Harness::parse(harness).and_then(|h| h.model_arg(m)))
    {
        c.args(["--model", m]);
    }
    if staff_harness_supports_session_id(harness) {
        if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
            c.args(["--session-id", sid]);
        }
    }
    Some(c)
}

fn staff_harness_supports_session_id(harness: &str) -> bool {
    harness == "claude"
}

/// Run a headless harness dispatch, capturing stdout (the would-be card) and killing it past
/// `wall`. Returns `(exit_success, stdout, combined)` where `combined` is stderr+stdout for
/// [`bn_core::staff::classify_run`]. Reader threads drain both pipes so a chatty harness can't
/// wedge on a full pipe buffer; a blown budget or a spawn error is a non-success carrying the
/// reason in `combined`. The harness binary is resolved through the cron runner's PATH prelude for
/// the same reason `run_gather` needs it: under the queue executor the inherited PATH carries
/// neither `~/.local/bin` (where `claude` installs) nor bn's own bin dir.
pub(crate) fn dispatch_capture(
    mut cmd: std::process::Command,
    wall: Option<std::time::Duration>,
) -> (bool, String, String) {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", crate::cmd::cron::job_path());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, String::new(), format!("spawn failed: {e}")),
    };
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_h = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let deadline = wall.map(|w| Instant::now() + w);
    let (success, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(st)) => break (st.success(), false),
            Ok(None) => {
                if deadline.is_some_and(|dl| Instant::now() >= dl) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (false, true);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => break (false, false),
        }
    };
    let stdout = out_h.join().unwrap_or_default();
    let stderr = err_h.join().unwrap_or_default();
    let combined = format!("{stderr}{stdout}");
    if timed_out {
        return (
            false,
            stdout,
            format!("dispatch exceeded wall budget\n{combined}"),
        );
    }
    (success, stdout, combined)
}

/// Run an agent's `gather` command, returning its stdout as the packet (or a reason on failure).
/// Runs through `/bin/sh -c` with the same PATH prelude the cron runner uses
/// ([`crate::cmd::cron::job_path`]), so a built-in gatherer (`bn staff gather <agent>`) and user
/// scripts alike resolve `bn`, `gh`, etc. under a non-login scheduler shell. The prelude's leading
/// entry is this executable's own directory — on a stock install that is the *only* place `bn`
/// exists, so a hand-rolled `~/.local/bin:~/.bin:$PATH` would leave every gather exiting 127 under
/// the queue executor.
pub(crate) fn run_gather(gather: &str) -> Result<String, String> {
    match std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(gather)
        .env("PATH", crate::cmd::cron::job_path())
        .output()
    {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => Err(format!(
            "gather `{gather}` exited {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("gather `{gather}` failed to launch: {e}")),
    }
}

/// `bn staff run <agent>` and the unified `bn run <name>` agent path. The deterministic half —
/// resolve the agent across roster tiers, gather the packet, load the pinned skill — runs here;
/// validate + route
/// live in [`bn_core::staff::run_pipeline`], which gets the real headless dispatch injected as a
/// closure (classified via the #271 taxonomy so auth/rate-limited runs record distinctly and don't
/// page). Returns a process exit code: 0 on a landed or idempotently re-run card, 1 on any failure
/// (already logged as a `staff_run` event, and — for a genuine `fail` — a warn attention item).
///
/// The human-readable outcome is emitted by [`report_staff_run`], which routes through
/// [`native_job_line`] so the same text lands in the `cron_run` tail for `bn cron show` /
/// `bn cron failures`. Don't replace those with bare `println!`/`eprintln!`.
pub(crate) fn staff_run_code(
    cfg: &Config,
    ctx: Option<&Resolved>,
    name: &str,
    as_json: bool,
) -> i32 {
    use bn_core::staff;
    let today = today();
    let agent = match staff::find(cfg, ctx, name) {
        Ok(Some(agent)) => agent,
        Ok(None) => {
            let msg = format!("bn staff: no gather agent '{name}' in config/agents");
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({"agent": name, "ok": false, "error": msg})
                );
            } else {
                native_job_line(&msg, true);
            }
            return 1;
        }
        Err(error) => {
            let msg = format!("bn staff: invalid agent catalog: {error}");
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({"agent": name, "ok": false, "error": msg})
                );
            } else {
                native_job_line(&msg, true);
            }
            return 1;
        }
    };
    let entry = &agent;

    // Homing (#270): the staff runs on exactly one machine so two boxes never duplicate-review the
    // same PRs into the synced feed. A non-home box no-ops here — loudly the first time, quietly
    // after — and never dispatches or logs a run. A per-agent `only_on` pins one agent to its own
    // box(es), overriding the global home.
    match staff::home_gate(cfg, entry) {
        staff::HomeGate::RunsHere => {}
        staff::HomeGate::NotHome { home } => {
            let loud = staff::note_home_skip_once(&home);
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({"agent": name, "ok": true, "skipped": "not-home", "home": home})
                );
            } else if loud {
                native_job_line(
                    &format!("bn staff: this machine is not the staff home (home = {home}) — '{name}' and every staff run no-op here."),
                    true,
                );
                native_job_line(
                    "  set [staff].home to this machine (or $BN_STAFF_HOME) to re-home the staff.",
                    true,
                );
            } else {
                native_job_line(
                    &format!("staff {name}: skipped — not the staff home ({home})"),
                    false,
                );
            }
            return 0;
        }
    }

    let packet = run_gather(entry.gather.as_deref().expect("staff agent has gather"));
    let harness = entry
        .harness
        .clone()
        .unwrap_or_else(|| "copilot".to_string());
    let sid = staff_harness_supports_session_id(&harness).then(crate::cmd::dispatch::gen_uuid);
    if let Some(sid) = &sid {
        bn_core::cost::attribute(
            sid,
            bn_core::cost::WorkClass::Scheduled,
            "staff",
            &entry.name,
        );
    }

    let report = staff::run_pipeline(
        cfg,
        entry,
        packet,
        |prompt| {
            let Some(cmd) = staff_dispatch_command(&harness, prompt, None, sid.as_deref()) else {
                eprintln!("bn staff: unknown harness '{harness}' (use copilot | claude)");
                return staff::Dispatched::Failed(
                    staff::RunStatus::Fail,
                    format!("unknown harness '{harness}' (use copilot | claude)"),
                );
            };
            let (success, stdout, combined) = dispatch_capture(cmd, None);
            match staff::classify_run(success, &combined) {
                staff::RunStatus::Ok => staff::Dispatched::Output(stdout),
                other => staff::Dispatched::Failed(other, staff::failure_detail(&combined)),
            }
        },
        &today,
    );

    if as_json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        report_staff_run(name, &today, &report);
    }
    if report.ok || report.status == staff::RunStatus::Skipped {
        0
    } else {
        1
    }
}

/// The human-readable outcome of one staff run.
///
/// Every line goes through [`native_job_line`] rather than `println!`/`eprintln!`, which is what
/// puts the text in the `cron_run` event tail as well as on the terminal. Under `bn run <agent>`
/// this function executes inside [`crate::cmd::jobs::run_native_job`]'s closure, and that closure
/// builds its tail *solely* from the `native_job_line` buffer — so a bare `eprintln!` here reaches
/// a terminal nobody is watching and records an empty tail. That is not hypothetical: `scout`
/// failed 28 consecutive scheduled runs on a `bn: command not found` gather while
/// `bn cron failures` displayed exit 1 and nothing else, and the cause was only recoverable by
/// grepping the sibling `staff_run` row out of the raw event log.
fn report_staff_run(name: &str, today: &str, report: &bn_core::staff::RunReport) {
    use bn_core::staff;

    if report.status == staff::RunStatus::Skipped {
        // A policy skip — budget shed or backpressure self-throttle (#269). Both ride `note` with
        // the one-line reason to surface *once*; a silent throttle repeat carries no note, so the
        // agent degrades toward silence rather than repeating its pause notice.
        if let Some(note) = &report.note {
            native_job_line(&format!("staff {name}: {note}"), false);
        }
        return;
    }
    if !report.ok {
        native_job_line(
            &format!(
                "staff {name}: {} [{}]",
                report.error.as_deref().unwrap_or("failed"),
                report.status.as_str()
            ),
            true,
        );
        return;
    }
    if report.rerun {
        native_job_line(
            &format!(
                "staff {name}: card for {today} already existed — refreshed, items not re-routed"
            ),
            false,
        );
    } else {
        let r = &report.routed;
        native_job_line(
            &format!(
                "staff {name}: card landed → attn {} · daily {} · note {} · research {} · dojo {}",
                r.attn, r.daily, r.note, r.research, r.dojo
            ),
            false,
        );
    }
    native_job_line(
        &format!(
            "  cost: ~{} tokens · {}",
            report.tokens,
            fmt_wall_ms(report.wall_ms)
        ),
        false,
    );
    if let Some(p) = &report.card_path {
        native_job_line(&format!("  {}", p.display()), false);
    }
}

/// `bn staff plan <agent>` — the dry-run: gather the packet and report the sinks the agent may
/// route into, with NO dispatch and nothing landed. Exit 0 when the packet gathered as valid JSON,
/// 1 otherwise (so a broken gather is caught before it ever reaches a scheduled run).
pub(crate) fn emit_staff_plan(
    cfg: &Config,
    ctx: Option<&Resolved>,
    name: &str,
    as_json: bool,
) -> i32 {
    use bn_core::staff;
    let agent = match staff::find(cfg, ctx, name) {
        Ok(Some(agent)) => agent,
        Ok(None) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({"agent": name, "ok": false, "error": "no such agent"})
                );
            } else {
                eprintln!("bn staff: no gather agent '{name}' in config/agents");
            }
            return 1;
        }
        Err(error) => {
            eprintln!("bn staff: invalid agent catalog: {error}");
            return 1;
        }
    };
    let entry = &agent;
    let packet = run_gather(entry.gather.as_deref().expect("staff agent has gather"));
    let harness = entry.harness.as_deref().unwrap_or("copilot");
    let packet_json = packet
        .as_ref()
        .ok()
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());

    if as_json {
        println!(
            "{}",
            serde_json::json!({
                "agent": name,
                "gather": entry.gather,
                "prompt_source": entry.path,
                "harness": harness,
                "sinks": entry.sinks,
                "packet": packet_json,
                "packet_valid_json": packet_json.is_some(),
                "gather_error": packet.as_ref().err(),
            })
        );
        return if packet_json.is_some() { 0 } else { 1 };
    }

    println!("staff plan: {name} (dry-run — no dispatch, nothing lands)");
    println!("  gather:  {}", entry.gather.as_deref().unwrap_or(""));
    println!("  prompt:  {}", entry.path.display());
    println!("  harness: {harness}");
    println!(
        "  sinks:   {}",
        if entry.sinks.is_empty() {
            "(none declared — no items could route)".to_string()
        } else {
            entry.sinks.join(", ")
        }
    );
    match &packet {
        Ok(p) => {
            println!("  packet:");
            match serde_json::from_str::<serde_json::Value>(p) {
                Ok(v) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&v).unwrap_or_else(|_| p.clone())
                    );
                    0
                }
                Err(_) => {
                    println!("{}", p.trim());
                    eprintln!("  warning: packet is not valid JSON — a real run would fail closed");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("  gather failed: {e}");
            1
        }
    }
}

/// `bn staff gather <agent>` — print the built-in deterministic packet JSON for `agent` to stdout.
/// The shipped `demo` gatherer (#256), the `scout` radar (#262), the `sentinel`/`mentor` packets,
/// and the `goals-align` fold (#563) build in; the `standup` fold (#566) too; other real agents
/// supply their own `gather` command
/// in `config/agents/*.md`. Exit 0 on a printed packet, 2 for an unknown gatherer.
pub(crate) fn emit_staff_gather(cfg: &Config, agent: &str) -> i32 {
    match agent {
        "demo" => {
            println!("{}", bn_core::staff::demo_packet(cfg, &today()));
            0
        }
        "scout" => {
            println!(
                "{}",
                bn_core::scout::gather_packet_json(cfg, &bn_core::runner::RealRunner, &today())
            );
            0
        }
        "sentinel" => crate::cmd::sentinel::emit_sentinel_gather(cfg),
        "goals-align" => {
            println!(
                "{}",
                bn_core::goals_align::gather_packet_json(cfg, &today())
            );
            0
        }
        "standup" => {
            println!("{}", bn_core::standup::gather_packet_json(cfg, &today()));
            0
        }
        "staff-chief-am" => {
            let packet =
                crate::cmd::chief::gather_chief_packet(cfg, bn_core::chief::Slot::Am, &today());
            println!(
                "{}",
                serde_json::to_string(&packet).unwrap_or_else(|_| "{}".into())
            );
            0
        }
        "staff-chief-pm" => {
            let packet =
                crate::cmd::chief::gather_chief_packet(cfg, bn_core::chief::Slot::Pm, &today());
            println!(
                "{}",
                serde_json::to_string(&packet).unwrap_or_else(|_| "{}".into())
            );
            0
        }
        "staff-mentor" | "mentor" => {
            // The Mentor's deterministic packet: yesterday's merged own-PRs discovered offline,
            // their diffs fetched (the only network step). See `bn_core::mentor`.
            let packet = gather_mentor_packet(cfg, &bn_core::runner::RealRunner, &today());
            match packet.to_json() {
                Ok(json) => {
                    println!("{json}");
                    0
                }
                Err(e) => {
                    eprintln!("bn staff gather mentor: serialize packet: {e}");
                    1
                }
            }
        }
        other => {
            eprintln!(
                "bn staff gather: no built-in gatherer for '{other}' (ships: demo, scout, sentinel, staff-mentor, staff-chief-am, staff-chief-pm, goals-align, standup; other agents define their own `gather` command)"
            );
            2
        }
    }
}

/// Bare `bn staff` — the roster (config/agents/*.md across tiers) joined with each agent's last-run health
/// (the `staff_run` event log) and whether it has an unread card. The observability half of the
/// framework: what's declared, when it last ran, and whether that run was ok / fail / auth /
/// rate-limited.
pub(crate) fn emit_staff_board(cfg: &Config, ctx: Option<&Resolved>, as_json: bool) {
    use bn_core::{staff, staff_schedule};
    if as_json {
        // The shared fold (#273 taxonomy + #269 backpressure state) — the same view the
        // dashboard's `GET /api/staff/board` serializes.
        match staff::board_view(cfg, ctx) {
            Ok(view) => println!("{view}"),
            Err(error) => fail(&format!("bn staff: invalid agent catalog: {error}")),
        }
        return;
    }
    let agents = staff::discover(cfg, ctx)
        .unwrap_or_else(|error| fail(&format!("bn staff: invalid agent catalog: {error}")));
    let last = staff::last_runs();
    let unread: std::collections::HashSet<String> = staff::list_cards(cfg)
        .into_iter()
        .filter(|c| !c.read)
        .map(|c| c.agent)
        .collect();
    let now = today_epoch();
    // Missed-run detection (#273): join each agent's schedule with the ledger. A slot that fired
    // past the grace window with no run at/after it is a missed run — distinct from a silent run
    // (which recorded a card) and a skip (which recorded a `skipped` row).
    let missed: std::collections::HashMap<String, i64> =
        staff_schedule::missed_runs(&agents, &last, now, staff_schedule::DEFAULT_MISS_GRACE_SECS)
            .into_iter()
            .map(|m| (m.agent, m.scheduled))
            .collect();

    if agents.is_empty() {
        println!("No staff agents. Add a .md file with `gather:` in config/agents/.");
        return;
    }
    println!(
        "STAFF · {} agent{}",
        agents.len(),
        if agents.len() == 1 { "" } else { "s" }
    );
    for a in &agents {
        let lr = last.get(&a.name);
        let missed_since = missed.get(&a.name).copied();
        let health = staff_schedule::health_label(lr, missed_since.is_some());
        // Colour the taxonomy word: red = a hard problem (missed / failed / auth), yellow = a
        // transient policy state (rate-limited / skipped), dim = quiet-by-design (silent / never),
        // green = a card landed. Padding is on the plain word so columns still line up under it.
        let colour = match health {
            "missed" | "fail" | "auth" => "\x1b[1;31m",
            "rate-limited" => "\x1b[33m",
            "ok-silent" | "skipped" | "never" => "\x1b[2m",
            _ => "\x1b[32m",
        };
        let when = match (missed_since, lr) {
            (Some(slot), _) => format!("missed {} ago", ago(now - slot)),
            (None, Some(r)) => format!("{} ago", ago(now - r.ts)),
            (None, None) => "never run".to_string(),
        };
        let mark = if unread.contains(&a.name) { " ●" } else { "" };
        println!(
            "  {:<16} {:<20} {colour}{:<13}\x1b[0m {}{}",
            a.name,
            a.on_calendar.as_deref().unwrap_or(""),
            health,
            when,
            mark
        );
        if let Some(slot) = missed_since {
            // Missed silence is death, not design — say so, and never conflate it with a run that
            // recorded a card and chose to stay quiet.
            println!(
                "       └ scheduled run never fired (no card, no run) — expected {} {}",
                bn_core::util::date_of(slot),
                bn_core::util::clock_hm(slot)
            );
        } else if let Some(r) = lr {
            if r.status != staff::RunStatus::Ok {
                if let Some(err) = &r.error {
                    println!("       └ {err}");
                }
            }
        }
    }
}

/// Styled status-bar fragment for unread briefing cards: `📮N` for today's unread count, a trailing
/// space so it sits cleanly beside the agent/fleet badges. Prints nothing when there are none —
/// silence is the default state — and nothing during a `bn focus` block, so the ambient badge never
/// becomes a new interruption channel (genuinely urgent card items still surface via `bn attn
/// badge`, which the attn path lets through focus). Counterpart of [`emit_attn_badge`]; for tmux
/// `#(…)`. Cheap by design: [`bn_core::staff::unread_count`] reads only today's feed dir + the
/// local read-marks, so it's safe on a status interval.
pub(crate) fn emit_staff_badge(cfg: &Config) {
    use bn_core::clock::Clock;
    let now = bn_core::clock::RealClock.now_epoch_secs();
    if let Some(frag) = staff_badge_fragment(cfg, now, &today()) {
        print!("{frag}");
    }
}

/// The badge decision, factored out so its focus/empty gating is testable without capturing stdout:
/// `Some("📮N …")` when today has unread cards and no focus block is active, `None` (badge hidden)
/// otherwise.
pub(crate) fn staff_badge_fragment(cfg: &Config, now: i64, today: &str) -> Option<String> {
    if bn_core::focus::is_active(now) {
        return None;
    }
    let n = bn_core::staff::unread_count(cfg, today);
    (n > 0).then(|| format!("#[fg=default]\u{1f4ee}{n} "))
}

/// `bn staff feed [<agent>|<date>]` — briefing cards, unread-first. Bare `bn staff feed` is the
/// index (marks nothing); a filter matching an agent name or a `YYYY-MM-DD` date renders the full
/// matching card(s) and marks each read (the "read on view" semantics). `--json` envelopes the
/// list of card refs (index) or the full cards (view) for the nvim/tmux surfaces.
pub(crate) fn emit_staff_feed(cfg: &Config, filter: Option<&str>, as_json: bool) {
    use bn_core::staff;
    let cards = staff::list_cards(cfg);
    let matched: Vec<&staff::CardRef> = match filter {
        None => cards.iter().collect(),
        Some(f) => cards
            .iter()
            .filter(|c| c.agent == f || c.date == f)
            .collect(),
    };

    // View mode: a filter renders full card(s) and marks them read.
    if let Some(f) = filter {
        let full: Vec<staff::BriefingCard> = matched
            .iter()
            .filter_map(|c| staff::read_card(&c.path))
            .inspect(|card| {
                let _ = staff::mark_read(&card.date, &card.agent);
            })
            .collect();
        if as_json {
            println!("{}", serde_json::json!({ "cards": full }));
        } else if full.is_empty() {
            println!("No cards match '{f}'.");
        } else {
            for card in &full {
                print!("{}", staff::render_card_md(card));
                println!();
            }
        }
        return;
    }

    // Index mode: list unread-first, mark nothing.
    if as_json {
        println!("{}", serde_json::json!({ "cards": matched }));
        return;
    }
    if matched.is_empty() {
        println!("No briefing cards yet — staff agents write one per run (see `bn staff`).");
        return;
    }
    let unread = matched.iter().filter(|c| !c.read).count();
    println!(
        "FEED · {} card{} · {} unread",
        matched.len(),
        if matched.len() == 1 { "" } else { "s" },
        unread
    );
    for c in &matched {
        let mark = if c.read { " " } else { "●" };
        println!("  {} {}  {:<16} {}", mark, c.date, c.agent, c.headline);
    }
}

/// Compact human token count: `48.2k`, `1.3m`, or the bare number under 1000.
pub(crate) fn fmt_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}m", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.1}k", t as f64 / 1_000.0)
    } else {
        t.to_string()
    }
}

/// Compact wall-clock from milliseconds: `3m12s`, `4s`, or `250ms`.
pub(crate) fn fmt_wall_ms(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if ms >= 1000 {
        format!("{secs}s")
    } else {
        format!("{ms}ms")
    }
}

/// `bn staff cost [<month>]` — staff spend by agent for a month.
pub(crate) fn emit_staff_cost(
    cfg: &Config,
    ctx: Option<&Resolved>,
    month: Option<&str>,
    as_json: bool,
) {
    use bn_core::staff;
    let month = month
        .map(str::to_string)
        .unwrap_or_else(|| staff::month_of(&today()).to_string());
    let report = staff::cost_report(cfg, ctx, &month)
        .unwrap_or_else(|error| fail(&format!("bn staff cost: invalid agent catalog: {error}")));

    if as_json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
        return;
    }

    if report.agents.is_empty() {
        println!("STAFF COST · {} — no staff runs.", report.month);
        return;
    }
    println!("STAFF COST · {} · month-to-date", report.month);
    println!(
        "  {:<16} {:>6} {:>9} {:>8}",
        "AGENT", "RUNS", "TOKENS", "WALL"
    );
    for a in &report.agents {
        let runs = if a.skipped > 0 {
            format!("{}·{} skip", a.runs, a.skipped)
        } else {
            a.runs.to_string()
        };
        println!(
            "  {:<16} {:>6} {:>9} {:>8}",
            a.agent,
            runs,
            format!("~{}", fmt_tokens(a.tokens)),
            fmt_wall_ms(a.wall_ms),
        );
    }
    println!(
        "  total ~{} tokens · {}",
        fmt_tokens(report.total_tokens),
        fmt_wall_ms(report.total_wall_ms)
    );
}

/// Human-readable bytes (`3.2 MB`, `948 KB`, `12 B`) for the retention growth report.
fn fmt_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let n = n as f64;
    if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.0} KB", n / KB)
    } else {
        format!("{n:.0} B")
    }
}

/// `bn staff archive [--dry-run]` — apply the #275 store-retention policy: compact fully-elapsed
/// quarters of feed cards into a per-quarter headline index and roll up `staff_run` ledger rows
/// older than a year into monthly aggregates. Chronicles and retro transcripts are exempt. Always
/// prints the growth measurement first (so the policy can be sized to reality); `--dry-run` reports
/// what would change without writing or deleting anything.
pub(crate) fn emit_staff_archive(cfg: &Config, dry_run: bool, as_json: bool) {
    use bn_core::staff_retention as ret;
    let report = ret::archive_now(cfg, dry_run);

    if as_json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
        return;
    }

    let head = if dry_run {
        "STAFF ARCHIVE · dry-run — nothing written"
    } else {
        "STAFF ARCHIVE"
    };
    println!("{head}  ({})", report.today);

    // Growth measurement — the numbers to size the policy against.
    let f = &report.feed;
    print!("  feed:   {} cards · {}", f.cards, fmt_bytes(f.bytes));
    match (&f.oldest, &f.newest) {
        (Some(o), Some(n)) if o != n => println!(" · {o} → {n}"),
        (Some(o), _) => println!(" · {o}"),
        _ => println!(),
    }
    let l = &report.ledger;
    print!("  ledger: {} staff_run events", l.events);
    if let Some(o) = &l.oldest {
        print!(" · since {o}");
    }
    println!(" · {} rollup-eligible", l.rolluppable);

    // What compaction did (or would do).
    if report.compactions.is_empty() {
        println!("  compaction: no elapsed quarters to compact");
    } else {
        let verb = if dry_run {
            "would compact"
        } else {
            "compacted"
        };
        for c in &report.compactions {
            println!(
                "  {verb} {} — {} card(s) → {}",
                c.quarter,
                c.cards,
                c.index.display()
            );
        }
    }

    // What rollup did (or would do).
    let r = &report.rollup;
    if !r.changed {
        println!("  rollup: ledger already compacted — no runs older than a year to fold");
    } else {
        let verb = if dry_run {
            "would roll up"
        } else {
            "rolled up"
        };
        println!(
            "  {verb} {} run(s) → {} monthly aggregate(s) across {}",
            r.runs_folded,
            r.aggregates,
            if r.months.is_empty() {
                "—".to_string()
            } else {
                r.months.join(", ")
            }
        );
    }

    println!("  exempt: chronicles + retro transcripts (kept forever)");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A staff gather runs under the queue executor, whose PATH carries neither `~/.local/bin` nor
    /// bn's own bin dir. Leading the subshell PATH with this executable's directory is what keeps
    /// `bn staff gather <agent>` from exiting 127 there, so assert the prelude rather than a
    /// hand-rolled equivalent.
    #[test]
    fn gather_subshell_path_leads_with_the_executable_directory() {
        let exe_dir = std::env::current_exe()
            .expect("current exe")
            .parent()
            .expect("exe dir")
            .to_string_lossy()
            .into_owned();
        let observed = run_gather("printf %s \"$PATH\"").expect("gather echoes PATH");
        assert_eq!(observed.split(':').next(), Some(exe_dir.as_str()));
        assert_eq!(observed, crate::cmd::cron::job_path());
    }

    /// The dispatch half of the same prelude: a headless harness spawn inherits the queue
    /// executor's PATH, so `claude` (in `~/.local/bin`) and `bn` (in its own bin dir) are both
    /// unreachable without it. Asserted as an exact match rather than a prefix, because a
    /// hand-rolled `~/.local/bin:~/.bin:$PATH` that merely happened to lead with the exe dir is
    /// exactly the regression this pins — it is what left every `goals-align` gather at exit 127.
    #[test]
    fn dispatch_capture_runs_the_child_under_the_job_path_prelude() {
        let exe_dir = std::env::current_exe()
            .expect("current exe")
            .parent()
            .expect("exe dir")
            .to_string_lossy()
            .into_owned();
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "printf %s \"$PATH\""]);
        let (ok, stdout, combined) = dispatch_capture(cmd, None);
        assert!(ok, "the echo child should exit 0: {combined}");
        assert_eq!(stdout.split(':').next(), Some(exe_dir.as_str()));
        assert_eq!(stdout, crate::cmd::cron::job_path());
    }

    /// Run `f` and return exactly the text it emitted through `native_job_line` — the same buffer
    /// `run_native_job` turns into the `cron_run` event tail. Lets these tests assert what a
    /// scheduled run *would* record without standing up a real cron run. The buffer is
    /// thread-local, so tests running in parallel do not see each other's output.
    fn captured_tail(f: impl FnOnce()) -> String {
        crate::cmd::jobs::reset_native_job_output();
        f();
        crate::cmd::jobs::take_native_job_output()
    }

    fn run_report(status: bn_core::staff::RunStatus, error: Option<&str>) -> bn_core::staff::RunReport {
        bn_core::staff::RunReport {
            agent: "scout".to_string(),
            ok: status == bn_core::staff::RunStatus::Ok,
            status,
            error: error.map(str::to_string),
            card_path: None,
            routed: bn_core::staff::RouteSummary::default(),
            rerun: false,
            tokens: 0,
            wall_ms: 0,
            note: None,
            silent: false,
        }
    }

    /// The regression that made `scout`'s 28 consecutive failures unreadable: the run reported its
    /// cause to stderr only, so `run_native_job` recorded an empty tail and `bn cron failures`
    /// showed exit 1 with no reason. The failure text must reach the `native_job_line` buffer.
    #[test]
    fn a_failed_staff_run_records_its_cause_in_the_cron_tail() {
        let report = run_report(
            bn_core::staff::RunStatus::Fail,
            Some("gather failed: gather `bn staff gather scout` exited 127: bn: command not found"),
        );
        let tail = captured_tail(|| {
            report_staff_run("scout", "2026-08-08", &report);
        });
        assert!(
            tail.contains("exited 127") && tail.contains("bn: command not found"),
            "the gather failure must be recoverable from the cron tail alone, got: {tail:?}"
        );
        assert!(tail.contains("[fail]"), "{tail:?}");
    }

    /// A failure with no error string still has to leave a row worth reading.
    #[test]
    fn a_failed_staff_run_with_no_detail_still_records_its_status() {
        let report = run_report(bn_core::staff::RunStatus::Auth, None);
        let tail = captured_tail(|| {
            report_staff_run("scout", "2026-08-08", &report);
        });
        assert!(tail.contains("staff scout: failed [auth]"), "{tail:?}");
    }

    /// A green run's tail is what `bn cron show <agent>` prints, so the landed-card summary has to
    /// be captured too — not just the failure path.
    #[test]
    fn a_landed_staff_run_records_its_summary_in_the_cron_tail() {
        let report = run_report(bn_core::staff::RunStatus::Ok, None);
        let tail = captured_tail(|| {
            report_staff_run("scout", "2026-08-08", &report);
        });
        assert!(tail.contains("staff scout: card landed"), "{tail:?}");
        assert!(tail.contains("cost: ~0 tokens"), "{tail:?}");
    }

    /// A policy skip carries its one-line reason; a silent throttle repeat (`note: None`) stays
    /// silent, so the tail must be empty rather than carrying a manufactured line.
    #[test]
    fn a_skipped_staff_run_records_only_its_note() {
        let mut report = run_report(bn_core::staff::RunStatus::Skipped, None);
        report.note = Some("pausing — 3 unread cards".to_string());
        let tail = captured_tail(|| {
            report_staff_run("scout", "2026-08-08", &report);
        });
        assert!(tail.contains("pausing — 3 unread cards"), "{tail:?}");

        report.note = None;
        let silent = captured_tail(|| {
            report_staff_run("scout", "2026-08-08", &report);
        });
        assert!(silent.is_empty(), "{silent:?}");
    }

    #[test]
    fn gather_failure_carries_the_exit_code_and_stderr() {
        let err = run_gather("echo boom >&2; exit 127").expect_err("non-zero gather fails");
        assert!(err.contains("exited 127"), "{err}");
        assert!(err.contains("boom"), "{err}");
    }
}
