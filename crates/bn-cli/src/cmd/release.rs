//! `bn release` — cut a GitHub release end-to-end (#627): pick/bump a semver from the last tag,
//! roll release notes up from the PRs merged since it, then create + push the tag and
//! `gh release create` with those notes. The pure planning + gating logic lives in
//! [`bn_core::release`]; this module owns the side-effects (tag/push/`gh`) and the verify gate.
//!
//! The same code powers the orchestrator's idle companion: [`companion_should_attempt`] is the
//! cheap "is there anything new to release?" pre-check the idle loop runs before spawning
//! `bn release`, so a green + idle fleet opportunistically ships a release with nobody watching.
use crate::*;
use bn_core::release::{self, GateDecision};
use bn_core::runner::RealRunner;

/// `bn release` — the manual entrypoint. Evaluate the release gate for the current checkout and, if
/// it passes, cut the release. `bump` selects the semver component (default patch). `dry_run`
/// prints the plan without touching git/GitHub. `skip_verify` trusts the caller that `main` is
/// green instead of running the local gate.
pub(crate) fn emit_release(
    cfg: &Config,
    bump: Option<String>,
    dry_run: bool,
    skip_verify: bool,
    as_json: bool,
) {
    let cwd = std::env::current_dir().unwrap_or_default();
    if resolver::resolve(cfg, &cwd).is_err() {
        fail_json(
            as_json,
            "release",
            "not_a_repo",
            "bn release: not in a git repo",
        );
    }
    let bump = match bump.as_deref().map(release::Bump::parse) {
        Some(Some(b)) => b,
        Some(None) => fail_json(
            as_json,
            "release",
            "bad_bump",
            "bn release: --bump must be major|minor|patch",
        ),
        None => release::Bump::default(),
    };

    // "green main" (#627): the latest local gate/verify must PASS. Run it ourselves unless the
    // caller opts out — the orchestrator companion already knows the fleet's signal.
    let green = if skip_verify {
        true
    } else {
        local_verify_passes(&cwd)
    };

    match release::evaluate_gate(&RealRunner, bump, /*idle_fleet*/ true, green) {
        GateDecision::Skip(reason) => {
            if as_json {
                let data = serde_json::json!({ "released": false, "reason": reason });
                println!("{}", Envelope::success("release", data).to_json());
            } else {
                println!("release: skipped — {reason}");
            }
        }
        GateDecision::Release(plan) => {
            if dry_run {
                if as_json {
                    let data = serde_json::json!({
                        "released": false,
                        "dry_run": true,
                        "tag": plan.next_tag,
                        "prev_tag": plan.prev_tag,
                        "notes": plan.notes,
                    });
                    println!("{}", Envelope::success("release", data).to_json());
                } else {
                    println!("release (dry-run): would cut {}\n", plan.next_tag);
                    print!("{}", plan.notes);
                }
                return;
            }
            match cut_release(&plan) {
                Ok(url) => {
                    if as_json {
                        let data = serde_json::json!({
                            "released": true,
                            "tag": plan.next_tag,
                            "url": url,
                        });
                        println!("{}", Envelope::success("release", data).to_json());
                    } else {
                        println!("release: cut {} — {url}", plan.next_tag);
                    }
                    bn_core::events::append(
                        "release_cut",
                        serde_json::Map::from_iter([(
                            "tag".to_string(),
                            serde_json::json!(plan.next_tag),
                        )]),
                    )
                    .ok();
                }
                Err(e) => fail_json(
                    as_json,
                    "release",
                    "cut_failed",
                    &format!("bn release: {e}"),
                ),
            }
        }
    }
}

/// Perform the side-effects for an approved plan: create an annotated tag, push it, then
/// `gh release create` with the rolled-up notes. Returns the release URL (or the tag, when `gh`
/// can't report a URL). Fails fast — a failed tag/push leaves no release, and a failed `gh` leaves
/// the pushed tag (re-runnable: the next attempt is idempotent since the tag now points at HEAD).
fn cut_release(plan: &release::ReleasePlan) -> Result<String, String> {
    let tag = &plan.next_tag;
    // Annotated tag at HEAD.
    if !crate::git::git_run(&["tag", "-a", tag, "-m", tag]) {
        return Err(format!("failed to create tag {tag}"));
    }
    if !crate::git::git_run(&["push", "origin", tag]) {
        return Err(format!("failed to push tag {tag}"));
    }
    // Notes via a temp file so arbitrary Markdown (backticks, newlines) survives the shell.
    let notes_path = std::env::temp_dir().join(format!("bn-release-{tag}.md"));
    std::fs::write(&notes_path, &plan.notes)
        .map_err(|e| format!("failed to write notes file: {e}"))?;
    let notes_arg = notes_path.to_string_lossy().to_string();
    let out = crate::gh::gh_command(&[
        "release",
        "create",
        tag,
        "--title",
        tag,
        "--notes-file",
        &notes_arg,
    ])
    .output();
    let _ = std::fs::remove_file(&notes_path);
    match out {
        Ok(o) if o.status.success() => {
            let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
            Ok(if url.is_empty() { tag.clone() } else { url })
        }
        Ok(o) => Err(format!(
            "gh release create failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("gh release create could not run: {e}")),
    }
}

/// Run the local `bn verify` gate in `cwd` (as a child `bn` process) and report whether it passed.
/// This is the "main is green" signal for [`emit_release`]; the orchestrator companion supplies its
/// own fleet signal instead.
fn local_verify_passes(cwd: &std::path::Path) -> bool {
    let bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("bn"));
    std::process::Command::new(bin)
        .args(["verify", "--json"])
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
