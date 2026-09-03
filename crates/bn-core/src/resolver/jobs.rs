//! Job configuration + discovery: the opt-in autopilot [`JobConfig`], the curated MCP
//! extras / quarter-review overrides, and the branch → repo → global job-script tiers.

use super::config_file::ConfigFile;
use super::util::{non_empty_env, split_list};
use super::{agents_dir, jobs_dir, Config, Resolved};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Resolved config for the autopilot jobs (#98 wrap, #99 standup, #110 DXI). Kept off [`Config`]
/// so the hot path stays untouched: these flags only matter to the three job entry points, which
/// load this on demand. Layering mirrors [`Config`]: `config.toml` then `BN_*` env overrides.
/// Every flag is **off by default** — the jobs are opt-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobConfig {
    /// `[wrap].enabled` / `$BN_WRAP_ENABLED`: the end-of-day digest cron is live.
    pub wrap_enabled: bool,
    /// `[wrap].time` / `$BN_WRAP_TIME`: local `HH:MM` the digest fires (default `17:00`).
    pub wrap_time: String,
    /// `[standup].enabled` / `$BN_STANDUP_ENABLED`: the standup-autopilot cron is live.
    pub standup_enabled: bool,
    /// `[standup].time` / `$BN_STANDUP_TIME`: local `HH:MM` standup fires (default `09:00`).
    pub standup_time: String,
    /// `[standup].fetch_delta` / `$BN_STANDUP_FETCH_DELTA`: also do the one-shot PR delta fetch.
    pub standup_fetch_delta: bool,
    /// `[dxi].enabled` / `$BN_DXI_ENABLED`: the DXI micro-prompt is opt-in on.
    pub dxi_enabled: bool,
    /// `[night].enabled` / `$BN_NIGHT_ENABLED`: the nightly fleet planner (`night-shift` job) is
    /// live. Off by default — the shift never dispatches unless this is set AND repos allowlisted.
    pub night_enabled: bool,
    /// `[night].max_dispatch` / `$BN_NIGHT_MAX_DISPATCH`: hard cap on promoted todos one shift
    /// dispatches. Default 3.
    pub night_max_dispatch: usize,
    /// `[night].window` / `$BN_NIGHT_WINDOW`: local `HH:MM-HH:MM` the shift may run in (default
    /// `23:00-06:00`). A manual `bn night run` outside it declines unless `--force`.
    pub night_window: String,
    /// `[night].repos` / `$BN_NIGHT_REPOS` (comma/space-separated): the allowlist of bare repo
    /// names the shift may touch. Empty = nothing runs (opt-in per repo).
    pub night_repos: Vec<String>,
    /// `[sentinel].repos` / `$BN_SENTINEL_REPOS` (comma/space-separated): the allowlist of bare
    /// repo names the Debt Sentinel patrol (#259) scans and may promote from. Defaults to
    /// `night_repos` (the two stay in lockstep); empty = the patrol is a no-op.
    pub sentinel_repos: Vec<String>,
    /// `[sentinel].todo_age_days` / `$BN_SENTINEL_TODO_AGE_DAYS`: how old a TODO/FIXME marker must
    /// be before the Sentinel flags it. Default 30.
    pub sentinel_todo_age_days: i64,
    /// `[quarter].enabled` / `$BN_QUARTER_REVIEW_ENABLED`: the recurring quarter impact-agent
    /// review cron is live. Off by default — it dispatches an LLM harness.
    pub quarter_review_enabled: bool,
    /// `[quarter].review_window_days` / `$BN_QUARTER_REVIEW_WINDOW_DAYS`: ground-truth window
    /// (days) when there's no prior review to tile against. Default 7 (the weekly cadence).
    pub quarter_review_window_days: i64,
    /// `[quarter].harness` / `$BN_QUARTER_HARNESS`: harness the impact agent uses (`copilot` /
    /// `claude`). `None` falls back to `review_harness`, then `copilot`.
    pub quarter_harness: Option<String>,
    /// `[dojo].enabled` / `$BN_DOJO_ENABLED`: the nightly `dojo-cards` mining cron is live. Off by
    /// default — mining is opt-in — but `bn dojo` sessions always run against whatever's mined.
    pub dojo_enabled: bool,
    /// `[dojo].repos` / `$BN_DOJO_REPOS` (comma/space-separated): the rotation allowlist of bare
    /// repo names the session + miner draw from. Empty = every repo with cards.
    pub dojo_repos: Vec<String>,
    /// `[dojo].daily_target` / `$BN_DOJO_DAILY_TARGET`: how many cards one `bn dojo` session
    /// presents. Default 5.
    pub dojo_daily_target: usize,
    /// `[chief].enabled` / `$BN_CHIEF_ENABLED`: the Chief of Staff briefing jobs
    /// (`staff-chief-am` / `staff-chief-pm`) are live. Read only by the legacy fold in
    /// [`super::config_file`], which maps it onto `[agents] enabled`.
    pub chief_enabled: bool,
    /// `[archivist].enabled` / `$BN_ARCHIVIST_ENABLED`: the Archivist's Friday weekly-chronicle job
    /// (`staff-archivist`) is live. Off by default — it writes a file into the private notes repo,
    /// so it stays opt-in. A manual `bn archivist` runs regardless of this flag.
    pub archivist_enabled: bool,
    /// `[retro].enabled` / `$BN_RETRO_ENABLED`: the Interviewer (#261) `staff-interviewer` cron is
    /// live — it raises the Friday weekly-retro attention item and escalates it once on Monday.
    /// Off by default (it raises a weekly nag), so it stays opt-in. The session itself
    /// (`bn retro chat`) always runs by hand regardless of this flag; it is never dispatched.
    pub retro_enabled: bool,
    /// `[mentor].enabled` / `$BN_MENTOR_ENABLED`: the Mentor's post-merge review cron
    /// (`staff-mentor`) is live. Off by default — it dispatches an LLM harness and appends craft
    /// notes to your branch notes, so it stays opt-in. A manual `bn mentor` runs regardless.
    /// `[agents].enabled` / `$BN_AGENTS_ENABLED`: the opt-in agents (`opt_in: true` in their
    /// manifest) that are switched on, by name. An agent not named here does nothing when its
    /// schedule fires, and says so on the board rather than reading as a missed run.
    pub agents_enabled: Vec<String>,
    /// `[today].enabled` / `$BN_TODAY_ENABLED`: Today's agent cron (`today-agent`, #577) is live.
    /// Off by default — a scheduled run writes reviewable suggestions — so it stays opt-in. A
    /// manual `bn today` runs regardless of this flag.
    pub today_enabled: bool,
    /// `[attention_agent].enabled` / `$BN_ATTENTION_AGENT_ENABLED`: the periodic LLM `attention-agent`
    /// cron is live. Off by default — a scheduled run spends a model call — so it stays opt-in.
    pub attention_agent_enabled: bool,
}

impl Default for JobConfig {
    fn default() -> Self {
        JobConfig {
            wrap_enabled: false,
            wrap_time: "17:00".into(),
            standup_enabled: false,
            standup_time: "09:00".into(),
            standup_fetch_delta: false,
            dxi_enabled: false,
            night_enabled: false,
            night_max_dispatch: 3,
            night_window: "23:00-06:00".into(),
            night_repos: Vec::new(),
            sentinel_repos: Vec::new(),
            sentinel_todo_age_days: crate::estate::DEFAULT_TODO_AGE_DAYS,
            quarter_review_enabled: false,
            quarter_review_window_days: 7,
            quarter_harness: None,
            dojo_enabled: false,
            dojo_repos: Vec::new(),
            dojo_daily_target: crate::dojo::DEFAULT_DAILY_TARGET,
            chief_enabled: false,
            archivist_enabled: false,
            retro_enabled: false,
            agents_enabled: Vec::new(),
            today_enabled: false,
            attention_agent_enabled: false,
        }
    }
}

impl JobConfig {
    /// Production entry: layer defaults → `config.toml` → `BN_*` env overrides.
    pub fn load() -> JobConfig {
        JobConfig::resolve(ConfigFile::load().as_ref())
    }

    pub(super) fn resolve(file: Option<&ConfigFile>) -> JobConfig {
        let env_bool =
            |key: &str| non_empty_env(key).map(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        let d = JobConfig::default();
        // Resolved once up front so the Sentinel allowlist can default to it (#259).
        let night_repos = non_empty_env("BN_NIGHT_REPOS")
            .map(|v| split_list(&v))
            .or_else(|| file.and_then(|f| f.night_repos()))
            .unwrap_or_else(|| d.night_repos.clone());
        JobConfig {
            wrap_enabled: env_bool("BN_WRAP_ENABLED")
                .or_else(|| file.and_then(|f| f.wrap_enabled()))
                .unwrap_or(d.wrap_enabled),
            wrap_time: non_empty_env("BN_WRAP_TIME")
                .or_else(|| file.and_then(|f| f.wrap_time()))
                .unwrap_or(d.wrap_time),
            standup_enabled: env_bool("BN_STANDUP_ENABLED")
                .or_else(|| file.and_then(|f| f.standup_enabled()))
                .unwrap_or(d.standup_enabled),
            standup_time: non_empty_env("BN_STANDUP_TIME")
                .or_else(|| file.and_then(|f| f.standup_time()))
                .unwrap_or(d.standup_time),
            standup_fetch_delta: env_bool("BN_STANDUP_FETCH_DELTA")
                .or_else(|| file.and_then(|f| f.standup_fetch_delta()))
                .unwrap_or(d.standup_fetch_delta),
            dxi_enabled: env_bool("BN_DXI_ENABLED")
                .or_else(|| file.and_then(|f| f.dxi_enabled()))
                .unwrap_or(d.dxi_enabled),
            night_enabled: env_bool("BN_NIGHT_ENABLED")
                .or_else(|| file.and_then(|f| f.night_enabled()))
                .unwrap_or(d.night_enabled),
            night_max_dispatch: non_empty_env("BN_NIGHT_MAX_DISPATCH")
                .and_then(|v| v.parse::<usize>().ok())
                .or_else(|| file.and_then(|f| f.night_max_dispatch()))
                .unwrap_or(d.night_max_dispatch),
            night_window: non_empty_env("BN_NIGHT_WINDOW")
                .or_else(|| file.and_then(|f| f.night_window()))
                .unwrap_or(d.night_window),
            night_repos: night_repos.clone(),
            sentinel_repos: non_empty_env("BN_SENTINEL_REPOS")
                .map(|v| split_list(&v))
                .or_else(|| file.and_then(|f| f.sentinel_repos()))
                // Default to the night-shift allowlist so the two stay in lockstep (#259).
                .unwrap_or(night_repos),
            sentinel_todo_age_days: non_empty_env("BN_SENTINEL_TODO_AGE_DAYS")
                .and_then(|v| v.parse::<i64>().ok())
                .or_else(|| file.and_then(|f| f.sentinel_todo_age_days()))
                .filter(|n| *n > 0)
                .unwrap_or(d.sentinel_todo_age_days),
            quarter_review_enabled: env_bool("BN_QUARTER_REVIEW_ENABLED")
                .or_else(|| file.and_then(|f| f.quarter_enabled()))
                .unwrap_or(d.quarter_review_enabled),
            quarter_review_window_days: non_empty_env("BN_QUARTER_REVIEW_WINDOW_DAYS")
                .and_then(|v| v.parse::<i64>().ok())
                .or_else(|| file.and_then(|f| f.quarter_review_window_days()))
                .filter(|n| *n > 0)
                .unwrap_or(d.quarter_review_window_days),
            quarter_harness: non_empty_env("BN_QUARTER_HARNESS")
                .or_else(|| file.and_then(|f| f.quarter_harness())),
            dojo_enabled: env_bool("BN_DOJO_ENABLED")
                .or_else(|| file.and_then(|f| f.dojo_enabled()))
                .unwrap_or(d.dojo_enabled),
            dojo_repos: non_empty_env("BN_DOJO_REPOS")
                .map(|v| split_list(&v))
                .or_else(|| file.and_then(|f| f.dojo_repos()))
                .unwrap_or(d.dojo_repos),
            dojo_daily_target: non_empty_env("BN_DOJO_DAILY_TARGET")
                .and_then(|v| v.parse::<usize>().ok())
                .or_else(|| file.and_then(|f| f.dojo_daily_target()))
                .filter(|n| *n > 0)
                .unwrap_or(d.dojo_daily_target),
            chief_enabled: env_bool("BN_CHIEF_ENABLED")
                .or_else(|| file.and_then(|f| f.chief_enabled()))
                .unwrap_or(d.chief_enabled),
            archivist_enabled: env_bool("BN_ARCHIVIST_ENABLED")
                .or_else(|| file.and_then(|f| f.archivist_enabled()))
                .unwrap_or(d.archivist_enabled),
            retro_enabled: env_bool("BN_RETRO_ENABLED")
                .or_else(|| file.and_then(|f| f.retro_enabled()))
                .unwrap_or(d.retro_enabled),
            agents_enabled: non_empty_env("BN_AGENTS_ENABLED")
                .map(|v| split_list(&v))
                .or_else(|| file.and_then(|f| f.agents_enabled()))
                .unwrap_or_else(|| d.agents_enabled.clone()),
            today_enabled: env_bool("BN_TODAY_ENABLED")
                .or_else(|| file.and_then(|f| f.today_enabled()))
                .unwrap_or(d.today_enabled),
            attention_agent_enabled: env_bool("BN_ATTENTION_AGENT_ENABLED")
                .or_else(|| file.and_then(|f| f.attention_agent_enabled()))
                .unwrap_or(d.attention_agent_enabled),
        }
    }
}

/// The opt-in curated MCP extras (`[mcp].extras` / `$BN_MCP_EXTRAS`, comma/space-separated) that
/// `bn mcp init` and the dispatch setup path wire alongside bn's server (#323). Names are
/// validated against [`crate::mcp::EXTRAS`] at wire time. Empty = bn only (the default).
pub fn mcp_extras() -> Vec<String> {
    non_empty_env("BN_MCP_EXTRAS")
        .map(|v| split_list(&v))
        .or_else(|| ConfigFile::load().as_ref().and_then(|f| f.mcp_extras()))
        .unwrap_or_default()
}

/// A legacy second schedule authority that must be migrated to an overriding
/// `agents/quarter-review.md` manifest.
pub fn quarter_review_cron_override() -> Option<(PathBuf, String)> {
    if let Some(value) = non_empty_env("BN_QUARTER_REVIEW_CRON") {
        return Some((PathBuf::from("$BN_QUARTER_REVIEW_CRON"), value));
    }
    ConfigFile::load()
        .as_ref()
        .and_then(|file| file.quarter_review_cron())
        .map(|value| (super::config_file_path(), value))
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
