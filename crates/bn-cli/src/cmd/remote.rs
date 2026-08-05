//! `bn remote` — scaffold + verify remote GitHub agent dispatch (#280).
//!
//! `bn remote init` writes the `claude-dispatch.yml` workflow into the current repo and reports
//! whether the `ANTHROPIC_API_KEY` secret is set. The readiness logic (workflow + secret
//! detection) lives in `bn_core::remote` behind the `CommandRunner` seam so it's unit-tested with
//! a stubbed `gh`; this module is the thin I/O + rendering glue, plus the `bn doctor` hook.
use crate::*;
use std::path::Path;

/// The canonical claude-dispatch workflow, embedded from the bn repo's own copy so `bn remote
/// init` scaffolds the exact file that ships and that CI reviews — one source of truth, no drift.
pub(crate) const CLAUDE_DISPATCH_WORKFLOW: &str =
    include_str!("../../../../.github/workflows/claude-dispatch.yml");

/// `bn remote <action>` dispatch.
pub(crate) fn emit_remote(action: RemoteCmd, as_json: bool) {
    match action {
        RemoteCmd::Init => remote_init(as_json),
    }
}

/// `bn remote init` — scaffold `.github/workflows/claude-dispatch.yml` into the current repo, then
/// verify the `ANTHROPIC_API_KEY` secret via `gh` and print the setup instructions if it's missing.
pub(crate) fn remote_init(as_json: bool) {
    use bn_core::remote::{self, ScaffoldOutcome};
    use bn_core::runner::RealRunner;

    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(root) = resolver::repo_root(&cwd) else {
        if as_json {
            println!(
                "{}",
                Envelope::failure(
                    "remote_init",
                    "not_a_repo",
                    "run `bn remote init` inside a git repository",
                )
                .to_json()
            );
            return;
        }
        fail("bn remote init: run inside a git repository (no repo root found)");
    };

    let outcome = match remote::scaffold_workflow(&root, CLAUDE_DISPATCH_WORKFLOW) {
        Ok(o) => o,
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure("remote_init", "io", e.to_string()).to_json()
                );
                return;
            }
            fail(&format!("bn remote init: writing workflow: {e}"));
        }
    };

    let readiness = remote::readiness(&RealRunner, &root);
    let workflow_path = root.join(remote::WORKFLOW_REL_PATH);

    if as_json {
        let data = serde_json::json!({
            "workflow_path": workflow_path.display().to_string(),
            "scaffold": outcome,
            "readiness": readiness,
            "secret_name": remote::SECRET_NAME,
        });
        println!("{}", Envelope::success("remote_init", data).to_json());
        return;
    }

    let verb = match outcome {
        ScaffoldOutcome::Created => "Scaffolded",
        ScaffoldOutcome::Updated => "Updated",
        ScaffoldOutcome::Unchanged => "Already up to date:",
    };
    println!("{verb} {}", workflow_path.display());

    if readiness.is_ready() {
        println!(
            "✓ {} secret is set — remote dispatch is ready.",
            remote::SECRET_NAME
        );
    } else if readiness.is_misconfigured() {
        println!("⚠ {} secret is not set. Add it with:", remote::SECRET_NAME);
        println!("    gh secret set {}", remote::SECRET_NAME);
    } else {
        println!("• Could not verify repo secrets — is `gh` installed and authenticated?");
        println!("    gh auth login");
        println!("    gh secret set {}", remote::SECRET_NAME);
    }
    println!();
    println!("Then trigger a run with:");
    println!("    gh workflow run claude-dispatch.yml -f prompt='<task>'");
}

/// Remote-dispatch readiness for the current repo, for `bn doctor` to fold in. Returns `None`
/// outside a repo or when the repo isn't configured for remote dispatch (no workflow) — an
/// unconfigured repo is never a fault. `Some(readiness)` when configured, so the caller can flag
/// the misconfigured case (workflow present, secret missing) and expose the state in `--json`.
///
/// Does the `gh secret list` network probe (only when the workflow is present), so callers gate
/// this off the fast/`--brief` paths.
pub(crate) fn doctor_remote_readiness(
    repo_root: &Path,
) -> Option<bn_core::remote::RemoteReadiness> {
    let r = bn_core::remote::readiness(&bn_core::runner::RealRunner, repo_root);
    r.is_configured().then_some(r)
}
