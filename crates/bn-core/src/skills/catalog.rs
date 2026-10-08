use super::skill_dirs;
use crate::resolver::Config;
use crate::template::{self, ArgumentDef, TemplateError};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// A fully parsed skill: frontmatter metadata plus the renderable Markdown body.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Optional one-line trigger ("when to use this"); appended to the served prompt description.
    pub when: Option<String>,
    /// Optional match terms for relevance ranking.
    pub keywords: Vec<String>,
    pub arguments: Vec<ArgumentDef>,
    /// Internal prompts remain runtime-resolvable but are hidden from discovery and installation.
    pub internal: bool,
    pub body: String,
}

impl Skill {
    /// Compatibility rendering for public skills. Omitted values become empty strings.
    pub fn render(&self, args: Option<&Map<String, Value>>) -> String {
        match template::render_compatible(&self.body, &self.arguments, args) {
            Ok(rendered) => rendered,
            Err(_) => self.body.clone(),
        }
    }

    /// Strict rendering for runtime templates.
    pub fn render_checked(
        &self,
        args: Option<&Map<String, Value>>,
    ) -> Result<String, TemplateError> {
        template::render_checked(&self.body, &self.arguments, args)
    }
}

#[derive(Debug, Deserialize)]
struct SkillFront {
    name: Option<String>,
    description: String,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    arguments: Vec<ArgumentDef>,
    #[serde(default)]
    internal: bool,
}

/// A malformed skill source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillParseError {
    MissingFrontmatter,
    InvalidFrontmatter { message: String },
    InvalidTemplate { source: TemplateError },
}

impl fmt::Display for SkillParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFrontmatter => write!(f, "missing YAML frontmatter"),
            Self::InvalidFrontmatter { message } => write!(f, "invalid frontmatter: {message}"),
            Self::InvalidTemplate { source } => write!(f, "invalid template: {source}"),
        }
    }
}

impl std::error::Error for SkillParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidTemplate { source } => Some(source),
            Self::MissingFrontmatter | Self::InvalidFrontmatter { .. } => None,
        }
    }
}

/// A path-qualified skill catalog failure.
#[derive(Debug)]
pub enum SkillCatalogError {
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidSkill {
        path: PathBuf,
        source: SkillParseError,
    },
}

impl SkillCatalogError {
    pub fn path(&self) -> &Path {
        match self {
            Self::ReadDirectory { path, .. }
            | Self::ReadFile { path, .. }
            | Self::InvalidSkill { path, .. } => path,
        }
    }
}

impl fmt::Display for SkillCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadDirectory { path, source } => {
                write!(f, "read skill directory {}: {source}", path.display())
            }
            Self::ReadFile { path, source } => {
                write!(f, "read skill {}: {source}", path.display())
            }
            Self::InvalidSkill { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for SkillCatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadDirectory { source, .. } | Self::ReadFile { source, .. } => Some(source),
            Self::InvalidSkill { source, .. } => Some(source),
        }
    }
}

/// A required runtime prompt could not be loaded or strictly rendered.
#[derive(Debug)]
pub enum RuntimeSkillError {
    Catalog(SkillCatalogError),
    Missing { name: String },
    Render { name: String, source: TemplateError },
}

impl fmt::Display for RuntimeSkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(error) => write!(f, "{error}"),
            Self::Missing { name } => write!(f, "required prompt '{name}' not found"),
            Self::Render { name, source } => {
                write!(f, "render required prompt '{name}': {source}")
            }
        }
    }
}

impl std::error::Error for RuntimeSkillError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Catalog(error) => Some(error),
            Self::Render { source, .. } => Some(source),
            Self::Missing { .. } => None,
        }
    }
}

/// Split `---\n<yaml>\n---\n<body>` into a [`Skill`]. `name` falls back to the file stem.
pub fn parse_skill(text: &str, stem: &str) -> Result<Skill, SkillParseError> {
    let (yaml, body) =
        crate::store::split_frontmatter(text).ok_or(SkillParseError::MissingFrontmatter)?;
    let front: SkillFront =
        serde_yaml::from_str(yaml).map_err(|error| SkillParseError::InvalidFrontmatter {
            message: error.to_string(),
        })?;
    template::validate(body, &front.arguments)
        .map_err(|source| SkillParseError::InvalidTemplate { source })?;
    Ok(Skill {
        name: front.name.unwrap_or_else(|| stem.to_string()),
        description: front.description,
        when: front.when,
        keywords: front.keywords,
        arguments: front.arguments,
        internal: front.internal,
        body: body.to_string(),
    })
}

/// Parse every `*.md` skill in `dir`, paired with its file path. A missing directory is empty.
/// Whether this `.md` documents the directory rather than declaring a skill.
///
/// A skills directory is a directory someone reads, so it grows a README — this repo ships one in
/// `config/skills/` explaining what belongs there. Without this the loader parsed that README as a
/// skill, found no frontmatter, and failed the **entire catalog**: one documentation file and every
/// skill on the machine stops resolving, with an error naming the README rather than saying that
/// nothing can load. It cost a red CI job whose tests passed on every developer machine that
/// happened not to have one.
///
/// Only the conventional name is exempt. A file someone meant as a skill and got wrong must still
/// be loud — silently skipping it would mean a skill that simply never runs, with nothing said,
/// which is the worse failure of the two.
fn is_directory_doc(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("README.md"))
}

pub(super) fn load_skill_files(dir: &Path) -> Result<Vec<(Skill, PathBuf)>, SkillCatalogError> {
    load_skill_entries(dir)?
        .into_iter()
        .map(|entry| entry.skill.map(|skill| (skill, entry.path)))
        .collect()
}

/// One `*.md` file in a skills directory: the name it claims and whether it parsed.
struct SkillEntry {
    name: String,
    path: PathBuf,
    skill: Result<Skill, SkillCatalogError>,
}

/// Every skill file in `dir`, each parsed on its own, so one broken file does not hide the rest.
/// A missing directory is empty; an unreadable one is still an error.
fn load_skill_entries(dir: &Path) -> Result<Vec<SkillEntry>, SkillCatalogError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(SkillCatalogError::ReadDirectory {
                path: dir.to_path_buf(),
                source,
            })
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SkillCatalogError::ReadDirectory {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("md")
            && !is_directory_doc(&path)
        {
            paths.push(path);
        }
    }
    paths.sort();

    Ok(paths
        .into_iter()
        .map(|path| {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("skill")
                .to_string();
            let text = std::fs::read_to_string(&path);
            let name = text
                .as_deref()
                .ok()
                .and_then(claimed_name)
                .unwrap_or_else(|| stem.clone());
            let skill = match text {
                Ok(text) => {
                    parse_skill(&text, &stem).map_err(|source| SkillCatalogError::InvalidSkill {
                        path: path.clone(),
                        source,
                    })
                }
                Err(source) => Err(SkillCatalogError::ReadFile {
                    path: path.clone(),
                    source,
                }),
            };
            SkillEntry { name, path, skill }
        })
        .collect())
}

/// The `name:` a skill file declares, read without parsing the rest, so a file that fails to parse
/// can still be matched to the skill it was meant to be.
fn claimed_name(text: &str) -> Option<String> {
    let (yaml, _) = crate::store::split_frontmatter(text)?;
    yaml.lines().find_map(|line| {
        let value = line.strip_prefix("name:")?.trim().trim_matches(['"', '\'']);
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// The highest-precedence definition of `name` across `dirs` (low→high), or its error.
///
/// Only the requested skill's own file can fail the lookup. Resolving one prompt used to parse the
/// whole catalog strictly, so a single half-edited personal skill stopped every `bn skills render`,
/// dispatch seed, critic and pipeline prompt on the machine. A broken file that claims `name` still
/// fails: falling back to a lower-precedence copy would silently run a definition the user replaced.
fn find_skill(dirs: &[PathBuf], name: &str) -> Result<Option<Skill>, SkillCatalogError> {
    let mut found = None;
    for dir in dirs {
        for entry in load_skill_entries(dir)? {
            let matches = match &entry.skill {
                Ok(skill) => skill.name == name,
                Err(_) => entry.name == name,
            };
            if matches {
                found = Some(entry.skill);
            }
        }
    }
    found.transpose()
}

/// Merge skills from `dirs` low→high precedence. Internal prompts remain in this runtime set.
pub fn load_skill_set(dirs: &[PathBuf]) -> Result<Vec<Skill>, SkillCatalogError> {
    let mut by_name = BTreeMap::new();
    for dir in dirs {
        for (skill, _) in load_skill_files(dir)? {
            by_name.insert(skill.name.clone(), skill);
        }
    }
    Ok(by_name.into_values().collect())
}

/// Resolve the highest-precedence runtime skill named `name`.
pub fn load_skill(
    cfg: &Config,
    base_dir: &Path,
    name: &str,
) -> Result<Option<Skill>, SkillCatalogError> {
    let dirs: Vec<PathBuf> = skill_dirs(cfg, base_dir)
        .into_iter()
        .map(|(_, dir)| dir)
        .collect();
    find_skill(&dirs, name)
}

/// Resolve and strictly render a required public or internal runtime prompt.
pub fn render_required(
    cfg: &Config,
    base_dir: &Path,
    name: &str,
    args: Option<&Map<String, Value>>,
) -> Result<String, RuntimeSkillError> {
    let skill = load_skill(cfg, base_dir, name).map_err(RuntimeSkillError::Catalog)?;
    let skill = skill.ok_or_else(|| RuntimeSkillError::Missing {
        name: name.to_string(),
    })?;
    skill
        .render_checked(args)
        .map_err(|source| RuntimeSkillError::Render {
            name: name.to_string(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_defaults_false_and_checked_render_is_strict() {
        let skill = parse_skill(
            "---\ndescription: test\narguments:\n  - name: value\n    required: true\n---\n{{value}}",
            "test",
        )
        .expect("skill parses");

        assert!(!skill.internal);
        assert!(matches!(
            skill.render_checked(None),
            Err(TemplateError::MissingRequiredValue { .. })
        ));
        assert_eq!(skill.render(None), "");
    }

    #[test]
    fn invalid_template_declarations_fail_parse() {
        let error = parse_skill(
            "---\ndescription: test\narguments:\n  - name: bad/name\n---\n",
            "test",
        );

        assert!(matches!(
            error,
            Err(SkillParseError::InvalidTemplate {
                source: TemplateError::InvalidArgumentName { .. }
            })
        ));
    }

    fn write_skill(dir: &Path, file: &str, text: &str) {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(dir.join(file), text).expect("write skill");
    }

    const GOOD: &str = "---\nname: good\ndescription: fine\n---\nbody\n";
    const BROKEN: &str = "---\nname: broken\ndescription: x\n---\n{{undeclared}}\n";

    /// One broken skill used to fail every lookup, so a half-edited personal skill stopped every
    /// dispatch seed and `bn skills render` on the machine.
    #[test]
    fn a_broken_sibling_does_not_block_another_skill() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_skill(tmp.path(), "good.md", GOOD);
        write_skill(tmp.path(), "broken.md", BROKEN);
        let dirs = [tmp.path().to_path_buf()];

        let good = find_skill(&dirs, "good")
            .expect("good resolves")
            .expect("good exists");
        assert_eq!(good.body, "body\n");
        assert!(find_skill(&dirs, "absent")
            .expect("absent is not an error")
            .is_none());
        // The strict whole-catalog load is unchanged.
        assert!(load_skill_set(&dirs).is_err());
    }

    #[test]
    fn the_broken_skill_itself_still_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_skill(tmp.path(), "renamed-file.md", BROKEN);
        let error = find_skill(&[tmp.path().to_path_buf()], "broken").expect_err("must fail");
        assert!(error.to_string().contains("renamed-file.md"), "{error}");
    }

    /// A broken higher-tier copy must not silently fall back to the lower tier's definition.
    #[test]
    fn a_broken_override_does_not_fall_back_to_a_lower_tier() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (low, high) = (tmp.path().join("low"), tmp.path().join("high"));
        write_skill(
            &low,
            "broken.md",
            "---\nname: broken\ndescription: ok\n---\nlow\n",
        );
        write_skill(&high, "broken.md", BROKEN);
        assert!(find_skill(&[low.clone(), high.clone()], "broken").is_err());
        // And the other way round, a good override wins over a broken lower tier.
        let found = find_skill(&[high, low], "broken")
            .expect("ok")
            .expect("found");
        assert_eq!(found.body, "low\n");
    }

    #[test]
    fn runtime_error_names_missing_prompt() {
        let error = RuntimeSkillError::Missing {
            name: "internal-test".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "required prompt 'internal-test' not found"
        );
    }

    #[test]
    fn a_readme_documents_the_directory_instead_of_breaking_every_skill_in_it() {
        // The failure this prevents took out a whole CI job. A skills directory is a directory
        // people read, so it grows a README — this repo ships one in `config/skills/`. Parsed as a
        // skill it has no frontmatter, and the error propagated out of the *set* loader, so a
        // single documentation file meant nothing on the machine could resolve a skill at all.
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("README.md"),
            "# Reference skills\n\nWhat belongs in this folder.",
        )
        .expect("readme");
        std::fs::write(
            temp.path().join("real.md"),
            "---\ndescription: real\n---\nBody.",
        )
        .expect("skill");

        let loaded = load_skill_set(&[temp.path().to_path_buf()]).expect("catalog still loads");
        assert_eq!(
            loaded.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["real"],
            "the README is not a skill, and must not become one"
        );
    }

    #[test]
    fn a_malformed_skill_is_still_loud() {
        // The other half: only the conventional README name is exempt. A file someone meant as a
        // skill and got wrong must still fail — silently skipping it would leave them with a skill
        // that never runs and nothing said about why, which is the worse of the two failures.
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("typo.md"), "no frontmatter here").expect("bad skill");
        assert!(load_skill_set(&[temp.path().to_path_buf()]).is_err());
    }

    #[test]
    fn internal_prompts_load_at_runtime_but_stay_hidden_from_discovery() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("public.md"),
            "---\ndescription: public\n---\nPublic.",
        )
        .expect("public prompt");
        std::fs::write(
            temp.path().join("internal.md"),
            "---\ndescription: internal\ninternal: true\n---\nInternal.",
        )
        .expect("internal prompt");

        let loaded = load_skill_set(&[temp.path().to_path_buf()]).expect("runtime catalog");
        assert!(loaded.iter().any(|skill| skill.name == "internal"));

        let discovered = super::super::discover_in(&[(
            crate::skills::SkillScope::GlobalShared,
            temp.path().to_path_buf(),
        )])
        .expect("public discovery");
        assert_eq!(
            discovered
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["public"]
        );
    }

    #[test]
    fn shipped_prompts_all_strictly_render() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let skills =
            load_skill_set(&[root.join("config/prompts")]).expect("shipped prompt catalog loads");

        let internal_names: BTreeMap<_, _> = skills
            .iter()
            .filter(|skill| skill.internal)
            .map(|skill| (skill.name.as_str(), ()))
            .collect();
        for name in [
            "attention-completion",
            "auto-review-envelope",
            "curriculum-resource",
            "dispatch-autonomy-interactive",
            "dispatch-autonomy-unattended",
            "dispatch-cli-map",
            "dispatch-completion",
            "dispatch-plan-review-gate",
            "dispatch-plan-work-bootstrap",
            "dispatch-scout",
            "dispatch-ship",
            "dispatch-verify-gate",
            "m365-commitments-query",
            "m365-commitments-schema",
            "m365-email-query",
            "m365-email-schema",
            "m365-owed-query",
            "m365-owed-schema",
            "m365-teams-query",
            "m365-teams-schema",
            "meeting-prep-query",
            "meeting-prep-schema",
            "meetings-query",
            "meetings-schema",
            "pipeline-critic-envelope",
            "pipeline-investigate-envelope",
            "pipeline-plan-envelope",
            "pipeline-sub-investigator",
            "plan-review-critic",
            "repo-orchestrator-goal",
            "repo-setup-agent",
            "review-gate-critic",
            "specialist-scaffold",
        ] {
            assert!(internal_names.contains_key(name), "missing prompt {name}");
        }

        for skill in skills {
            let args: Map<String, Value> = skill
                .arguments
                .iter()
                .map(|argument| {
                    (
                        argument.name.clone(),
                        Value::String(format!("test-{}", argument.name)),
                    )
                })
                .collect();
            let rendered = skill
                .render_checked(Some(&args))
                .unwrap_or_else(|error| panic!("render {}: {error}", skill.name));
            assert!(
                !rendered.contains("{{"),
                "{} left an unresolved placeholder",
                skill.name
            );
        }
    }

    /// Every declared template argument must be referenced by its body — a declared-but-unused
    /// argument is dead plumbing that the strict renderer tolerates silently (it was how the
    /// dead `auto-review-pr.url` and `critique-fresh.pr` args survived unnoticed).
    #[test]
    fn shipped_prompt_arguments_are_all_referenced() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let skills =
            load_skill_set(&[root.join("config/prompts")]).expect("shipped prompt catalog loads");
        for skill in skills {
            for argument in &skill.arguments {
                let placeholder = format!("{{{{{}}}}}", argument.name);
                assert!(
                    skill.body.contains(&placeholder),
                    "{} declares argument '{}' but its body never references {placeholder}",
                    skill.name,
                    argument.name
                );
            }
        }
    }

    /// Every `bn_<name>` token in a shipped prompt must be a real bn MCP tool. The MCP surface is
    /// exactly the ten current-branch tools served by bn-mcp; anything else (`bn_next`,
    /// `bn_metrics`, …) is a CLI command and must be written `bn <sub>` — prompts instructing a
    /// phantom tool fail on every run.
    #[test]
    fn shipped_prompts_reference_only_real_mcp_tools() {
        const MCP_TOOLS: &[&str] = &[
            "bn_add",
            "bn_done",
            "bn_pr",
            "bn_goal",
            "bn_cat",
            "bn_context",
            "bn_status",
            "bn_sync",
            "bn_handoff_write",
            "bn_handoff_read",
            "bn_links_add",
            "bn_ref",
            "bn_ref_append",
            "bn_cmd",
        ];
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dirs = [root.join("config/prompts"), root.join("config/agents")];
        for dir in dirs {
            for entry in std::fs::read_dir(&dir).expect("config dir listing") {
                let path = entry.expect("dir entry").path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("prompt readable");
                let bytes = text.as_bytes();
                let mut i = 0;
                while let Some(pos) = text[i..].find("bn_") {
                    let start = i + pos;
                    // Only a token boundary counts (skip e.g. `_bn_prompt` shell vars).
                    let preceded = start > 0
                        && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
                    let end = text[start..]
                        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .map(|off| start + off)
                        .unwrap_or(text.len());
                    let token = &text[start..end];
                    if !preceded && token != "bn_" {
                        assert!(
                            MCP_TOOLS.contains(&token),
                            "{} references '{token}', which is not one of the bn MCP tools — \
                             CLI commands must be written `bn <sub>`",
                            path.display()
                        );
                    }
                    i = end;
                }
            }
        }
    }
}
