//! `bn promote`/`bn trust` — publish a branch + fold-in folder-trust settings.
use crate::*;

/// `bn trust-folder` — pre-grant a harness's folder trust for `path` (default cwd) so a dispatched
/// agent boots without the "trust this folder?" prompt. Edits the harness config(s) via the pure
/// `bn_core::trust` helpers. Best-effort: a missing/one-harness config isn't an error.
/// Apply harness folder-trust for `path` (claude | copilot | both), editing the harness config(s)
/// via the pure `bn_core::trust` helpers. Returns which harnesses were trusted. Best-effort: a
/// missing/one-harness config isn't fatal. Shared by `bn trust-folder` and dispatch's `--yolo`.
pub(crate) fn apply_folder_trust(path: &str, harness: &str) -> Vec<&'static str> {
    let home = std::env::var("HOME").unwrap_or_default();
    let want = |h: &str| harness == "both" || harness == h;
    let mut trusted: Vec<&'static str> = Vec::new();
    if want("claude") {
        let p = format!("{home}/.claude.json");
        let cur = std::fs::read_to_string(&p).unwrap_or_default();
        match bn_core::trust::claude_trust(&cur, path) {
            Ok(new) => {
                if new != cur {
                    if let Err(e) = std::fs::write(&p, new) {
                        eprintln!("bn trust-folder: writing {p}: {e}");
                    }
                }
                trusted.push("claude");
            }
            Err(e) => eprintln!("bn trust-folder: {e}"),
        }
    }
    if want("copilot") {
        let p = format!("{home}/.copilot/config.json");
        let cur = std::fs::read_to_string(&p).unwrap_or_default();
        match bn_core::trust::copilot_trust(&cur, path) {
            Ok((new, changed)) => {
                if changed {
                    if let Err(e) = std::fs::write(&p, new) {
                        eprintln!("bn trust-folder: writing {p}: {e}");
                    }
                }
                trusted.push("copilot");
            }
            Err(e) => eprintln!("bn trust-folder: {e}"),
        }
    }
    trusted
}

pub(crate) fn emit_trust_folder(path: Option<String>, harness: &str, as_json: bool) {
    let path = path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    // Only ever persist a real absolute folder path. `canonicalize` makes an *existing* relative
    // path absolute; if it's still relative here the target doesn't exist — e.g.
    // `bn trust-folder claude`, where `claude` was meant as `--harness`, not a folder. Writing it
    // would leak a bare token into the harness trust lists (copilot's `trustedFolders`), so refuse.
    if !path.is_absolute() {
        let msg = format!(
            "bn trust-folder: '{}' is not an absolute folder path — pass a real directory (folder trust only accepts absolute paths; the harness goes in --harness)",
            path.display()
        );
        if as_json {
            println!(
                "{}",
                Envelope::failure("trust_folder", "not_absolute", msg).to_json()
            );
            return;
        }
        fail(&msg);
    }
    let path = path.to_string_lossy().to_string();
    let trusted = apply_folder_trust(&path, harness);

    if as_json {
        let env = Envelope::success(
            "trust_folder",
            serde_json::json!({ "path": path, "harnesses": trusted }),
        );
        println!("{}", env.to_json());
    } else {
        println!("trusted {path} for: {}", trusted.join(", "));
    }
}

/// `bn promote` — flip a scout worktree to a ship: record the change of task in the branch note and,
/// if the agent's window is live, tell it it may now implement. bn-native: the record is a note
/// decision line (no new store), the nudge is the existing `agent send` mechanism.
pub(crate) fn emit_promote(cfg: &Config, ctx: &Resolved, as_json: bool) {
    // Refuse to ship a report-only finding (a scout that found a recommendation, not a code change)
    // — promoting it would just open an empty PR. The scout marks this via `bn agent done --report-only`.
    let report_only = bn_core::store::read(&cfg.note_file(ctx))
        .ok()
        .map(|n| {
            n.front
                .decisions
                .iter()
                .any(|d| d.text.contains("finding: report-only"))
        })
        .unwrap_or(false);
    if report_only {
        if as_json {
            let env = Envelope::failure(
                "promote",
                "report_only",
                "this finding is report-only (a recommendation, not a code change) — not shipping",
            );
            println!("{}", env.to_json());
        } else {
            println!(
                "Not promoting {}/{}: this scout's finding is report-only (a recommendation, not \
                 code to ship). Read the note.",
                ctx.repo, ctx.branch
            );
        }
        return;
    }
    let _ = commands::ensure_note(cfg, ctx, &today());
    let _ = commands::add(
        cfg,
        ctx,
        "decision",
        "task: ship — promoted from scout (implement the plan, open a PR when done)",
        Some("task: ship — promoted"),
        &today(),
    );
    // Nudge the agent to implement. Implementing is a LONG turn, so fire it via the reliable
    // `-p --resume` channel DETACHED (not the flaky send-keys) — it runs in the background and the
    // agent reports back via `bn agent done`. Falls back to send-keys only when no session is
    // recorded (a non-dispatch agent with just a window).
    let target = format!("{}:{}", ctx.repo, ctx.branch);
    let instr = "Promoted to ship: your scouting is accepted. Implement your plan — keep the diff \
                 minimal and focused (one concern). Run `bn verify` and re-run it after any rebase. \
                 Then commit with a clear message, run `bn ingest`, push the branch, and open a PR \
                 with `gh pr create --fill`. Finally run `bn agent done --summary \"<what you \
                 shipped>\" --pr <the PR url>`. If nothing is safely shippable, say so and open no PR.";
    let (via, sent) = match resolve_agent_session(cfg, &target) {
        Some((wt, harness, id)) => (
            "resume-detached",
            steer_detached(&harness, &id, &wt, instr).is_some(),
        ),
        None => ("keys", tmux_send_keys(&target, instr)),
    };
    if as_json {
        let env = Envelope::success(
            "promote",
            serde_json::json!({ "repo": ctx.repo, "branch": ctx.branch, "nudged": sent, "via": via }),
        );
        println!("{}", env.to_json());
        return;
    }
    println!("Promoted {target}: scout → ship");
    if sent {
        println!("  nudged the agent to implement ({via}) — it'll `bn agent done` with a PR when finished");
    } else {
        println!("  (no live agent — tell the agent it may implement the plan)");
    }
}
