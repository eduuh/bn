//! `bn retro` — the Interviewer's weekly subjective retro (#261).
//!
//! The Interviewer is a *pull* agent: the `staff-interviewer` cron never dispatches an unattended
//! agent, it only raises (and later escalates) the weekly attention item. The session itself is
//! always run by hand here — `bn retro chat` (typed) or `--voice` (spoken), or `bn retro skip` to
//! record an explicit skip. Recording either resolves the open item.
use crate::*;
use bn_core::retro;
use std::io::Write;

/// Prompt on stdout and read one trimmed line of typed input. `None` on EOF (Ctrl-D) so the
/// session can end early, keeping whatever was answered so far.
fn read_answer(prompt: &str) -> Option<String> {
    println!("{prompt}");
    print!("  ");
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    match std::io::stdin().read_line(&mut buf) {
        Ok(0) => None,
        Ok(_) => Some(buf.trim().to_string()),
        Err(_) => None,
    }
}

/// Record + transcribe one spoken answer via the configured voice tool: `$BN_VOICE_TRANSCRIBE` is
/// a command that captures audio and prints the transcript to stdout. Its stdin/stderr are
/// inherited so it can reach the mic and show its own UI; only stdout is captured. `Err` with
/// guidance when unset or the tool fails — the text path stays primary.
fn transcribe() -> Result<String, String> {
    let cmd = std::env::var("BN_VOICE_TRANSCRIBE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or(
            "voice tooling not configured — set BN_VOICE_TRANSCRIBE to a record+transcribe \
             command (it should print the transcript to stdout), or use the text path",
        )?;
    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&cmd)
        .stdin(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("voice: failed to launch `{cmd}`: {e}"))?;
    if !out.status.success() {
        return Err(format!("voice: `{cmd}` exited {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `bn retro chat [--voice]` (also the bare `bn retro`) — run the interactive weekly retro: three
/// fixed questions plus one adaptive follow-up derived from the week's defer/punch/daily history.
pub(crate) fn emit_retro_chat(cfg: &Config, voice: bool, as_json: bool) {
    let today = today();
    let packet = retro::gather_packet(cfg, &today);
    let questions = retro::questions(&packet);
    let mut session = retro::Session::new(&packet.week, &today, voice);

    println!(
        "🎙 Weekly retro — {} · ~10 minutes, {} question{}.",
        packet.week,
        questions.len(),
        if questions.len() == 1 { "" } else { "s" }
    );
    println!("   Answer honestly; there's no advice during the session — synthesis comes after.\n");

    for q in &questions {
        let answer = if voice {
            println!("{}\n   (speak your answer — recording…)", q.prompt);
            match transcribe() {
                Ok(a) => {
                    println!("   → {a}\n");
                    a
                }
                Err(e) => fail(&format!("bn retro chat --voice: {e}")),
            }
        } else {
            match read_answer(&q.prompt) {
                Some(a) => {
                    println!();
                    a
                }
                None => {
                    println!("\n(ended early)");
                    break;
                }
            }
        };
        session.record(q, answer);
    }

    finalize_session(cfg, &today, session, as_json);
}

/// Persist a completed (or partial) session: transcript → tags → validated card → resolve the
/// attention item, then surface any avoided-task blocker (offered, never automatic).
fn finalize_session(cfg: &Config, today: &str, session: retro::Session, as_json: bool) {
    if session.qa.is_empty() {
        if as_json {
            println!(
                "{}",
                Envelope::success(
                    "retro",
                    serde_json::json!({ "recorded": false, "week": session.week })
                )
                .to_json()
            );
        } else {
            println!(
                "Nothing recorded — the retro item stays open. Run `bn retro chat` when ready."
            );
        }
        return;
    }

    let tags = retro::extract_tags(&session);
    let transcript = retro::write_transcript(cfg, &session);
    let card = retro::validate_and_write_card(cfg, &session, &tags, today);
    if let Err(e) = retro::record_session(&session) {
        eprintln!("bn retro: failed to record session: {e}");
    }
    let resolved = retro::resolve(today).unwrap_or(0);

    if as_json {
        println!(
            "{}",
            Envelope::success(
                "retro",
                serde_json::json!({
                    "recorded": true,
                    "week": session.week,
                    "questions": session.qa.len(),
                    "voice": session.voice,
                    "transcript": transcript.as_ref().ok().map(|p| p.display().to_string()),
                    "card": card.as_ref().ok().map(|p| p.display().to_string()),
                    "resolved_items": resolved,
                    "tags": tags,
                })
            )
            .to_json()
        );
        return;
    }

    println!(
        "Recorded {} answer(s) for {}.",
        session.qa.len(),
        session.week
    );
    match &transcript {
        Ok(p) => println!("  transcript: {}", p.display()),
        Err(e) => eprintln!("  transcript failed: {e}"),
    }
    match &card {
        Ok(p) => println!("  summary card: {}", p.display()),
        Err(e) => eprintln!("  card failed: {e}"),
    }
    if resolved > 0 {
        println!("  resolved the retro attention item.");
    }
    // The avoided-task blocker: surfaced from the avoidance/adaptive answers' referenced
    // repos/branches, offered as a `bn add blocker` you run when ready — never recorded for you.
    let refs: Vec<String> = session
        .qa
        .iter()
        .filter(|qa| matches!(qa.axis, retro::Axis::Avoidance | retro::Axis::Adaptive))
        .flat_map(|qa| retro::extract_refs(&qa.answer))
        .collect();
    if let Some(first) = refs.first() {
        println!(
            "  💡 blocked on `{first}`? note it: bn add blocker \"<what's blocking it>\" (on that branch)"
        );
    }
}

/// `bn retro skip` — record an explicit skip for the current ISO week (recorded, not silently
/// missed) and resolve the open attention item.
pub(crate) fn emit_retro_skip(_cfg: &Config, as_json: bool) {
    let today = today();
    let week = bn_core::util::iso_week(&today).unwrap_or_else(|| today.clone());
    if let Err(e) = retro::record_skip(&week, &today) {
        fail(&format!("bn retro skip: {e}"));
    }
    let resolved = retro::resolve(&today).unwrap_or(0);
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "retro",
                serde_json::json!({ "skipped": true, "week": week, "resolved_items": resolved })
            )
            .to_json()
        );
    } else {
        println!("Skipped the weekly retro for {week} (recorded).");
        if resolved > 0 {
            println!("  resolved the retro attention item.");
        }
    }
}

/// `bn retro show [week]` — print a recorded week's transcript (defaults to the current ISO week),
/// or its status when there's no transcript.
pub(crate) fn emit_retro_show(cfg: &Config, week: Option<String>, as_json: bool) {
    let today = today();
    let week =
        week.unwrap_or_else(|| bn_core::util::iso_week(&today).unwrap_or_else(|| today.clone()));
    let status = retro::week_status(&week);
    let path = cfg.retro_file(&week);
    let transcript = std::fs::read_to_string(&path).ok();
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "retro",
                serde_json::json!({
                    "week": week,
                    "status": status,
                    "transcript_path": path.display().to_string(),
                    "has_transcript": transcript.is_some(),
                })
            )
            .to_json()
        );
        return;
    }
    match transcript {
        Some(t) => print!("{t}"),
        None => println!("No retro transcript for {week} (status: {status:?})."),
    }
}

/// `staff-interviewer` cron — the Interviewer's only unattended action: raise the Friday retro
/// attention item, escalate it once on Monday, then drop it (never nag twice). Opt-in via
/// `[retro].enabled`; a manual `bn retro chat` always runs regardless of this flag.
pub(crate) fn job_staff_interviewer(_cfg: &Config, _ctx: Option<&Resolved>) -> i32 {
    if !resolver::JobConfig::load().retro_enabled {
        println!("staff-interviewer: Interviewer disabled — set [retro].enabled to opt in");
        return 0;
    }
    match retro::run_weekly_tick(&today()) {
        Ok(report) => {
            println!(
                "staff-interviewer: {:?} for {} (dropped {} stale)",
                report.action, report.week, report.dropped_stale
            );
            0
        }
        Err(e) => {
            eprintln!("staff-interviewer: {e}");
            1
        }
    }
}
