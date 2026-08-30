//! Repository resolution and machine authorization for direct dispatch.

use crate::*;

/// Resolve the requested local bare repository and reject it unless this machine registered it.
pub(super) fn resolve_authorized_repo(
    cfg: &Config,
    ctx: Option<&Resolved>,
    repo_override: Option<&str>,
    json: bool,
) -> (String, std::path::PathBuf) {
    let (repo, bare) = match wt::resolve_bare_by_name(cfg, repo_override) {
        Ok(Some((repo, bare))) => (repo, bare),
        Ok(None) => {
            let ctx = ctx.unwrap_or_else(|| {
                fail_json(
                    json,
                    "dispatch",
                    "no-repo",
                    "bn dispatch: not in a repo — pass --repo <name> to target one from anywhere",
                )
            });
            let bare = cfg.bare_dir().join(format!("{}.git", ctx.repo));
            if !bare.is_dir() {
                fail_json(
                    json,
                    "dispatch",
                    "no-bare-repo",
                    &format!(
                        "bn dispatch: no bare repo at {} — dispatch needs the bare+worktree layout",
                        bare.display()
                    ),
                );
            }
            (ctx.repo.clone(), bare)
        }
        Err(error) => fail_json(
            json,
            "dispatch",
            "unknown-repo",
            &format!("bn dispatch: {error}"),
        ),
    };

    if !cfg.repo_allowed_here(&repo) {
        fail_json(
            json,
            "dispatch",
            "repo_not_enabled",
            &format!(
                "bn dispatch: repository '{repo}' is not enabled on this machine ('{}')",
                cfg.machine()
            ),
        );
    }
    (repo, bare)
}
