//! Job configuration + discovery: the opt-in autopilot [`JobConfig`], the curated MCP extras,
//! and the branch → repo → global job-script tiers.

use super::config_file::ConfigFile;
use super::{agents_dir, jobs_dir, Config, Resolved};
use serde::Serialize;
use std::collections::BTreeMap;
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

/// The user-scoped MCP servers declared in `[mcp.servers.<name>]`, which `bn mcp sync-user` writes
/// into every MCP client on this machine. Because they resolve through the normal config layering,
/// the shared `<notes_root>/config.toml` declares the ones every machine should have and a
/// machine's own file can add to (or override) them. Empty = nothing beyond what install.sh
/// registers. See [`crate::mcp_user`].
///
/// Returned sorted so a sync run writes — and reports — in a stable order.
pub fn mcp_user_servers() -> BTreeMap<String, crate::mcp_user::UserServer> {
    ConfigFile::load()
        .and_then(|f| f.mcp_servers())
        .unwrap_or_default()
        .into_iter()
        .collect()
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
    script_tier_dirs(cfg, ctx, "jobs", jobs_dir())
}

/// The tiers of [`job_dirs`] for any script kind kept under `sub` (`jobs`, `cmds`): the branch
/// note's `<sub>/`, the project's `.bn/<sub>/`, the notes repo's `<sub>/<repo>/`, then the global
/// `<sub>/` dirs ending in `shipped`. One walk, so a new script kind cannot drift from jobs.
pub fn script_tier_dirs(
    cfg: &Config,
    ctx: Option<&Resolved>,
    sub: &str,
    shipped: PathBuf,
) -> Vec<(JobScope, PathBuf)> {
    let mut dirs = Vec::new();
    if let Some(c) = ctx {
        if c.branch != "main" {
            dirs.push((JobScope::Branch, cfg.note_dir(c).join(sub)));
        }
        // Project tier: this branch's own checkout, preferably at the conventional
        // `<worktree_dir>/<repo>/<branch>` path. When the branch lives in a warm pool slot, a
        // reused checkout directory, or a session whose `BN_WORKTREE_DIR` points elsewhere, that
        // convention cannot name it; fall back to the live checkout (current cwd when it matches,
        // else git's own `worktree list`) so `.bn/jobs` still resolves from the real checkout.
        if let Some(project_root) = project_root(cfg, c) {
            dirs.push((JobScope::Project, project_root.join(".bn").join(sub)));
        }
        // Repo tier: `<notes_dir_for(repo)>/jobs/<repo>/` — no `machine()` segment, so every
        // machine that syncs the notes repo resolves the exact same script. This replaced an
        // earlier per-machine `<machine>/<repo>/main note dir>/jobs/` copy that let a repo's
        // build/start/etc. scripts silently drift between machines.
        dirs.push((
            JobScope::Repo,
            cfg.notes_dir_for(&c.repo).join(sub).join(&c.repo),
        ));
    }
    // Global tiers, highest precedence first: private work notes, private personal notes, then
    // the shipped repo jobs. `<root>/jobs` is machine-agnostic (synced across machines via the
    // notes repo) — true "global", privately. Dedupe so a collapsed-root config (BN_NOTES_DIR in
    // tests) doesn't list the same dir twice.
    for dir in [
        cfg.work_root.join(sub),
        cfg.personal_root.join(sub),
        shipped,
    ] {
        if !dirs.iter().any(|(_, d)| *d == dir) {
            dirs.push((JobScope::Global, dir));
        }
    }
    dirs
}

fn project_root(cfg: &Config, ctx: &Resolved) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    let current_ctx = cwd
        .as_deref()
        .and_then(|dir| crate::resolver::resolve(cfg, dir).ok());
    select_project_root(cfg, ctx, cwd.as_deref(), current_ctx.as_ref())
}

fn select_project_root(
    cfg: &Config,
    ctx: &Resolved,
    current_dir: Option<&Path>,
    current_ctx: Option<&Resolved>,
) -> Option<PathBuf> {
    find_worktree_repo_dir(&cfg.worktree_dir, &ctx.repo)
        .map(|d| d.join(&ctx.branch))
        .filter(|d| d.is_dir())
        .or_else(|| {
            let literal = cfg.worktree_dir.join(&ctx.repo).join(&ctx.branch);
            literal.is_dir().then_some(literal)
        })
        .or_else(|| match (current_dir, current_ctx) {
            (Some(dir), Some(here)) if here.repo == ctx.repo && here.branch == ctx.branch => {
                crate::resolver::repo_root(dir)
            }
            _ => None,
        })
        .or_else(|| crate::lifecycle::worktree_path(cfg, &ctx.repo, &ctx.branch))
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
        // A monorepo's `start.sh` is shaped exactly like this: the prose sits below `set -eu`.
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

    fn git_bin() -> &'static str {
        [
            "/usr/bin/git",
            "/bin/git",
            "/usr/local/bin/git",
            "/opt/homebrew/bin/git",
        ]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("git")
    }

    fn git(args: &[&str]) -> bool {
        std::process::Command::new(git_bin())
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn build_repo_into(srcdir: &Path, bare: &Path) {
        std::fs::create_dir_all(srcdir).unwrap();
        let s = srcdir.to_string_lossy().to_string();
        assert!(git(&["init", "-q", &s]));
        assert!(git(&["-C", &s, "config", "user.email", "t@t"]));
        assert!(git(&["-C", &s, "config", "user.name", "t"]));
        std::fs::write(srcdir.join("a.txt"), "hi\n").unwrap();
        assert!(git(&["-C", &s, "add", "-A"]));
        assert!(git(&["-C", &s, "commit", "-qm", "init"]));
        assert!(git(&["-C", &s, "branch", "-M", "main"]));
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        assert!(git(&["clone", "-q", "--bare", &s, &bare.to_string_lossy()]));
    }

    #[test]
    fn project_job_dir_falls_back_to_a_pooled_checkout_when_conventional_path_is_missing() {
        let _lock = crate::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let worktree_dir = tmp.path().join("worktree");
        std::fs::create_dir_all(&worktree_dir).unwrap();
        let cfg = crate::test_support::config_with_roots(
            tmp.path(),
            tmp.path(),
            &worktree_dir,
            "testhost",
        );
        let bare = cfg.bare_dir().join("bn.git");
        build_repo_into(&tmp.path().join("src"), &bare);

        let pool = worktree_dir.join("bn").join("_pool").join("2");
        std::fs::create_dir_all(pool.parent().unwrap()).unwrap();
        let bare_s = bare.to_string_lossy().to_string();
        let pool_s = pool.to_string_lossy().to_string();
        assert!(git(&[
            "--git-dir",
            &bare_s,
            "worktree",
            "add",
            "-b",
            "explore/20260924-0202",
            &pool_s,
            "main",
        ]));

        let ctx = Resolved {
            repo: "bn".to_string(),
            branch: "explore/20260924-0202".to_string(),
        };
        let project = job_dirs(&cfg, Some(&ctx))
            .into_iter()
            .find(|(scope, _)| *scope == JobScope::Project)
            .map(|(_, dir)| dir);

        assert_eq!(
            project
                .as_deref()
                .and_then(|dir| std::fs::canonicalize(dir).ok()),
            std::fs::canonicalize(pool.join(".bn").join("jobs")).ok()
        );
    }

    #[test]
    fn project_job_dir_uses_the_live_current_checkout_when_worktree_dir_points_elsewhere() {
        let _lock = crate::test_support::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let configured_worktree_dir = tmp.path().join("configured").join("worktree");
        std::fs::create_dir_all(&configured_worktree_dir).unwrap();
        let cfg = crate::test_support::config_with_roots(
            tmp.path(),
            tmp.path(),
            &configured_worktree_dir,
            "testhost",
        );
        let bare = tmp.path().join("live").join("bare").join("bn.git");
        build_repo_into(&tmp.path().join("src"), &bare);

        let pool = tmp
            .path()
            .join("live")
            .join("worktree")
            .join("bn")
            .join("_pool")
            .join("2");
        std::fs::create_dir_all(pool.parent().unwrap()).unwrap();
        let bare_s = bare.to_string_lossy().to_string();
        let pool_s = pool.to_string_lossy().to_string();
        assert!(git(&[
            "--git-dir",
            &bare_s,
            "worktree",
            "add",
            "-b",
            "explore/20260924-0202",
            &pool_s,
            "main",
        ]));
        std::fs::create_dir_all(pool.join(".bn").join("jobs")).unwrap();

        let ctx = Resolved {
            repo: "bn".to_string(),
            branch: "explore-20260924-0202".to_string(),
        };
        let project = select_project_root(&cfg, &ctx, Some(&pool), Some(&ctx));

        assert_eq!(
            project
                .as_deref()
                .and_then(|dir| std::fs::canonicalize(dir).ok()),
            std::fs::canonicalize(&pool).ok()
        );
    }

    #[test]
    fn bn_checkout_ships_local_build_jobs() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        for job in ["build-to-local.sh", "publish-prod.sh"] {
            assert!(
                repo_root.join(".bn").join("jobs").join(job).is_file(),
                "missing committed job script: {job}"
            );
        }
    }
}
