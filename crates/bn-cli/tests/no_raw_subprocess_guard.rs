//! Consolidation guard for issue #357: every `git` / `gh` / `tmux` subprocess must go through the
//! shared seams — `bn_core::runner` (`RealRunner`), `bn_core::tmux` (the shared tmux wrappers),
//! or `bn-cli`'s `git.rs` / `gh.rs` — so each external program has one place that shells out
//! (testable, uniform error/exit semantics). This test fails if a raw
//! `Command::new("git"|"gh"|"tmux")` reappears in any crate's `src/`, keeping the sweep from
//! silently regressing.
//!
//! Note (issue #428): tmux is no longer a CLI-only concern. Its wrappers moved from
//! `bn-cli/src/proc.rs` to `bn_core::tmux` so bn-web / bn-mcp can reuse them in-process; the seam
//! that owns the raw `Command::new("tmux")` is now `bn-core/src/tmux.rs`.
//!
//! Scope is deliberately the three literal program names the seams cover; other programs (fzf, az,
//! ssh, editor, …) legitimately spawn directly, which is why this is a targeted source scan rather
//! than a blanket clippy `disallowed-methods` on `Command::new` (that would demand a noisy
//! `#[allow]` at ~60 innocent sites).

use std::path::{Path, PathBuf};

/// The only files allowed to contain a raw `Command::new("git"|"gh"|"tmux")` — the wrapper modules
/// every other call site routes through. Paths are relative to the `crates/` directory.
const ALLOWED: &[&str] = &[
    "bn-core/src/runner.rs", // RealRunner: the git/gh capture seam
    "bn-core/src/tmux.rs",   // tmux_command / tmux / tmux_raw / tmux_run / tmux_ok
    "bn-cli/src/git.rs",     // git_command / git_run / git_run_quiet
    "bn-cli/src/gh.rs",      // gh_command / gh_capture / gh_ok / gh_run_in
];

/// The forbidden spawns, as the literal source substring. `std::process::Command::new("git")`
/// contains `Command::new("git")`, so matching the substring catches both the bare and
/// fully-qualified forms.
const NEEDLES: &[&str] = &[
    "Command::new(\"git\")",
    "Command::new(\"gh\")",
    "Command::new(\"tmux\")",
];

fn crates_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is this package root (crates/bn-cli); its parent is the crates/ dir.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_raw_git_gh_tmux_subprocess_outside_the_seams() {
    let crates = crates_dir();
    let allowed: Vec<PathBuf> = ALLOWED.iter().map(|rel| crates.join(rel)).collect();

    // Scan every crate's src/ tree (bn-core, bn-cli, bn-mcp, …).
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates)
        .expect("read crates dir")
        .flatten()
    {
        let src = entry.path().join("src");
        if src.is_dir() {
            collect_rs_files(&src, &mut files);
        }
    }
    assert!(
        !files.is_empty(),
        "guard found no source files under {} — path resolution is wrong",
        crates.display()
    );

    let mut violations = Vec::new();
    for file in &files {
        if allowed.iter().any(|a| a == file) {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("read source file");
        for (i, line) in text.lines().enumerate() {
            for needle in NEEDLES {
                if line.contains(needle) {
                    let rel = file.strip_prefix(&crates).unwrap_or(file);
                    violations.push(format!("{}:{} → {}", rel.display(), i + 1, needle));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "raw git/gh/tmux subprocess spawns must route through the shared seams \
         (bn_core::runner / bn_core::tmux / bn-cli git.rs / gh.rs). Offending sites:\n  {}",
        violations.join("\n  ")
    );
}
