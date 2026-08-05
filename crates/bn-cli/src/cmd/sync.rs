//! `bn sync` — reconcile a branch note against git/PR reality.
use crate::*;

/// `bn sync` — reconcile the branch note against git/PR ground truth. Reads the note, computes
/// the proposals (read-only) through the real runner/clock, then: `--json` emits them and writes
/// nothing; otherwise prints each proposal and applies it on confirmation (or unconditionally
/// with `--yes`). Silent, zero-exit when the note is already in sync.
pub(crate) fn emit_sync(cfg: &Config, ctx: &Resolved, yes: bool, as_json: bool) {
    use bn_core::clock::RealClock;
    use bn_core::runner::RealRunner;

    let note = match bn_core::store::read(&cfg.note_file(ctx)) {
        Ok(n) => n,
        Err(_) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("sync", "no_note", "No note for this branch").to_json()
                );
            } else {
                fail("bn sync: no note for this branch");
            }
            return;
        }
    };

    let proposals =
        bn_core::sync::reconcile(&note, &ctx.repo, &ctx.branch, &RealRunner, &RealClock);

    if as_json {
        // --json is the agent's read-only path: emit the proposals, write nothing.
        println!(
            "{}",
            Envelope::success("sync", payload::sync_data(&proposals)).to_json()
        );
        return;
    }

    if proposals.is_empty() {
        return; // already in sync — silent, clean exit
    }

    let mut applied = 0usize;

    if let Some(line) = &proposals.progress_line {
        if confirm_apply(yes, &format!("Log progress: \"{line}\"")) {
            match commands::append_body(cfg, ctx, "progress", line, &today()) {
                Ok(_) => {
                    println!("  ✓ logged progress");
                    applied += 1;
                }
                Err(e) => eprintln!("  ✗ progress: {}", e.message()),
            }
        }
    }

    for cand in &proposals.done_candidates {
        let prompt = format!(
            "Mark [{}] \"{}\" done (matched {} {})",
            cand.todo_id, cand.todo_text, cand.commit.hash, cand.commit.subject
        );
        if confirm_apply(yes, &prompt) {
            match commands::done(cfg, ctx, &cand.todo_id, &today(), None) {
                Ok(_) => {
                    println!("  ✓ done {}", cand.todo_id);
                    applied += 1;
                }
                Err(e) => eprintln!("  ✗ done {}: {}", cand.todo_id, e.message()),
            }
        }
    }

    if let Some(close) = &proposals.close {
        if confirm_apply(
            yes,
            &format!("PR merged ({}) — close the note + clean up", close.pr_url),
        ) {
            let _ = commands::set_pr(cfg, ctx, &close.pr_url, &today());
            match commands::close(cfg, ctx, &today(), None) {
                Ok(o) => {
                    println!("  ✓ closed note ({} todo(s) completed)", o.todos_completed);
                    println!("  → run: wt cleanup {}", close.cleanup_slug);
                    applied += 1;
                }
                Err(e) => eprintln!("  ✗ close: {}", e.message()),
            }
        }
    }

    if applied > 0 {
        sync_tmux(cfg, ctx);
    } else {
        println!("Nothing applied.");
    }
}

/// Apply-or-skip gate for one `bn sync` proposal. With `--yes` it auto-applies (printing the
/// action); otherwise it prints the proposal and reads a `y`/`n` from stdin (default no).
pub(crate) fn confirm_apply(yes: bool, prompt: &str) -> bool {
    use std::io::Write;
    if yes {
        println!("• {prompt}");
        return true;
    }
    print!("• {prompt}? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes")
}
