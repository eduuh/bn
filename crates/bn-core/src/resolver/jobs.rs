//! Job configuration + discovery: the opt-in autopilot [`JobConfig`], the curated MCP extras,
//! and the branch → repo → global job-script tiers.

use super::config_file::ConfigFile;
use super::{agents_dir, jobs_dir, Config, Resolved};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Resolved config for the autopilot jobs (#98 wrap, #99 standup, #110 DXI) and the opt-in agent
/// list. Kept off [`Config`] so the hot path stays untouched: these flags only matter to the job
/// entry points, which load this on demand. Every flag is **off by default** — the jobs are opt-in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobConfig {
    /// `[wrap].enabled`: the end-of-day digest cron is live.
    pub wrap_enabled: bool,
    /// `[standup].enabled`: the standup-autopilot cron is live.
    pub standup_enabled: bool,
    /// `[standup].fetch_delta`: also do the one-shot PR delta fetch.
    pub standup_fetch_delta: bool,
    /// `[dxi].enabled`: the DXI micro-prompt is opt-in on.
    pub dxi_enabled: bool,
    /// `[agents].enabled`: the opt-in agents (`opt_in: true` in their manifest) that are switched
    /// on, by name. An agent not named here does nothing when its schedule fires, and says so on
    /// the board rather than reading as a missed run.
    pub agents_enabled: Vec<String>,
}

impl JobConfig {
    /// Production entry: the compiled defaults, then the config file layers.
    pub fn load() -> JobConfig {
        JobConfig::resolve(ConfigFile::load().as_ref())
    }

    pub(super) fn resolve(file: Option<&ConfigFile>) -> JobConfig {
        let Some(file) = file else {
            return JobConfig::default();
        };
        JobConfig {
            wrap_enabled: file.wrap_enabled().unwrap_or(false),
            standup_enabled: file.standup_enabled().unwrap_or(false),
            standup_fetch_delta: file.standup_fetch_delta().unwrap_or(false),
            dxi_enabled: file.dxi_enabled().unwrap_or(false),
            agents_enabled: file.agents_enabled().unwrap_or_default(),
        }
    }
}

/// The opt-in curated MCP extras (`[mcp].extras`) that `bn mcp init` and the dispatch setup path
/// wire alongside bn's server (#323). Names are validated against [`crate::mcp::EXTRAS`] at wire
/// time. Empty = bn only (the default).
pub fn mcp_extras() -> Vec<String> {
    ConfigFile::load()
        .and_then(|f| f.mcp_extras())
        .unwrap_or_default()
}

/// Where a job script lives. Jobs resolve branch → project → repo → global, like PATH.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobScope {
    /// `<branch note dir>/jobs/` — scoped to one branch.
    Branch,
    /// `<worktree>/.bn/jobs/` — committed with the project itself, like `.bn/skills`. Travels
    /// with a plain `git clone` of the repo, so every collaborator (and every worktree/branch
    /// carrying the file) gets the same script with no bn-notes setup required.
    Project,
    /// `<notes root>/jobs/<repo>/` — the repo's own maintenance scripts, machine-independent and
    /// synced through the same notes git repo as everything else in `notes_root`. Deliberately
    /// carries no `machine()` segment: one script per repo, not one per machine. A script that
    /// truly needs to branch on machine can do so internally (e.g. `hostname`/`uname`) rather
    /// than bn maintaining separate near-duplicate copies that drift.
    Repo,
    /// `~/.config/bn/jobs/` (→ repo `jobs/`) — machine-level jobs.
    Global,
}

impl JobScope {
    pub fn label(self) -> &'static str {
        match self {
            JobScope::Branch => "branch",
            JobScope::Project => "project",
            JobScope::Repo => "repo",
            JobScope::Global => "global",
        }
    }
}

/// One discovered job script.
#[derive(Debug, Clone, Serialize)]
pub struct JobRef {
    pub name: String,
    pub scope: JobScope,
    pub path: String,
}

/// Candidate job dirs in resolution order for `ctx`: branch, then the project's own committed
/// `.bn/jobs/`, then the repo's machine-independent `jobs/<repo>/` (synced via the notes repo),
/// then global. Global resolves across three tiers — private work-notes `jobs/`, private
/// personal-notes `jobs/`, then the shipped `jobs_dir()` — so a sensitive global job lives in
/// your synced notes (private) and shadows a same-named shipped default.
pub fn job_dirs(cfg: &Config, ctx: Option<&Resolved>) -> Vec<(JobScope, PathBuf)> {
    let mut dirs = Vec::new();
    if let Some(c) = ctx {
        if c.branch != "main" {
            dirs.push((JobScope::Branch, cfg.note_dir(c).join("jobs")));
        }
        // Project tier: this branch's own checkout, at the conventional `<worktree_dir>/<repo>/
        // <branch>` path bn's own `wt` layout uses — no cwd needed, so cron/web/CLI callers all
        // resolve it the same way regardless of process cwd. `c.repo` is always lowercased (note
        // key normalization), but the on-disk worktree dir preserves its original case (e.g.
        // `BigMono`, not `bigmono`) — a literal join would silently miss it on a case-sensitive fs, so
        // look the entry up case-insensitively first and fall back to the literal join (covers
        // case-insensitive filesystems and repos that happen to already be lowercase).
        if let Some(project_root) = find_worktree_repo_dir(&cfg.worktree_dir, &c.repo)
            .map(|d| d.join(&c.branch))
            .filter(|d| d.is_dir())
            .or_else(|| {
                let literal = cfg.worktree_dir.join(&c.repo).join(&c.branch);
                literal.is_dir().then_some(literal)
            })
        {
            dirs.push((JobScope::Project, project_root.join(".bn").join("jobs")));
        }
        // Repo tier: `<notes_dir_for(repo)>/jobs/<repo>/` — no `machine()` segment, so every
        // machine that syncs the notes repo resolves the exact same script. This replaced an
        // earlier per-machine `<machine>/<repo>/main note dir>/jobs/` copy that let a repo's
        // build/start/etc. scripts silently drift between machines.
        dirs.push((
            JobScope::Repo,
            cfg.notes_dir_for(&c.repo).join("jobs").join(&c.repo),
        ));
    }
    // Global tiers, highest precedence first: private work notes, private personal notes, then
    // the shipped repo jobs. `<root>/jobs` is machine-agnostic (synced across machines via the
    // notes repo) — true "global", privately. Dedupe so a collapsed-root config (BN_NOTES_DIR in
    // tests) doesn't list the same dir twice.
    for dir in [
        cfg.work_root.join("jobs"),
        cfg.personal_root.join("jobs"),
        jobs_dir(),
    ] {
        if !dirs.iter().any(|(_, d)| *d == dir) {
            dirs.push((JobScope::Global, dir));
        }
    }
    dirs
}

/// Find `<worktree_dir>/<name>` case-insensitively — the physical worktree directory preserves
/// whatever case the repo was cloned/checked-out with (e.g. `BigMono`), while every `Resolved.repo`
/// is lowercased for note-key purposes. `None` when `worktree_dir` itself isn't readable or no
/// entry matches.
fn find_worktree_repo_dir(worktree_dir: &Path, name_lower: &str) -> Option<PathBuf> {
    std::fs::read_dir(worktree_dir).ok()?.find_map(|entry| {
        let entry = entry.ok()?;
        let file_name = entry.file_name();
        (file_name.to_string_lossy().to_lowercase() == name_lower).then(|| entry.path())
    })
}

/// Candidate self-describing-agent dirs in resolution order for `ctx` — the agent analogue of
/// [`job_dirs`], resolving the `agents/` subdir of the same branch → repo → global note tiers so
/// a branch's `agents/<name>.md` shadows a repo or shipped default. The global tiers are the
/// private work-notes `agents/`, the private personal-notes `agents/`, then the shipped
/// [`agents_dir`]. Deduped so a collapsed-root test config never lists a dir twice.
pub fn agent_dirs(cfg: &Config, ctx: Option<&Resolved>) -> Vec<(JobScope, PathBuf)> {
    let mut dirs = Vec::new();
    if let Some(c) = ctx {
        if c.branch == "main" {
            dirs.push((JobScope::Repo, cfg.note_dir(c).join("agents")));
        } else {
            dirs.push((JobScope::Branch, cfg.note_dir(c).join("agents")));
            let repo_main = Resolved {
                repo: c.repo.clone(),
                branch: "main".to_string(),
            };
            dirs.push((JobScope::Repo, cfg.note_dir(&repo_main).join("agents")));
        }
    }
    for dir in [
        cfg.overlay_root.as_ref().map(|r| r.join("agents")),
        Some(cfg.work_root.join("agents")),
        Some(cfg.personal_root.join("agents")),
        Some(agents_dir()),
    ]
    .into_iter()
    .flatten()
    {
        if !dirs.iter().any(|(_, d)| *d == dir) {
            dirs.push((JobScope::Global, dir));
        }
    }
    dirs
}

/// Resolve a job name to `<dir>/<job>.sh`, trying each tier in order. First match wins, so a
/// branch job shadows a repo or global one of the same name. `None` if no tier has it.
pub fn job_script(cfg: &Config, ctx: Option<&Resolved>, job: &str) -> Option<(JobScope, PathBuf)> {
    for (scope, dir) in job_dirs(cfg, ctx) {
        let path = dir.join(format!("{job}.sh"));
        if path.is_file() {
            return Some((scope, path));
        }
    }
    None
}

/// Every `*.sh` job across all tiers for `ctx`, tagged with its scope. Sorted by scope
/// (branch first) then name. Names can repeat across tiers — the higher tier shadows on run.
pub fn list_jobs(cfg: &Config, ctx: Option<&Resolved>) -> Vec<JobRef> {
    let mut out = Vec::new();
    for (scope, dir) in job_dirs(cfg, ctx) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_string_lossy()
                    .strip_suffix(".sh")
                    .map(str::to_string)
            })
            .collect();
        names.sort();
        for name in names {
            let path = dir.join(format!("{name}.sh")).display().to_string();
            out.push(JobRef { name, scope, path });
        }
    }
    out
}

/// What a job script declares about itself, read from its leading comments.
///
/// Job scripts are plain `sh`, so the comments at the top are the only place for metadata a runner
/// can act on. One directive so far — `# bn: latest-main`, which asks whoever runs the script to
/// fetch and fast-forward the repo's main *before* starting it. The runner honours it, not the
/// surface that started the script, so a script gets latest main whether a cron, `bn run` or the
/// picker asked; the picker's side pane reads the same header to say so up front.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptHeader {
    /// `# bn: latest-main` — sync the repo's main checkout before running.
    pub latest_main: bool,
    /// The leading comment block, shebang and directive lines removed — what a side pane shows as
    /// the script's own description. Capped at [`SCRIPT_HEADER_LINES`].
    pub comments: Vec<String>,
}

/// How much of a script the header parser reads. A bound rather than the whole file, because the
/// picker's side pane asks on every cursor move and a build script can carry a large heredoc.
pub const SCRIPT_HEADER_BYTES: usize = 2048;

/// How many comment lines a header keeps for display.
pub const SCRIPT_HEADER_LINES: usize = 8;

/// Read a script's header. A missing or unreadable file is an empty header, not an error: the
/// caller already has a path it believed in, and "nothing declared" is the honest answer.
pub fn script_header(path: &Path) -> ScriptHeader {
    use std::io::Read;
    let mut buf = vec![0u8; SCRIPT_HEADER_BYTES];
    let mut n = 0;
    if let Ok(mut file) = std::fs::File::open(path) {
        while n < buf.len() {
            match file.read(&mut buf[n..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => n += k,
            }
        }
    }
    parse_script_header(&String::from_utf8_lossy(&buf[..n]))
}

/// The pure half of [`script_header`], over the text the bounded read produced.
///
/// A directive line counts wherever it appears in the read window — `set -eu` often precedes the
/// first comment, and a rule that only honoured directives above it would be a rule nobody could
/// see failing. The *comments* are only the leading block: the first non-blank, non-comment line
/// ends them, because past that a `#` is as likely a commented-out command as a description.
pub fn parse_script_header(text: &str) -> ScriptHeader {
    let mut out = ScriptHeader::default();
    let mut in_leading_block = true;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if i == 0 && line.starts_with("#!") {
            continue;
        }
        if let Some(directives) = line
            .strip_prefix("# bn:")
            .or_else(|| line.strip_prefix("#bn:"))
        {
            for token in directives.split(',').map(str::trim) {
                if token.eq_ignore_ascii_case("latest-main") {
                    out.latest_main = true;
                }
            }
            continue;
        }
        if !in_leading_block {
            continue;
        }
        match line.strip_prefix('#') {
            Some(comment) if out.comments.len() < SCRIPT_HEADER_LINES => {
                out.comments.push(comment.trim().to_string());
            }
            Some(_) => {}
            // A blank line before any comment is still the top of the file; one after the block
            // has started closes it, the same way a paragraph ends.
            None if line.is_empty() && out.comments.is_empty() => {}
            None => in_leading_block = false,
        }
    }
    // A blank comment line (`#` alone) at either end is spacing, not a description.
    while out.comments.last().is_some_and(String::is_empty) {
        out.comments.pop();
    }
    while out.comments.first().is_some_and(String::is_empty) {
        out.comments.remove(0);
    }
    out
}

#[cfg(test)]
mod script_header_tests {
    use super::*;

    #[test]
    fn a_directive_is_honoured_wherever_it_sits_in_the_window() {
        let above = parse_script_header("#!/bin/sh\n# bn: latest-main\nset -eu\n");
        assert!(above.latest_main);
        let below = parse_script_header("#!/bin/sh\nset -eu\n\n# bn: latest-main\nyarn build\n");
        assert!(
            below.latest_main,
            "a directive after `set -eu` still counts"
        );
        let none = parse_script_header("#!/bin/sh\n# builds the thing\nyarn build\n");
        assert!(!none.latest_main);
    }

    #[test]
    fn directives_are_a_comma_list_and_unknown_tokens_are_ignored() {
        let h = parse_script_header("# bn: something-else, LATEST-MAIN\n");
        assert!(h.latest_main);
        assert!(
            h.comments.is_empty(),
            "a directive line is not a description"
        );
    }

    #[test]
    fn the_comments_are_the_leading_block_without_shebang_or_directives() {
        let h = parse_script_header(
            "#!/bin/sh\n# bn: latest-main\n# Builds the app.\n#\n# Idempotent.\nset -eu\n# not this\n",
        );
        assert_eq!(h.comments, vec!["Builds the app.", "", "Idempotent."]);
    }

    #[test]
    fn a_script_whose_comments_follow_its_setup_line_has_no_description() {
        // BigMono's `start.sh` is shaped exactly like this: the prose sits below `set -eu`.
        let h = parse_script_header("#!/bin/sh\nset -eu\n\n# node/yarn may live under nvm\n");
        assert!(h.comments.is_empty());
    }

    #[test]
    fn the_description_is_capped() {
        let text: String = (0..20).map(|i| format!("# line {i}\n")).collect();
        assert_eq!(
            parse_script_header(&text).comments.len(),
            SCRIPT_HEADER_LINES
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_header() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            script_header(&tmp.path().join("nope.sh")),
            ScriptHeader::default()
        );
    }

    #[test]
    fn the_read_is_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.sh");
        let mut text = String::from("#!/bin/sh\n# top\n");
        text.push_str(&"x".repeat(SCRIPT_HEADER_BYTES * 4));
        text.push_str("\n# bn: latest-main\n");
        std::fs::write(&path, text).unwrap();
        let h = script_header(&path);
        assert_eq!(h.comments, vec!["top"]);
        assert!(
            !h.latest_main,
            "a directive past the window is not seen — by design"
        );
    }
}
