use crate::model::{Note, PlanMetadata};
use crate::resolver::{Config, Resolved};
use crate::store::{self, StoreError};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const ACTIVE_PLAN_REL: &str = "plans/active.md";
pub const AGENT_PLANS_REL: &str = "plans/agents";

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
    pub note_dir: String,
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

pub fn active_path(cfg: &Config, ctx: &Resolved) -> PathBuf {
    cfg.active_plan_file(ctx)
}

pub fn agent_path(cfg: &Config, ctx: &Resolved, session_id: &str) -> PathBuf {
    cfg.agent_plan_file(ctx, session_id)
}

pub fn init_active(cfg: &Config, ctx: &Resolved) -> Result<PathBuf, PlanError> {
    ensure_dirs(cfg, ctx)?;
    let path = cfg.active_plan_file(ctx);
    if !path.exists() {
        store::write_atomic(&path, active_template(ctx).as_bytes())?;
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
        store::write_atomic(&path, agent_template(ctx, session_id).as_bytes())?;
    }
    let rel = cfg
        .note_relative_path(ctx, &path)
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
    ensure_dirs(cfg, ctx)?;
    let note = store::read(&cfg.note_file(ctx)).ok();
    let active = note
        .as_ref()
        .and_then(|note| note.front.plan.active.clone())
        .unwrap_or_else(|| ACTIVE_PLAN_REL.to_string());
    let active_abs = resolve_relative(cfg, ctx, &active)?;
    let active_meta = file_meta(cfg, ctx, &active_abs)?;
    let mut draft_rels = note
        .as_ref()
        .map(|note| note.front.plan.drafts.clone())
        .unwrap_or_default();
    for entry in std::fs::read_dir(cfg.agent_plan_dir(ctx))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(rel) = cfg.note_relative_path(ctx, &path) {
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
        note_dir: cfg.note_dir(ctx).display().to_string(),
        active: active_meta.path,
        active_abs: active_meta.absolute_path,
        active_exists: active_meta.exists,
        status: note
            .as_ref()
            .and_then(|note| note.front.plan.status.clone())
            .unwrap_or_else(|| {
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
    store::write_atomic(&active, &text)?;
    let active_rel = cfg
        .note_relative_path(ctx, &active)
        .unwrap_or_else(|| ACTIVE_PLAN_REL.to_string());
    update_metadata(cfg, ctx, |plan| {
        plan.active = Some(active_rel);
        plan.selected = Some(draft_rel.to_string());
        plan.status = Some("ready".to_string());
    })?;
    Ok(active)
}

pub fn migrate(cfg: &Config, ctx: &Resolved) -> Result<PathBuf, PlanError> {
    ensure_dirs(cfg, ctx)?;
    let note = store::read(&cfg.note_file(ctx))?;
    let path = cfg.active_plan_file(ctx);
    if !path.exists() {
        store::write_atomic(&path, legacy_plan_template(&note).as_bytes())?;
    }
    update_metadata(cfg, ctx, |plan| {
        plan.active = Some(ACTIVE_PLAN_REL.to_string());
        plan.status = Some("drafting".to_string());
    })?;
    Ok(path)
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
    let note_dir = cfg.note_dir(ctx);
    let joined = note_dir.join(rel_path);
    if joined.symlink_metadata().is_ok() {
        let canon = std::fs::canonicalize(&joined)?;
        let note_canon = std::fs::canonicalize(&note_dir)?;
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
        .note_relative_path(ctx, path)
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
    let note_path = cfg.note_file(ctx);
    let _lock = store::NoteLock::acquire_required(&note_path)?;
    let mut note = store::read(&note_path)?;
    mutate(&mut note.front.plan);
    note.front.plan.updated = Some(crate::util::today());
    store::save(&note, &note_path)?;
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

fn legacy_plan_template(note: &Note) -> String {
    let mut out = String::from("# Plan\n\n");
    let goal = crate::render::goal_text(&note.body);
    if let Some(goal) = goal.filter(|goal| !goal.is_empty()) {
        out.push_str("## Goal\n\n");
        out.push_str(&goal);
        out.push_str("\n\n");
    }
    if !note.front.done_when.is_empty() {
        out.push_str("## Done When\n\n");
        for item in &note.front.done_when {
            out.push_str("- ");
            out.push_str(&item.text);
            out.push('\n');
        }
        out.push('\n');
    }
    if !note.front.todos.is_empty() {
        out.push_str("## Steps\n\n");
        for item in &note.front.todos {
            out.push_str("- [ ] ");
            out.push_str(&item.text);
            out.push('\n');
        }
        out.push('\n');
    }
    if !note.front.research.is_empty() {
        out.push_str("## Research\n\n");
        for item in &note.front.research {
            out.push_str("- ");
            out.push_str(&item.text);
            out.push('\n');
        }
        out.push('\n');
    }
    if !note.front.decisions.is_empty() {
        out.push_str("## Decisions\n\n");
        for item in &note.front.decisions {
            out.push_str("- ");
            out.push_str(&item.text);
            out.push('\n');
        }
    }
    out
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
            cfg.note_relative_path(&ctx, &path).as_deref(),
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
}
