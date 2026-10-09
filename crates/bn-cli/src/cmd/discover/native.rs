//! Merge native installed skills with bn's effective workflow catalogue, then use its ranker.

use super::shell_word;
use crate::Config;
use bn_core::skills::{SkillInfo, SkillScope};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Deserialize)]
struct NativeMetadata {
    name: Option<String>,
    description: String,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    internal: bool,
}

#[derive(Clone)]
pub(super) struct Workflow {
    pub info: SkillInfo,
    pub copilot: Option<String>,
    pub claude: Option<String>,
    owned: bool,
}

impl Workflow {
    pub fn with_score(&self, score: Option<u32>) -> Self {
        let mut workflow = self.clone();
        workflow.info.score = score;
        workflow
    }

    pub fn read_command(&self, cfg: &Config, cwd: &Path) -> Result<Option<String>, String> {
        self.read_command_on_path(cfg, cwd, &std::env::var("PATH").unwrap_or_default())
    }

    pub fn read_command_on_path(
        &self,
        cfg: &Config,
        cwd: &Path,
        path: &str,
    ) -> Result<Option<String>, String> {
        if self.owned {
            super::hints::skill_read(cfg, cwd, &self.info.name).map(Some)
        } else {
            Ok(bn_core::mcp::command_on_path("cat", path)
                .then(|| format!("cat -- {}", shell_word(&self.info.path))))
        }
    }
}

pub(super) fn catalog(
    cfg: &Config,
    cwd: &Path,
    home: &Path,
) -> Result<BTreeMap<String, Workflow>, String> {
    let mut workflows = BTreeMap::new();
    scan(&mut workflows, home, false)?;
    if let Some(root) = bn_core::resolver::repo_root(cwd) {
        scan(&mut workflows, &root, true)?;
    }
    // The owned tier has its own tested precedence. Prefer its effective source over copies
    // generated into native roots, and retain its MCP invocation rather than inventing one.
    for info in bn_core::skills::discover(cfg, cwd).map_err(|e| e.to_string())? {
        if info.effective {
            workflows.insert(
                info.name.clone(),
                Workflow {
                    copilot: Some(format!("/{}", info.name)),
                    claude: Some(format!("/bn:{}", info.name)),
                    owned: true,
                    info,
                },
            );
        }
    }
    Ok(workflows)
}

fn scan(workflows: &mut BTreeMap<String, Workflow>, root: &Path, repo: bool) -> Result<(), String> {
    for (relative, copilot, claude) in [
        (".agents/skills", true, false),
        (
            if repo {
                ".github/skills"
            } else {
                ".copilot/skills"
            },
            true,
            false,
        ),
        (".claude/skills", repo, true),
    ] {
        let dir = root.join(relative);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("read skill directory {}: {e}", dir.display())),
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("read skill directory {}: {e}", dir.display()))?;
            let path = entry.path().join("SKILL.md");
            if path.is_file() {
                files.push(path);
            }
        }
        files.sort();
        for path in files {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("read skill {}: {e}", path.display()))?;
            let stem = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            // Native bodies are not bn templates. Validate metadata only, so native examples
            // containing {{placeholders}} are not treated as undeclared bn arguments.
            let metadata = metadata(&text)
                .ok_or_else(|| format!("{}: missing YAML frontmatter", path.display()))?;
            let skill: NativeMetadata = serde_yaml::from_str(metadata)
                .map_err(|e| format!("{}: invalid frontmatter: {e}", path.display()))?;
            if skill.internal {
                continue;
            }
            let name = skill.name.unwrap_or_else(|| stem.to_string());
            let workflow = workflows.entry(name.clone()).or_insert_with(|| Workflow {
                info: SkillInfo {
                    name: name.clone(),
                    description: String::new(),
                    when: None,
                    keywords: Vec::new(),
                    arguments: Vec::new(),
                    scope: SkillScope::GlobalPersonal,
                    path: String::new(),
                    effective: true,
                    score: None,
                },
                copilot: None,
                claude: None,
                owned: false,
            });
            workflow.info.description = skill.description;
            workflow.info.when = skill.when;
            workflow.info.keywords = skill.keywords;
            workflow.info.scope = if repo {
                SkillScope::RepoShared
            } else {
                SkillScope::GlobalPersonal
            };
            workflow.info.path = path.display().to_string();
            if copilot {
                workflow.copilot = Some(format!("/{name}"));
            }
            if claude {
                workflow.claude = Some(format!("/{name}"));
            }
        }
    }
    Ok(())
}

fn metadata(text: &str) -> Option<&str> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    if first != "---\n" && first != "---\r\n" {
        return None;
    }
    let mut end = first.len();
    for line in lines {
        end += line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some(&text[first.len()..end - line.len()]);
        }
    }
    None
}
