//! Machine-setup drift: tracked `config/stow` files that are not linked into `$HOME`, and skills
//! that are unmanaged or out of date. Pure over two directories, so it is testable
//! with a temp tree; the CLI supplies the real paths.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const MANAGED_MARKER: &str = "bn-managed-skill:";

/// One thing on this machine that has drifted from what the bn checkout tracks.
#[derive(Debug, Serialize, Deserialize)]
pub struct SetupFinding {
    /// The path (stow link or skill dir) that has drifted.
    pub item: String,
    /// What is wrong, as a short phrase.
    pub problem: String,
    /// One-line fix.
    pub fix: String,
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => files_under(&p, out),
            Ok(_) => out.push(p),
            Err(_) => {}
        }
    }
}

/// Compare every file in `stow_dir/<pkg>/…` with `home/…`.
pub fn stow_drift(stow_dir: &Path, home: &Path) -> Vec<SetupFinding> {
    let fix = "./install.sh --force-home".to_string();
    let mut out = Vec::new();
    let Ok(pkgs) = fs::read_dir(stow_dir) else {
        return out;
    };
    let mut pkgs: Vec<_> = pkgs.flatten().filter(|e| e.path().is_dir()).collect();
    pkgs.sort_by_key(|e| e.file_name());
    for pkg in pkgs {
        let mut files = Vec::new();
        files_under(&pkg.path(), &mut files);
        files.sort();
        for src in files {
            let Ok(rel) = src.strip_prefix(pkg.path()) else {
                continue;
            };
            let dest = home.join(rel);
            let problem = match fs::symlink_metadata(&dest) {
                Err(_) => "tracked config is not linked",
                Ok(m) if !m.file_type().is_symlink() => "a real file shadows the tracked config",
                Ok(_) => match (fs::canonicalize(&dest), fs::canonicalize(&src)) {
                    (Ok(a), Ok(b)) if a == b => continue,
                    _ => "link does not point at the tracked config",
                },
            };
            out.push(SetupFinding {
                item: dest.to_string_lossy().into_owned(),
                problem: problem.to_string(),
                fix: fix.clone(),
            });
        }
    }
    out
}

/// Rendering adds an argument preamble, so a byte compare would always differ. A prompt edit
/// changes lines near its start or end far more often than not; check that its first and last
/// lines (minus `{{arg}}` lines, which rendering substitutes) still appear in the installed copy.
fn body_present(body: &str, installed: &str) -> bool {
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("{{"))
        .collect();
    let (head, tail) = (lines.iter().take(5), lines.iter().rev().take(5));
    head.chain(tail)
        .all(|l| installed.lines().any(|i| i.trim() == *l))
}

fn split_frontmatter(src: &str) -> (&str, &str) {
    let Some(rest) = src.strip_prefix("---\n") else {
        return ("", src);
    };
    match rest.split_once("\n---\n") {
        Some((front, body)) => (front, body),
        None => ("", src),
    }
}

/// For each `prompts_dir/<name>.md`, check `skills_dir/<name>/SKILL.md`: missing, unmanaged or
/// skills that no longer match their prompt. Prompts marked `internal: true` are not skills and are skipped.
pub fn skill_drift(prompts_dir: &Path, skills_dir: &Path) -> Vec<SetupFinding> {
    let fix = "bn skills install".to_string();
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(prompts_dir) else {
        return out;
    };
    let mut prompts: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    prompts.sort();
    for p in prompts {
        let Some(name) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(src) = fs::read_to_string(&p) else {
            continue;
        };
        let (front, body) = split_frontmatter(&src);
        if front.lines().any(|l| l.trim() == "internal: true") {
            continue;
        }
        let skill = skills_dir.join(name).join("SKILL.md");
        let problem = match fs::read_to_string(&skill) {
            Err(_) => "skill is not installed",
            Ok(installed) if !installed.contains(MANAGED_MARKER) => "skill is not bn-managed",
            Ok(installed) if !body_present(body, &installed) => {
                "installed skill differs from the checkout"
            }
            Ok(_) => continue,
        };
        out.push(SetupFinding {
            item: skill.to_string_lossy().into_owned(),
            problem: problem.to_string(),
            fix: fix.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn put(p: &Path, body: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn stow_reports_missing_shadowed_and_ok() {
        let t = tempfile::tempdir().unwrap();
        let (stow, home) = (t.path().join("stow"), t.path().join("home"));
        for f in ["a", "b", "c"] {
            put(&stow.join(format!("p/.cfg/{f}")), "x");
        }
        fs::create_dir_all(home.join(".cfg")).unwrap();
        symlink(stow.join("p/.cfg/a"), home.join(".cfg/a")).unwrap();
        put(&home.join(".cfg/b"), "real");
        let got = stow_drift(&stow, &home);
        assert_eq!(got.len(), 2);
        assert!(got[0].problem.contains("shadows"));
        assert!(got[1].problem.contains("not linked"));
    }

    #[test]
    fn skills_report_missing_and_unmanaged() {
        let t = tempfile::tempdir().unwrap();
        let (prompts, skills) = (t.path().join("prompts"), t.path().join("skills"));
        for n in ["one", "two", "three"] {
            put(&prompts.join(format!("{n}.md")), "---\nname: x\n---\nbody");
        }
        put(
            &skills.join("one/SKILL.md"),
            "<!-- bn-managed-skill: one -->\nbody",
        );
        put(&skills.join("two/SKILL.md"), "hand written");
        let got = skill_drift(&prompts, &skills);
        let probs: Vec<_> = got.iter().map(|f| f.problem.as_str()).collect();
        assert!(probs.contains(&"skill is not bn-managed"));
        assert!(probs.contains(&"skill is not installed"));
        assert!(!got.iter().any(|f| f.item.contains("/one/")));
    }
}
