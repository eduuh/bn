use crate::fs::{self, StoreError};
use crate::resolver::{Config, Resolved};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const ACTIVE_PLAN_REL: &str = "plans/active.md";
pub const AGENT_PLANS_REL: &str = "plans/agents";

#[derive(Debug, Default, Serialize, Deserialize)]
struct PlanMetadata {
    active: Option<String>,
    #[serde(default)]
    drafts: Vec<String>,
    status: Option<String>,
    selected: Option<String>,
    updated: Option<String>,
}

fn metadata(cfg: &Config, ctx: &Resolved) -> Result<PlanMetadata, PlanError> {
    match std::fs::read(cfg.plan_dir(ctx).join("state.json")) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| PlanError::Io(std::io::Error::other(e)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PlanMetadata::default()),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanFile {
    pub path: String,
    pub absolute_path: String,
    pub exists: bool,
    pub size: u64,
    pub modified: Option<i64>,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanStatus {
    pub repo: String,
    pub branch: String,
    pub artifact_dir: String,
    pub active: String,
    pub active_abs: String,
    pub active_exists: bool,
    pub status: String,
    pub drafts: Vec<PlanFile>,
}

#[derive(Debug)]
pub enum PlanError {
    Io(std::io::Error),
    Store(StoreError),
    InvalidPath(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::Io(e) => write!(f, "io error: {e}"),
            PlanError::Store(e) => write!(f, "{e}"),
            PlanError::InvalidPath(path) => write!(f, "invalid note-relative path: {path}"),
        }
    }
}

impl From<std::io::Error> for PlanError {
    fn from(e: std::io::Error) -> Self {
        PlanError::Io(e)
    }
}

impl From<StoreError> for PlanError {
    fn from(e: StoreError) -> Self {
        PlanError::Store(e)
    }
}

pub fn ensure_dirs(cfg: &Config, ctx: &Resolved) -> Result<(), PlanError> {
    std::fs::create_dir_all(cfg.agent_plan_dir(ctx))?;
    Ok(())
}

pub fn init_active(cfg: &Config, ctx: &Resolved) -> Result<PathBuf, PlanError> {
    ensure_dirs(cfg, ctx)?;
    let path = cfg.active_plan_file(ctx);
    if !path.exists() {
        fs::write_atomic(&path, active_template(ctx).as_bytes())?;
    }
    update_metadata(cfg, ctx, |plan| {
        plan.active
            .get_or_insert_with(|| ACTIVE_PLAN_REL.to_string());
        plan.status.get_or_insert_with(|| "drafting".to_string());
    })?;
    Ok(path)
}

pub fn init_agent(cfg: &Config, ctx: &Resolved, session_id: &str) -> Result<PathBuf, PlanError> {
    ensure_dirs(cfg, ctx)?;
    let path = cfg.agent_plan_file(ctx, session_id);
    if !path.exists() {
        fs::write_atomic(&path, agent_template(ctx, session_id).as_bytes())?;
    }
    let rel = cfg
        .artifact_relative_path(ctx, &path)
        .unwrap_or_else(|| format!("{AGENT_PLANS_REL}/{session_id}.md"));
    update_metadata(cfg, ctx, |plan| {
        if !plan.drafts.iter().any(|d| d == &rel) {
            plan.drafts.push(rel);
        }
        plan.status = Some("drafting".to_string());
    })?;
    Ok(path)
}

pub fn status(cfg: &Config, ctx: &Resolved) -> Result<PlanStatus, PlanError> {
    let plan = metadata(cfg, ctx)?;
    let active = plan
        .active
        .clone()
        .unwrap_or_else(|| ACTIVE_PLAN_REL.to_string());
    let active_abs = resolve_relative(cfg, ctx, &active)?;
    let active_meta = file_meta(cfg, ctx, &active_abs)?;
    let mut draft_rels = plan.drafts;
    for entry in std::fs::read_dir(cfg.agent_plan_dir(ctx))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(rel) = cfg.artifact_relative_path(ctx, &path) {
                if !draft_rels.iter().any(|existing| existing == &rel) {
                    draft_rels.push(rel);
                }
            }
        }
    }
    draft_rels.sort();
    let drafts = draft_rels
        .iter()
        .filter_map(|rel| {
            let path = resolve_relative(cfg, ctx, rel).ok()?;
            file_meta(cfg, ctx, &path).ok()
        })
        .collect::<Vec<_>>();
    Ok(PlanStatus {
        repo: ctx.repo.clone(),
        branch: ctx.branch.clone(),
        artifact_dir: cfg.artifact_dir(ctx).display().to_string(),
        active: active_meta.path,
        active_abs: active_meta.absolute_path,
        active_exists: active_meta.exists,
        status: plan.status.unwrap_or_else(|| {
            if active_meta.exists {
                "drafting".to_string()
            } else {
                "none".to_string()
            }
        }),
        drafts,
    })
}

pub fn promote(cfg: &Config, ctx: &Resolved, draft_rel: &str) -> Result<PathBuf, PlanError> {
    let source = resolve_relative(cfg, ctx, draft_rel)?;
    let active = init_active(cfg, ctx)?;
    let text = std::fs::read(&source)?;
    fs::write_atomic(&active, &text)?;
    let active_rel = cfg
        .artifact_relative_path(ctx, &active)
        .unwrap_or_else(|| ACTIVE_PLAN_REL.to_string());
    update_metadata(cfg, ctx, |plan| {
        plan.active = Some(active_rel);
        plan.selected = Some(draft_rel.to_string());
        plan.status = Some("ready".to_string());
    })?;
    Ok(active)
}

pub fn resolve_relative(cfg: &Config, ctx: &Resolved, rel: &str) -> Result<PathBuf, PlanError> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute()
        || rel_path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
    {
        return Err(PlanError::InvalidPath(rel.to_string()));
    }
    let artifact_dir = cfg.artifact_dir(ctx);
    let joined = artifact_dir.join(rel_path);
    if joined.symlink_metadata().is_ok() {
        let canon = std::fs::canonicalize(&joined)?;
        let note_canon = std::fs::canonicalize(&artifact_dir)?;
        if !canon.starts_with(&note_canon) {
            return Err(PlanError::InvalidPath(rel.to_string()));
        }
        Ok(canon)
    } else {
        Ok(joined)
    }
}

pub fn file_meta(cfg: &Config, ctx: &Resolved, path: &Path) -> Result<PlanFile, PlanError> {
    let rel = cfg
        .artifact_relative_path(ctx, path)
        .ok_or_else(|| PlanError::InvalidPath(path.display().to_string()))?;
    let meta = std::fs::metadata(path).ok();
    let kind = if meta.as_ref().is_some_and(|m| m.is_dir()) {
        "dir"
    } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
        "markdown"
    } else if meta.is_some() {
        "file"
    } else {
        "missing"
    };
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    Ok(PlanFile {
        path: rel,
        absolute_path: path.display().to_string(),
        exists: meta.is_some(),
        size: meta.as_ref().map(std::fs::Metadata::len).unwrap_or(0),
        modified,
        kind: kind.to_string(),
    })
}

fn update_metadata(
    cfg: &Config,
    ctx: &Resolved,
    mutate: impl FnOnce(&mut PlanMetadata),
) -> Result<(), PlanError> {
    let path = cfg.plan_dir(ctx).join("state.json");
    let _lock = fs::FileLock::acquire_required(&path)?;
    let mut plan = metadata(cfg, ctx)?;
    mutate(&mut plan);
    plan.updated = Some(crate::util::today());
    fs::save_json_atomic(&path, &plan)?;
    Ok(())
}

fn active_template(ctx: &Resolved) -> String {
    format!(
        "# Plan\n\n## Goal\n\n## Approach\n\n## Done When\n\n## Steps\n\n## Risks\n\n## Open Questions\n\n<!-- bn: {}/{} active plan -->\n",
        ctx.repo, ctx.branch
    )
}

fn agent_template(ctx: &Resolved, session_id: &str) -> String {
    format!(
        "# Agent Plan\n\n- repo: `{}`\n- branch: `{}`\n- session: `{}`\n\n## Goal\n\n## Proposed Approach\n\n## Done When\n\n## Steps\n\n## Risks\n\n## Open Questions\n",
        ctx.repo, ctx.branch, session_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_and_ctx(root: &Path) -> (Config, Resolved) {
        (
            crate::test_support::config_with_hostname(root, "host"),
            Resolved {
                repo: "repo".to_string(),
                branch: "feature".to_string(),
            },
        )
    }

    #[test]
    fn agent_plan_file_sanitizes_session_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (cfg, ctx) = cfg_and_ctx(temp.path());
        let path = cfg.agent_plan_file(&ctx, "abc/def:ghi");

        assert!(path.ends_with("plans/agents/abc-def-ghi.md"));
        assert_eq!(
            cfg.artifact_relative_path(&ctx, &path).as_deref(),
            Some("plans/agents/abc-def-ghi.md")
        );
    }

    #[test]
    fn resolve_relative_rejects_absolute_and_parent_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (cfg, ctx) = cfg_and_ctx(temp.path());

        assert!(resolve_relative(&cfg, &ctx, "/tmp/plan.md").is_err());
        assert!(resolve_relative(&cfg, &ctx, "../plan.md").is_err());
    }

    #[test]
    fn resolve_relative_accepts_note_relative_plan_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (cfg, ctx) = cfg_and_ctx(temp.path());

        let path = resolve_relative(&cfg, &ctx, "plans/active.md").expect("resolve");

        assert_eq!(path, cfg.active_plan_file(&ctx));
    }

    #[test]
    fn task_and_plan_work_without_creating_session_history() {
        let temp = tempfile::tempdir().unwrap();
        let (cfg, ctx) = cfg_and_ctx(temp.path());
        assert_eq!(status(&cfg, &ctx).unwrap().status, "none");
        assert!(!cfg.artifact_dir(&ctx).exists());
        let task =
            crate::task_store::capture(&cfg, "Implement a tested plan", Some(ctx.rb()), "test")
                .unwrap();
        let draft = init_agent(&cfg, &ctx, "worker").unwrap();
        std::fs::write(&draft, "# Approved plan\n").unwrap();
        let active = promote(&cfg, &ctx, "plans/agents/worker.md").unwrap();
        assert_eq!(
            std::fs::read_to_string(active).unwrap(),
            "# Approved plan\n"
        );
        assert_eq!(status(&cfg, &ctx).unwrap().status, "ready");
        assert!(crate::task_doc::read(&cfg, &task).is_some());
        let artifacts = cfg.artifact_dir(&ctx);
        assert!(!artifacts.join("note.md").exists());
        assert!(!artifacts.join("note.yaml").exists());
        assert!(!artifacts.join("handoff.json").exists());
        assert!(!artifacts.join("handoff.md").exists());
    }
}
