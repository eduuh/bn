//! `bn agent instruct` — give a running agent an instruction that outlives its context.
//!
//! A message pasted into a pane lives exactly as long as that session. Relaunch the agent (or let
//! it compact), and "always run clippy before pushing" is gone — which is why standing guidance
//! had to be repeated every time an agent was reset.
//!
//! So an instruction is written down *and* delivered:
//!
//!   * **recorded** under `## Directives` in the branch note, where the handoff packet picks it up.
//!     `bn agent relaunch --hard` seeds a fresh session from that packet, and `bn dispatch` seeds a
//!     new one from the note, so the directive is re-stated to every future session automatically.
//!   * **delivered** to the live session through the same channel selection `bn agent send` uses,
//!     so the agent acts on it now rather than at its next restart.
//!
//! `--no-send` records without delivering (for an agent that is mid-turn and you don't want to
//! interrupt); `--list` and `--drop` manage what has accumulated.

use crate::*;
use bn_core::resolver::Resolved;

/// The section keyword the note API addresses directives by.
const SECTION: &str = "directive";

/// Resolve `repo:branch` to the note context it names. Reuses the agent-session resolver's own
/// fallback — a target whose note does not exist under that exact name is looked up through the
/// window's recorded `@bn_repo`/`@bn_branch`, so a pooled slot resolves like anything else.
fn ctx_for(cfg: &Config, target: &str) -> Option<Resolved> {
    let (repo, branch) = target.split_once(':')?;
    let direct = Resolved { repo: repo.to_string(), branch: branch.to_string() };
    if cfg.note_file(&direct).exists() {
        return Some(direct);
    }
    let metadata = proc::tmux(&[
        "display-message",
        "-p",
        "-t",
        &crate::proc::exact_target(target),
        "#{@bn_repo}\t#{@bn_branch}",
    ])?;
    let (repo, branch) = metadata.trim().split_once('\t')?;
    (!repo.is_empty() && !branch.is_empty())
        .then(|| Resolved { repo: repo.to_string(), branch: branch.to_string() })
}

fn fail_json(cmd: &str, code: &str, msg: &str, as_json: bool) {
    if as_json {
        println!("{}", Envelope::failure(cmd, code, msg).to_json());
    } else {
        eprintln!("bn agent instruct: {msg}");
    }
    std::process::exit(1);
}

/// The text a delivered directive is wrapped in. Naming it a standing instruction (and saying it is
/// recorded) is the difference between an agent treating it as a one-off aside and treating it as a
/// constraint that outlives the turn.
fn delivery_text(text: &str) -> String {
    format!(
        "Standing instruction (recorded in this branch's note under `## Directives`, and re-seeded \
         into any future session): {text}"
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_instruct(
    cfg: &Config,
    target: &str,
    text: &str,
    no_send: bool,
    list: bool,
    drop: Option<&str>,
    as_json: bool,
) {
    let cmd = "agent_instruct";
    let target = target.trim();
    if target.is_empty() || !target.contains(':') {
        fail_json(cmd, "invalid_target", &format!("invalid agent target '{target}' (expected repo:branch)"), as_json);
        return;
    }
    let Some(ctx) = ctx_for(cfg, target) else {
        fail_json(cmd, "no_note", &format!("no branch note resolves for '{target}'"), as_json);
        return;
    };

    if list {
        return emit_list(cfg, &ctx, target, as_json);
    }
    if let Some(id) = drop {
        return emit_drop(cfg, &ctx, target, id, as_json);
    }

    let text = text.trim();
    if text.is_empty() {
        fail_json(cmd, "empty_instruction", "an instruction is required", as_json);
        return;
    }

    // Record first. If delivery fails the directive still survives to the next session, which is
    // the whole point — the reverse order would lose it exactly when the agent is least reachable.
    let outcome = bn_core::commands::add(cfg, &ctx, SECTION, text, Some(text), &bn_core::util::today());
    let recorded = match outcome {
        Ok(o) => o,
        Err(e) => {
            fail_json(cmd, "write_failed", &format!("could not record the directive: {e:?}"), as_json);
            return;
        }
    };

    let delivered = if no_send {
        None
    } else {
        Some(super::deliver_instruction(cfg, target, &delivery_text(text)))
    };

    if as_json {
        let data = serde_json::json!({
            "target": target,
            "repo": ctx.repo,
            "branch": ctx.branch,
            "id": recorded.id,
            "text": recorded.text,
            "recorded": !recorded.skipped,
            "delivered": delivered,
        });
        println!("{}", Envelope::success(cmd, data).to_json());
    } else {
        if recorded.skipped {
            println!("\u{2713} already recorded [{}] {}", recorded.id, recorded.text);
        } else {
            println!("\u{2713} recorded [{}] {}", recorded.id, recorded.text);
        }
        match delivered {
            Some(true) => println!("\u{2192} delivered to {target}"),
            Some(false) => eprintln!(
                "! not delivered to {target} (no live session) — it will be seeded on the next launch"
            ),
            None => println!("  (not sent; --no-send)"),
        }
    }
}

fn emit_list(cfg: &Config, ctx: &Resolved, target: &str, as_json: bool) {
    let cmd = "agent_instruct";
    let note = match bn_core::store::read(&cfg.note_file(ctx)) {
        Ok(n) => n,
        Err(e) => {
            fail_json(cmd, "read_failed", &format!("could not read the note: {e}"), as_json);
            return;
        }
    };
    let items = &note.front.directives;
    if as_json {
        let data = serde_json::json!({ "target": target, "count": items.len(), "directives": items });
        println!("{}", Envelope::success(cmd, data).to_json());
    } else if items.is_empty() {
        println!("No standing directives for {target}.");
    } else {
        println!("Standing directives for {target}:");
        for i in items {
            println!("  [{}] {}", i.id, i.text);
        }
    }
}

fn emit_drop(cfg: &Config, ctx: &Resolved, target: &str, id: &str, as_json: bool) {
    let cmd = "agent_instruct";
    let path = cfg.note_file(ctx);
    let mut note = match bn_core::store::read(&path) {
        Ok(n) => n,
        Err(e) => {
            fail_json(cmd, "read_failed", &format!("could not read the note: {e}"), as_json);
            return;
        }
    };
    let before = note.front.directives.len();
    note.front.directives.retain(|i| i.id != id);
    if note.front.directives.len() == before {
        fail_json(cmd, "no_such_directive", &format!("no directive '{id}' on {target}"), as_json);
        return;
    }
    if let Err(e) = bn_core::store::save(&note, &path) {
        fail_json(cmd, "write_failed", &format!("could not write the note: {e}"), as_json);
        return;
    }
    if as_json {
        let data = serde_json::json!({ "target": target, "dropped": id });
        println!("{}", Envelope::success(cmd, data).to_json());
    } else {
        println!("\u{2713} dropped [{id}] from {target}");
    }
}
