//! The two ends of a compaction: `prep` (recap + handoff, logged) and `audit` (re-orient from the
//! note, recap and proof; log how it came back).

use super::CMD;
use crate::*;
use bn_core::compact::{self, Entry, Phase};
use bn_core::proof::ItemState;
use serde_json::{json, Value};
use std::path::PathBuf;

/// What a harness hook passes on stdin: `session_id`, `transcript_path`, and the `trigger`
/// (PreCompact: `manual`/`auto`) or `source` (SessionStart: `compact`, …).
#[derive(Debug, Default)]
struct HookInput {
    session: Option<String>,
    transcript: Option<PathBuf>,
    trigger: Option<String>,
}

fn read_hook_input() -> HookInput {
    // Bounded, like the memory hook's read: a harness closes stdin at once, and a hook must never
    // hang the harness waiting on a pipe that stays open.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
        let _ = tx.send(buf);
    });
    let buf = rx
        .recv_timeout(std::time::Duration::from_millis(500))
        .unwrap_or_default();
    let v: Value = serde_json::from_str(buf.trim()).unwrap_or(Value::Null);
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    HookInput {
        session: s("session_id"),
        transcript: s("transcript_path").map(PathBuf::from),
        trigger: s("trigger").or_else(|| s("source")),
    }
}

/// Context use from a transcript file, when the hook named one.
fn ctx_pct(transcript: Option<&PathBuf>) -> Option<u8> {
    let text = std::fs::read_to_string(transcript?).ok()?;
    let lines: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    bn_core::agent::context_usage::from_claude_lines(&lines).pct
}

/// Compose and write the recap for `ctx`.
pub(super) fn recap(
    cfg: &Config,
    ctx: &Resolved,
    words: &str,
    session: Option<&str>,
) -> std::io::Result<PathBuf> {
    let text = compact::compose(cfg, ctx, words, &bn_core::util::now_stamp());
    compact::write_recap(cfg, ctx, &text, &bn_core::util::now_filestamp(), session)
}

/// Write the recap and handoff and log the prep. Returns the recap path (if written) and any
/// problems, one line each.
pub(super) fn prep_now(
    cfg: &Config,
    ctx: &Resolved,
    words: &str,
    session: Option<String>,
    trigger: &str,
    pct: Option<u8>,
) -> (Option<PathBuf>, Vec<String>) {
    let recap_ok = recap(cfg, ctx, words, session.as_deref());
    // Carries forward the author layers of any earlier handoff (see `handoff::merge`).
    let handoff_ok = bn_core::handoff::write(cfg, ctx, &today(), &Default::default());
    let entry = Entry {
        ts: bn_core::util::now_epoch_secs(),
        phase: Phase::Prep,
        session,
        trigger: Some(trigger.to_string()),
        ctx_pct: pct,
        recap: recap_ok.is_ok(),
        outcome: None,
    };
    let logged = compact::append(cfg, ctx, &entry);
    let problems = [
        recap_ok.as_ref().err().map(|e| format!("recap: {e}")),
        handoff_ok.as_ref().err().map(|e| format!("handoff: {e}")),
        logged.as_ref().err().map(|e| format!("log: {e}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (recap_ok.ok(), problems)
}

pub(super) fn run_prep(
    cfg: &Config,
    ctx: &Resolved,
    text: Option<String>,
    dry_run: bool,
    hook: bool,
    json: bool,
) {
    let input = if hook { read_hook_input() } else { HookInput::default() };
    let words = text.unwrap_or_default();
    if dry_run {
        let preview = compact::compose(cfg, ctx, &words, &bn_core::util::now_stamp());
        if json {
            println!("{}", Envelope::success(CMD, json!({"dry_run": true, "recap": preview})).to_json());
        } else {
            println!("Would write RECAP.md and a handoff, then log the prep. The recap:\n");
            print!("{preview}");
        }
        return;
    }
    let pct = ctx_pct(input.transcript.as_ref());
    let trigger = input.trigger.unwrap_or_else(|| "manual".into());
    let (recap_path, problems) = prep_now(cfg, ctx, &words, input.session, &trigger, pct);
    if json {
        let data = json!({"recap": recap_path, "ctx_pct": pct, "problems": problems});
        println!("{}", Envelope::success(CMD, data).to_json());
    } else {
        println!(
            "bn: prepared {}/{} for compaction — RECAP.md and handoff written{}",
            ctx.repo,
            ctx.branch,
            pct.map(|p| format!(" (context {p}%)")).unwrap_or_default()
        );
        for p in &problems {
            eprintln!("bn compact prep: {p}");
        }
    }
    if !hook && !problems.is_empty() {
        std::process::exit(1);
    }
}

/// The banner the compacted session reads first.
const BANNER: &str = "You just came back from a compaction. What came back believes it knows \
everything and does not. Before acting, check each thing you think you know against the note, \
the recap and the proof below — and re-read any file before you edit it.";

pub(super) fn run_audit(cfg: &Config, ctx: &Resolved, hook: bool, json: bool) {
    let input = if hook { read_hook_input() } else { HookInput::default() };
    let note_readable = bn_core::store::read(&cfg.note_file(ctx)).is_ok();
    let now = bn_core::util::now_epoch_secs();
    let outcome = compact::outcome_for_restore(&compact::entries(cfg, ctx), now, note_readable);
    let entry = Entry {
        ts: now,
        phase: Phase::Restore,
        session: input.session,
        trigger: input.trigger,
        ctx_pct: None,
        recap: false,
        outcome: Some(outcome),
    };
    let _ = compact::append(cfg, ctx, &entry);

    let orientation = bn_core::handoff::read(cfg, ctx)
        .map(|h| bn_core::handoff::render_orientation(&h))
        .unwrap_or_default();
    let recap = std::fs::read_to_string(compact::recap_path(cfg, ctx)).unwrap_or_default();
    let (readiness, _) = bn_core::proof::readiness_now(cfg, ctx, None);
    let plan = cfg.active_plan_file(ctx);
    if json {
        let data = json!({"outcome": outcome, "orientation": orientation, "recap": recap,
            "readiness": readiness, "plan": plan.exists().then_some(plan)});
        println!("{}", Envelope::success(CMD, data).to_json());
        return;
    }
    println!("{BANNER}\n");
    if !orientation.is_empty() {
        print!("{orientation}");
    }
    if !recap.is_empty() {
        println!("\n{}", recap.trim_end());
    } else {
        println!("\n(no recap was written before this compaction — rebuilt from the note)");
    }
    let unproven: Vec<String> = readiness
        .items
        .iter()
        .filter(|i| !matches!(i.state, ItemState::Accepted { .. }))
        .map(|i| format!("{} {}", i.id, i.text))
        .collect();
    if !unproven.is_empty() {
        println!("\nNot yet proven (don't assume these are done): {}", unproven.join("; "));
    }
    if plan.exists() {
        println!("Active plan: {}", plan.display());
    }
}
