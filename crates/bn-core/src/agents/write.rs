//! Authoring the agent roster: create, save and delete an `agents/<name>.md`.
//!
//! The read side of this module ([`super::discover`], [`super::parse_agent`]) is what every other
//! caller uses; this is the small write half behind `/agents`' editor, and it is the agent analogue
//! of [`crate::skills`]'s `create_skill` / `save_skill` / `delete_skill`.
//!
//! One rule governs all three: **nothing unparseable reaches the disk**. Every write is validated
//! through [`super::parse_agent`] first, so a bad edit is a rejected request rather than an agent
//! file that then breaks `discover` — and `discover` is not per-agent-tolerant. It returns
//! `Err(InvalidAgent)` for the *whole catalog* when one file fails to parse, which would take the
//! scheduler, the dispatcher and the roster down together. The editor is therefore the one place
//! that must not trust its input.

use std::path::{Path, PathBuf};

use super::{parse_agent, AgentKind};
use crate::resolver::{self, Config, JobScope};

/// The tiers a dashboard create may write into, by keyword.
///
/// Deliberately the same two keywords `bn agents new` already takes, and deliberately only the
/// global ones: the branch and repo tiers `agent_dirs` also resolves depend on a `Resolved`
/// context, and bn-web serves a machine rather than a checkout — a "repo" tier would mean whichever
/// repo the server process happened to start in, which is not a thing anyone can point at.
pub const CREATE_SCOPES: [&str; 2] = ["global-personal", "global-shared"];

/// Resolve a create-scope keyword to the `agents/` directory it writes into.
///
/// `global-personal` prefers the personal overlay when one is configured, matching
/// [`crate::specialists::scaffold`] so an agent authored from the dashboard lands where one
/// authored from the CLI does.
pub fn scope_dir(cfg: &Config, scope: &str) -> Result<PathBuf, String> {
    match scope {
        "global-personal" => Ok(cfg
            .overlay_root
            .as_ref()
            .map(|root| root.join("agents"))
            .unwrap_or_else(|| cfg.personal_root.join("agents"))),
        "global-shared" => Ok(resolver::agents_dir()),
        other => Err(format!(
            "unknown scope '{other}' — use {}",
            CREATE_SCOPES.join(" | ")
        )),
    }
}

/// A name that is safe as both a file stem and a dispatch target.
pub fn valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Overwrite one agent's source file with `content`, after proving `content` still parses as the
/// agent it claims to be.
///
/// Two checks, not one. Parsing catches a malformed manifest; comparing the parsed name against
/// `name` catches the subtler edit — renaming via the `name:` frontmatter, which would leave a
/// `foo.md` that defines `bar`, shadowing whatever else is called `bar` and leaving `foo`
/// unreachable at a path still called `foo.md`. Renaming an agent is a create plus a delete, and
/// saying so is kinder than silently doing half of it.
pub fn save_agent(path: &Path, name: &str, scope: JobScope, content: &str) -> Result<(), String> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("agent")
        .to_string();
    let parsed = parse_agent(content, &stem, scope, path.to_path_buf())
        .map_err(|error| format!("content is not a valid agent: {error}"))?;
    if parsed.name != name {
        return Err(format!(
            "this file defines agent '{name}'; the edit renames it to '{}'. Create the new name \
             and delete the old one instead",
            parsed.name
        ));
    }
    crate::store::write_atomic(path, content.as_bytes())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Delete one agent's source file. The path always comes from discovery, never from a request.
pub fn delete_agent(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// What a create needs beyond a name: the fields whose presence or absence each kind validates.
///
/// Carried as a struct rather than six positional arguments because they are conditionally
/// required — `gather`/`sinks` for staff, `repo` for a specialist — and a caller passing them in
/// the wrong order would produce a file that parses and means something else.
#[derive(Debug, Clone)]
pub struct NewAgent {
    pub kind: AgentKind,
    pub description: String,
    /// Staff: the command whose output the prompt is rendered over. Forbidden for every other kind.
    pub gather: Option<String>,
    /// Staff: where the briefing card is written. Forbidden for every other kind.
    pub sinks: Vec<String>,
    /// Specialist: the repo the reviewer is gated to. Forbidden for every other kind.
    pub repo: Option<String>,
    /// The prompt below the frontmatter. Required for every kind except `command`.
    pub body: String,
}

/// Assemble the `.md` source for a new agent — frontmatter, then body.
///
/// Only the fields the caller actually set are emitted. An agent manifest is read by people as
/// often as by `serde`, and a scaffold full of `gather: null` placeholders teaches the wrong shape.
fn render_source(name: &str, spec: &NewAgent) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str(&format!("kind: {}\n", spec.kind.label()));
    out.push_str(&format!(
        "description: {}\n",
        crate::specialists::yaml_scalar(&spec.description)
    ));
    if let Some(repo) = spec.repo.as_deref().filter(|r| !r.trim().is_empty()) {
        out.push_str(&format!(
            "repo: {}\n",
            crate::specialists::yaml_scalar(repo)
        ));
    }
    if let Some(gather) = spec.gather.as_deref().filter(|g| !g.trim().is_empty()) {
        out.push_str(&format!(
            "gather: {}\n",
            crate::specialists::yaml_scalar(gather)
        ));
    }
    if !spec.sinks.is_empty() {
        out.push_str(&format!("sinks: [{}]\n", spec.sinks.join(", ")));
    }
    out.push_str("---\n\n");
    out.push_str(spec.body.trim_end());
    out.push('\n');
    out
}

/// Create `<scope dir>/<name>.md` and return the path written.
///
/// The assembled source is parsed before it is written, so a create that would violate its kind's
/// rules (a staff agent with no `sinks:`, a specialist with no `repo:`) fails with the same message
/// the catalog would have failed with — and fails *before* there is a file to clean up.
pub fn create_agent(
    cfg: &Config,
    scope: &str,
    name: &str,
    spec: &NewAgent,
    overwrite: bool,
) -> Result<PathBuf, String> {
    if !valid_agent_name(name) {
        return Err(format!(
            "agent name '{name}' may contain letters, digits, '-' and '_' (max 64)"
        ));
    }
    if spec.description.trim().is_empty() {
        return Err("description is required".to_string());
    }
    let dir = scope_dir(cfg, scope)?;
    let path = dir.join(format!("{name}.md"));
    if path.exists() && !overwrite {
        return Err(format!("{} already exists", path.display()));
    }

    let source = render_source(name, spec);
    // Validated against the tier it is about to live in, so the error a caller sees is the error
    // `discover` would have raised on the very next tick.
    parse_agent(&source, name, JobScope::Global, path.clone())
        .map_err(|error| format!("that would not be a valid agent: {error}"))?;

    std::fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    crate::store::write_atomic(&path, source.as_bytes())
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str =
        "---\nname: demo\nkind: command\ndescription: a demo\n---\n\nDo the thing.\n";

    fn command(description: &str, body: &str) -> NewAgent {
        NewAgent {
            kind: AgentKind::Command,
            description: description.to_string(),
            gather: None,
            sinks: Vec::new(),
            repo: None,
            body: body.to_string(),
        }
    }

    fn cfg_at(root: &std::path::Path) -> Config {
        crate::test_support::config(root)
    }

    #[test]
    fn a_created_command_agent_is_discoverable_where_it_was_written() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_at(tmp.path());
        let path = create_agent(
            &cfg,
            "global-personal",
            "demo",
            &command("a demo", "Do the thing."),
            false,
        )
        .expect("create");

        assert_eq!(path, tmp.path().join("agents/demo.md"));
        let text = std::fs::read_to_string(&path).expect("read back");
        let parsed = parse_agent(&text, "demo", JobScope::Global, path).expect("parses");
        assert_eq!(parsed.name, "demo");
        assert_eq!(parsed.kind, AgentKind::Command);
        assert_eq!(parsed.description.as_deref(), Some("a demo"));
        assert_eq!(parsed.body.trim(), "Do the thing.");
    }

    #[test]
    fn a_create_that_its_kind_would_reject_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_at(tmp.path());
        // Staff requires `gather` and at least one sink; this has neither.
        let spec = NewAgent {
            kind: AgentKind::Staff,
            description: "a briefing".to_string(),
            gather: None,
            sinks: Vec::new(),
            repo: None,
            body: "Summarize.".to_string(),
        };
        let error = create_agent(&cfg, "global-personal", "brief", &spec, false)
            .expect_err("staff without gather must be refused");

        assert!(error.contains("gather"), "{error}");
        // The whole point of validating before writing: no half-created file to clean up.
        assert!(!tmp.path().join("agents/brief.md").exists());
    }

    #[test]
    fn a_create_refuses_to_clobber_unless_told_to() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_at(tmp.path());
        create_agent(
            &cfg,
            "global-personal",
            "demo",
            &command("first", ""),
            false,
        )
        .expect("create");

        let error = create_agent(
            &cfg,
            "global-personal",
            "demo",
            &command("second", ""),
            false,
        )
        .expect_err("second create must conflict");
        assert!(error.contains("already exists"), "{error}");

        create_agent(
            &cfg,
            "global-personal",
            "demo",
            &command("second", ""),
            true,
        )
        .expect("overwrite");
        let text = std::fs::read_to_string(tmp.path().join("agents/demo.md")).expect("read back");
        assert!(text.contains("description: second"), "{text}");
    }

    #[test]
    fn an_unparseable_edit_leaves_the_file_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("demo.md");
        std::fs::write(&path, VALID).expect("seed");

        let error = save_agent(&path, "demo", JobScope::Global, "no frontmatter here\n")
            .expect_err("must refuse");
        assert!(error.contains("not a valid agent"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), VALID);
    }

    #[test]
    fn an_edit_that_renames_the_agent_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("demo.md");
        std::fs::write(&path, VALID).expect("seed");

        // Parses perfectly well — and would leave `demo.md` defining an agent called `other`,
        // shadowing whatever else answers to that name while `demo` vanishes from the roster.
        let renamed = "---\nname: other\nkind: command\ndescription: a demo\n---\n\nDo it.\n";
        let error =
            save_agent(&path, "demo", JobScope::Global, renamed).expect_err("must refuse rename");
        assert!(error.contains("renames it to 'other'"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), VALID);
    }

    #[test]
    fn a_valid_edit_lands() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("demo.md");
        std::fs::write(&path, VALID).expect("seed");

        let next = "---\nname: demo\nkind: command\ndescription: a demo\n---\n\nDo it twice.\n";
        save_agent(&path, "demo", JobScope::Global, next).expect("save");
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), next);
    }

    #[test]
    fn an_unknown_create_scope_names_the_ones_that_exist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let error = scope_dir(&cfg_at(tmp.path()), "repo-shared").expect_err("must refuse");
        assert!(error.contains("global-personal | global-shared"), "{error}");
    }
}
