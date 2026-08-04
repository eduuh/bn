//! The Scout (#262) — the Saturday read-only radar. It watches the *outside* world (upstream
//! issues, dependency releases, tooling changelogs) and files findings as **research items** on
//! the relevant repo's note. It never writes anything public: no comments, stars, reactions, or
//! any authenticated write — the collectors here only *read*.
//!
//! This module is the deterministic **gather** half of the Scout staff agent (see
//! [`crate::staff`]): `bn staff gather scout` calls [`gather_packet`], whose JSON is the agent's
//! entire world. The agent's job is judgment — from the collected raw items it picks ≤ 5 worth
//! attention, each with a one-line "why you specifically". The collectors' job is everything that
//! must be deterministic and testable:
//!
//! - **Three collectors, each degradable in isolation** ([`collect_dependency_radar`],
//!   [`collect_upstream`], [`collect_tooling_delta`]). A collector's network failure sets
//!   [`Section::degraded`] on *that* section only; the packet still builds. Results are cached
//!   per-day ([`with_cache`]) so a same-day re-run costs no network and a failed live fetch can
//!   fall back to the last good cache.
//! - **Dedup against the last 4 weeks of Scout cards** ([`recent_scout_keys`]) so the same release
//!   or issue is never re-surfaced (acceptance: ≤ 5 findings, deduped over [`DEDUP_WEEKS`]).
//! - **"Why-you" matching** ([`subsystem_tokens`] + [`why_match`]): an upstream issue only
//!   surfaces if it touches a subsystem you've recently worked in, matched against your merged-PR
//!   file paths — read locally, never sent outbound.
//! - **Privacy guard** ([`planned_outbound`] + [`leaked_identifiers`]): the *only* outbound tokens
//!   are the public watchlist repos and public dependency/tool names. Private-repo identifiers
//!   (the `used_by` field of the inventory, your recently-merged repos) stay local; every outbound
//!   `gh` call is checked and skipped fail-closed if it would carry one.
//!
//! **Dependency inventory (shared cache).** The dependency radar reads a
//! [`DependencyInventory`] the Sentinel is meant to build (issue #262 "Depends on"). Until that
//! lands, the file is simply absent and the dependency section degrades with a note — the Scout
//! never blocks on it. The schema and path ([`inventory_path`]) are defined here so either agent
//! can build it first.

use crate::events::Event;
use crate::resolver::Config;
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// The Scout's own configuration, from the `[scout]` table of `config.toml` (resolved into
/// [`Config::scout`]). Owned here so the config layer only forwards it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoutConfig {
    /// Public `owner/repo` watchlist — the upstreams whose help-wanted / good-first issues the
    /// Scout scans. Public repos by definition (they are outbound search tokens).
    pub watch: Vec<String>,
    /// The tools in your own stack, as public `owner/repo` GitHub slugs — the Scout reads their
    /// latest release / changelog. Outbound tokens, so public by definition.
    pub tools: Vec<String>,
    /// Hard cap on findings the agent may keep (default [`DEFAULT_MAX_FINDINGS`]). Carried in the
    /// packet so the prompt states the same ceiling the acceptance criteria fix.
    pub max_findings: usize,
}

impl Default for ScoutConfig {
    fn default() -> Self {
        ScoutConfig {
            watch: Vec::new(),
            tools: Vec::new(),
            max_findings: DEFAULT_MAX_FINDINGS,
        }
    }
}

/// The findings ceiling the Scout enforces — the "≤ 5" of the spec.
pub const DEFAULT_MAX_FINDINGS: usize = 5;

/// Dedup horizon: a finding already surfaced on a Scout card within this many weeks is dropped.
pub const DEDUP_WEEKS: i64 = 4;

/// Recency horizon (days) for "subsystems you've recently worked in" — the merged-PR path window.
pub const RECENT_WORK_DAYS: i64 = 60;

/// Per-label issue fetch cap per watch repo — keeps the packet bounded.
pub const ISSUES_PER_LABEL: usize = 20;

/// Dependency inventory entries the radar enriches per run — the top-N most-depended-on.
pub const DEP_RADAR_CAP: usize = 15;

/// The upstream labels the Scout scans for. Exactly the "cheap to find, high-leverage" surface the
/// spec names — an open issue a maintainer has flagged as a good outside contribution.
pub const UPSTREAM_LABELS: &[&str] = &["help wanted", "good first issue"];

// --- dependency inventory (shared cache, Sentinel-owned) ----------------------------------------

/// The dependency inventory the Sentinel builds and the Scout reads — a machine-local JSON cache
/// at [`inventory_path`]. Each [`InventoryDep`] pairs a public dependency (`name` + its public
/// source `repo`) with the **private** repos that use it (`used_by`) — the private half stays in
/// the packet for "why-you" context and is never an outbound token.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DependencyInventory {
    /// Date the Sentinel generated this (`YYYY-MM-DD`); informational.
    #[serde(default)]
    pub generated: String,
    #[serde(default)]
    pub deps: Vec<InventoryDep>,
}

/// One dependency in the [`DependencyInventory`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InventoryDep {
    /// Package name as used in a manifest (e.g. `serde`) — public.
    pub name: String,
    /// Public GitHub source slug `owner/repo` (e.g. `serde-rs/serde`); empty when unknown, in
    /// which case the radar can't check releases for it. The only outbound token for this dep.
    #[serde(default)]
    pub repo: String,
    /// Version currently in use (for breaking-change detection). Empty when unknown.
    #[serde(default)]
    pub current: String,
    /// Ecosystem tag (`cargo` / `npm` / …) — informational.
    #[serde(default)]
    pub ecosystem: String,
    /// Your (possibly private) repos that depend on this — the "why-you" context. **Never** sent
    /// outbound; guarded as a private identifier.
    #[serde(default)]
    pub used_by: Vec<String>,
}

/// The inventory cache path: `<state>/scout/dependency-inventory.json`.
pub fn inventory_path(cfg: &Config) -> PathBuf {
    cfg.state_dir()
        .join("scout")
        .join("dependency-inventory.json")
}

/// Load the dependency inventory if the Sentinel has written one; `None` when the file is absent
/// or unparseable (the radar then degrades with a note — never an error).
pub fn load_inventory(cfg: &Config) -> Option<DependencyInventory> {
    let text = std::fs::read_to_string(inventory_path(cfg)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The top-N deps to enrich, most-used first (by `used_by` count, then name) — bounds the outbound
/// surface to what matters.
pub fn top_deps(inv: &DependencyInventory, cap: usize) -> Vec<InventoryDep> {
    let mut deps = inv.deps.clone();
    deps.sort_by(|a, b| {
        b.used_by
            .len()
            .cmp(&a.used_by.len())
            .then_with(|| a.name.cmp(&b.name))
    });
    deps.truncate(cap);
    deps
}

// --- collector output types ---------------------------------------------------------------------

/// A collector's result: its items plus whether it degraded (a network/auth failure) and any
/// per-item notes. A degraded section carries an `error` and (usually) still whatever items a
/// stale cache or partial fetch provided — degradation is isolated to the one section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section<T> {
    pub items: Vec<T>,
    /// Set when the whole section failed to gather live and no cache stood in — the agent should
    /// treat this section as "unknown", not "empty".
    pub degraded: bool,
    /// A human reason for a degrade, or a partial-failure note.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
    /// Non-fatal notes (a skipped leak, a served-stale-cache marker, an unparseable row).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notes: Vec<String>,
}

impl<T> Default for Section<T> {
    fn default() -> Self {
        Section {
            items: Vec::new(),
            degraded: false,
            error: None,
            notes: Vec::new(),
        }
    }
}

/// A dependency-radar candidate: a release / breaking change / deprecation / advisory on a dep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DependencyItem {
    pub name: String,
    /// Public source repo (outbound token).
    pub repo: String,
    #[serde(default)]
    pub current: String,
    #[serde(default)]
    pub latest: String,
    /// `release` | `breaking` | `deprecation` | `advisory`.
    pub kind: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub link: String,
    /// Local why-you context — the (possibly private) repos affected. Never outbound.
    #[serde(default)]
    pub used_by: Vec<String>,
}

/// An upstream-issue candidate — an open help-wanted / good-first issue on a watchlist repo that
/// matched a subsystem you've recently worked in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpstreamItem {
    pub repo: String,
    pub number: i64,
    pub title: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub link: String,
    /// The subsystem tokens (from your merged-PR paths) this issue matched — the "why-you".
    #[serde(default)]
    pub matched: Vec<String>,
}

/// A tooling-delta candidate — the latest release/changelog of a tool in your stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolingItem {
    /// Public tool repo slug (outbound token).
    pub tool: String,
    #[serde(default)]
    pub latest: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub link: String,
}

/// The privacy audit carried on every packet — the outbound tokens the run used and the private
/// identifiers guarded against. `leaks` MUST be empty; any entry means a call was skipped
/// fail-closed rather than leak.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrivacyReport {
    /// Every distinct token that appears in a planned outbound request (all public).
    pub outbound_tokens: Vec<String>,
    /// The private-repo identifiers guarded against this run.
    pub private_guarded: Vec<String>,
    /// Any private identifier that would have leaked — always empty in a healthy run.
    pub leaks: Vec<String>,
}

/// The full Scout packet — the agent's entire world.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoutPacket {
    pub agent: String,
    pub date: String,
    pub machine: String,
    /// Echoed watchlist (public).
    pub watchlist: Vec<String>,
    pub dependency_radar: Section<DependencyItem>,
    pub upstream_matches: Section<UpstreamItem>,
    pub tooling_delta: Section<ToolingItem>,
    /// Subsystem tokens from your recent merged-PR paths — the why-you vocabulary (local).
    pub recent_subsystems: Vec<String>,
    /// The dedup keys already surfaced on Scout cards in the last [`DEDUP_WEEKS`] — the agent must
    /// not re-file any of these (candidates matching them are already filtered out).
    pub deduped_recent: Vec<String>,
    /// The findings ceiling (≤ 5).
    pub max_findings: usize,
    /// Gaps and degradations worth telling the agent about (e.g. missing inventory).
    pub notes: Vec<String>,
    pub privacy: PrivacyReport,
}

// --- outbound arg builders (the only gh calls the Scout makes) ----------------------------------

/// `gh issue list` args for one watch repo + label. The public `repo` and `label` are the only
/// tokens.
pub fn upstream_issue_args(repo: &str, label: &str, limit: usize) -> Vec<String> {
    vec![
        "issue".into(),
        "list".into(),
        "--repo".into(),
        repo.into(),
        "--state".into(),
        "open".into(),
        "--label".into(),
        label.into(),
        "--json".into(),
        "number,title,labels,url".into(),
        "--limit".into(),
        limit.to_string(),
    ]
}

/// `gh api` args for a dependency's recent releases. `repo` (the dep's public source) is the only
/// token.
pub fn release_args(repo: &str) -> Vec<String> {
    vec!["api".into(), format!("repos/{repo}/releases?per_page=5")]
}

/// `gh api` args for a dependency's repository security advisories.
pub fn advisory_args(repo: &str) -> Vec<String> {
    vec![
        "api".into(),
        format!("repos/{repo}/security-advisories?per_page=5&state=published"),
    ]
}

/// `gh api` args for a tool's latest release.
pub fn tool_release_args(repo: &str) -> Vec<String> {
    vec!["api".into(), format!("repos/{repo}/releases/latest")]
}

/// Every outbound `gh` invocation the Scout would issue for this config + inventory — pure and
/// network-free. Enumerating the whole surface lets a test assert it never carries a private
/// identifier (acceptance: "outbound requests contain no private-repo identifiers"). The live
/// collectors issue exactly these.
pub fn planned_outbound(scfg: &ScoutConfig, inv: &DependencyInventory) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for repo in &scfg.watch {
        for label in UPSTREAM_LABELS {
            out.push(upstream_issue_args(repo, label, ISSUES_PER_LABEL));
        }
    }
    for dep in top_deps(inv, DEP_RADAR_CAP) {
        if !dep.repo.is_empty() {
            out.push(release_args(&dep.repo));
            out.push(advisory_args(&dep.repo));
        }
    }
    for tool in &scfg.tools {
        out.push(tool_release_args(tool));
    }
    out
}

// --- privacy guard ------------------------------------------------------------------------------

/// Tokenize an arg vector into lowercased path/word segments (splitting on anything that isn't
/// `[A-Za-z0-9_-]`, so `/`, `.`, `?`, `=` all separate). `repos/serde-rs/serde?per_page=5` →
/// `{repos, serde-rs, serde, per_page, 5}`. Whole-token matching (below) then avoids the false
/// positives a raw substring scan would hit on a short private name.
fn arg_tokens(args: &[String]) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for a in args {
        for part in a.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
            if !part.is_empty() {
                set.insert(part.to_lowercase());
            }
        }
    }
    set
}

/// Which of `private` identifiers appear in an outbound arg vector. A private entry leaks if its
/// bare repo name (last `/` segment) shows up as a whole token, or its full slug appears verbatim
/// as an arg. Used both as the runtime fail-closed guard and by the privacy test.
pub fn leaked_identifiers<'a>(args: &[String], private: &'a [String]) -> Vec<&'a str> {
    let toks = arg_tokens(args);
    let mut hits = Vec::new();
    for p in private {
        if p.trim().is_empty() {
            continue;
        }
        let full = p.to_lowercase();
        let bare = p.rsplit('/').next().unwrap_or(p).to_lowercase();
        let leaks = toks.contains(&bare) || args.iter().any(|a| a.to_lowercase() == full);
        if leaks {
            hits.push(p.as_str());
        }
    }
    hits
}

// --- why-you matching (pure, over merged-PR paths) ----------------------------------------------

/// Path segments too generic to be a "subsystem" signal — scaffolding, extensions, ubiquitous dir
/// names. Dropping them keeps the match vocabulary meaningful.
const PATH_NOISE: &[&str] = &[
    "src",
    "lib",
    "mod",
    "main",
    "tests",
    "test",
    "index",
    "crates",
    "pkg",
    "cmd",
    "bin",
    "target",
    "node_modules",
    "dist",
    "build",
    "spec",
    "docs",
    "doc",
    "rs",
    "js",
    "ts",
    "tsx",
    "jsx",
    "go",
    "py",
    "md",
    "toml",
    "json",
    "yaml",
    "yml",
    "txt",
    "lock",
    "cfg",
    "ini",
    "sh",
    "css",
    "html",
    // Ubiquitous root/manifest/meta filenames — never a meaningful subsystem signal.
    "readme",
    "license",
    "licence",
    "copying",
    "changelog",
    "contributing",
    "authors",
    "notice",
    "makefile",
    "dockerfile",
    "gitignore",
    "gitattributes",
    "editorconfig",
    "cargo",
    "package",
];

fn is_noise(tok: &str) -> bool {
    PATH_NOISE.contains(&tok)
}

/// Push the meaningful tokens of one path segment into `freq`: the file stem (before the first
/// `.`), the hyphenated stem itself (e.g. `bn-cli`), and its hyphen-split parts (`bn`, `cli`).
fn push_segment_tokens(seg: &str, freq: &mut HashMap<String, usize>) {
    let stem = seg.split('.').next().unwrap_or(seg);
    if stem.is_empty() {
        return;
    }
    let mut candidates: Vec<String> = vec![stem.to_lowercase()];
    for part in stem.split('-') {
        if !part.is_empty() {
            candidates.push(part.to_lowercase());
        }
    }
    for tok in candidates {
        let tok: String = tok
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if tok.len() >= 3 && !is_noise(&tok) {
            *freq.entry(tok).or_insert(0) += 1;
        }
    }
}

/// The subsystem vocabulary derived from your merged-PR file `paths`: the meaningful directory /
/// file-stem tokens, most-frequent first (ties broken alphabetically), capped. This is the
/// "subsystems you've recently worked in" the upstream match keys on.
pub fn subsystem_tokens(paths: &[String]) -> Vec<String> {
    let mut freq: HashMap<String, usize> = HashMap::new();
    for p in paths {
        for seg in p.split('/') {
            push_segment_tokens(seg, &mut freq);
        }
    }
    let mut toks: Vec<(String, usize)> = freq.into_iter().collect();
    toks.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    toks.truncate(40);
    toks.into_iter().map(|(t, _)| t).collect()
}

/// The subsystem tokens an issue touches: the words of its `title` + `labels` that appear in your
/// recent-subsystem `tokens`. Non-empty ⇒ the issue is worth your specific attention.
pub fn why_match(title: &str, labels: &[String], tokens: &[String]) -> Vec<String> {
    let tokset: BTreeSet<&str> = tokens.iter().map(String::as_str).collect();
    let mut hay = title.to_lowercase();
    for l in labels {
        hay.push(' ');
        hay.push_str(&l.to_lowercase());
    }
    let words: BTreeSet<String> = hay
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| w.len() >= 3)
        .map(str::to_string)
        .collect();
    let mut matched: Vec<String> = words
        .into_iter()
        .filter(|w| tokset.contains(w.as_str()))
        .collect();
    matched.sort();
    matched
}

/// Repos with a `pr_merged` event in the last `days` — the repos whose merged work bounds the
/// "recently worked in" window (and whose names are private identifiers to guard).
pub fn recent_merged_repos(events: &[Event], now: i64, days: i64) -> Vec<String> {
    let cutoff = now - days * 86_400;
    let mut set = BTreeSet::new();
    for e in events {
        if e.kind == "pr_merged" && e.ts >= cutoff {
            if let Some(r) = e.fields.get("repo").and_then(|v| v.as_str()) {
                if !r.is_empty() {
                    set.insert(r.to_string());
                }
            }
        }
    }
    set.into_iter().collect()
}

/// File paths changed in `dir`'s history since `since` (a git `--since` value), via
/// `git -C <dir> log --name-only`. Best-effort and read-only: a git failure (not a repo, git
/// absent) yields an empty list. Deduped, order-preserving.
pub fn git_changed_paths(runner: &dyn CommandRunner, dir: &Path, since: &str) -> Vec<String> {
    let dir_s = dir.to_string_lossy();
    let since_arg = format!("--since={since}");
    let out = runner.git(&[
        "-C",
        &dir_s,
        "log",
        &since_arg,
        "--no-merges",
        "--name-only",
        "--pretty=format:",
    ]);
    let Some(stdout) = out.ok_stdout() else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for line in stdout.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if seen.insert(l.to_string()) {
            paths.push(l.to_string());
        }
    }
    paths
}

/// Gather your recent merged-PR file paths across `repos`, trying each repo's likely local
/// checkout under the worktree root and the managed-repos root. Read-only and best-effort — a repo
/// with no local checkout contributes nothing. The returned paths feed [`subsystem_tokens`].
pub fn collect_recent_paths(
    runner: &dyn CommandRunner,
    cfg: &Config,
    repos: &[String],
    since: &str,
) -> Vec<String> {
    let mut all = Vec::new();
    let mut seen = BTreeSet::new();
    for repo in repos {
        for dir in candidate_repo_dirs(cfg, repo) {
            if !dir.is_dir() {
                continue;
            }
            for p in git_changed_paths(runner, &dir, since) {
                if seen.insert(p.clone()) {
                    all.push(p);
                }
            }
        }
    }
    all
}

/// Likely local-checkout directories for `repo`: the worktree root's repo dir (and its `main`
/// worktree) plus the managed-repos root. Best-effort; callers skip non-directories.
fn candidate_repo_dirs(cfg: &Config, repo: &str) -> Vec<PathBuf> {
    let base = repo.rsplit('/').next().unwrap_or(repo);
    vec![
        cfg.worktree_dir.join(base),
        cfg.worktree_dir.join(base).join("main"),
        crate::resolver::managed_repos_dir().join(base),
    ]
}

// --- dedup against recent Scout cards -----------------------------------------------------------

/// The dedup key for a candidate: its normalized link when present (host + path, lowercased,
/// trailing slash trimmed), else a normalized title. Two findings with the same link — or the same
/// title when linkless — are "the same finding".
pub fn dedup_key_for(link: &str, title: &str) -> String {
    let link = link.trim();
    if !link.is_empty() {
        let norm = link
            .trim_end_matches('/')
            .to_lowercase()
            .replace("https://", "")
            .replace("http://", "");
        return format!("l:{norm}");
    }
    let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("t:{}", title.to_lowercase())
}

/// The dedup keys of every item on a Scout card filed within the last `weeks` weeks of `today`.
/// A candidate whose [`dedup_key_for`] is in this set has already been surfaced and must be
/// dropped — the "no re-surfacing the same release" guarantee.
pub fn recent_scout_keys(cfg: &Config, today: &str, weeks: i64) -> BTreeSet<String> {
    let horizon = weeks * 7;
    let mut keys = BTreeSet::new();
    for card in crate::staff::list_cards(cfg) {
        if card.agent != "scout" {
            continue;
        }
        match crate::util::days_since(&card.date, today) {
            Some(d) if (0..=horizon).contains(&d) => {}
            _ => continue,
        }
        if let Some(full) = crate::staff::read_card(&card.path) {
            for item in &full.items {
                keys.insert(dedup_key_for(&item.link, &item.title));
            }
        }
    }
    keys
}

// --- caching ------------------------------------------------------------------------------------

fn cache_dir(cfg: &Config) -> PathBuf {
    cfg.state_dir().join("scout").join("cache")
}

fn cache_path(cfg: &Config, name: &str, date: &str) -> PathBuf {
    cache_dir(cfg).join(format!("{name}-{date}.json"))
}

/// Read today's cached section for `name`, if present (a same-day re-run reuses it — no network).
fn cache_read_today<T: for<'de> Deserialize<'de>>(
    cfg: &Config,
    name: &str,
    today: &str,
) -> Option<Section<T>> {
    let text = std::fs::read_to_string(cache_path(cfg, name, today)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The newest cached section for `name` on any prior date, with its date — the stale fallback when
/// a live fetch fails.
fn cache_read_newest<T: for<'de> Deserialize<'de>>(
    cfg: &Config,
    name: &str,
) -> Option<(String, Section<T>)> {
    let prefix = format!("{name}-");
    let mut newest: Option<(String, PathBuf)> = None;
    for entry in std::fs::read_dir(cache_dir(cfg)).ok()?.flatten() {
        let fname = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = fname.strip_prefix(&prefix) else {
            continue;
        };
        let Some(date) = rest.strip_suffix(".json") else {
            continue;
        };
        if newest.as_ref().is_none_or(|(d, _)| date > d.as_str()) {
            newest = Some((date.to_string(), entry.path()));
        }
    }
    let (date, path) = newest?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok().map(|s| (date, s))
}

fn cache_write<T: Serialize>(cfg: &Config, name: &str, date: &str, section: &Section<T>) {
    let dir = cache_dir(cfg);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(section) {
        let _ = std::fs::write(cache_path(cfg, name, date), text);
    }
}

/// Run a collector with per-day caching. Same-day cache hit → no network. Else fetch live; a
/// non-degraded result is cached, a degraded one falls back to the newest prior cache (annotated)
/// before giving up. This is the isolation guarantee: one section's network failure never sinks
/// the others, and never loses the last good data.
pub fn with_cache<T, F>(cfg: &Config, name: &str, today: &str, fetch: F) -> Section<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
    F: FnOnce() -> Section<T>,
{
    if let Some(cached) = cache_read_today::<T>(cfg, name, today) {
        return cached;
    }
    let live = fetch();
    if !live.degraded {
        cache_write(cfg, name, today, &live);
        return live;
    }
    if let Some((date, mut stale)) = cache_read_newest::<T>(cfg, name) {
        stale.notes.push(format!(
            "live fetch failed ({}) — served last good cache from {date}",
            live.error.as_deref().unwrap_or("network error")
        ));
        return stale;
    }
    live
}

// --- collectors ---------------------------------------------------------------------------------

fn as_strs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// Cap and clean an untrusted upstream string: strip control chars, collapse whitespace, bound the
/// length. The framework re-sanitizes card text at validate time; this keeps the *packet* tidy and
/// bounded regardless.
fn clean(s: &str, max: usize) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if out.chars().count() > max {
        out = out.chars().take(max).collect::<String>() + "…";
    }
    out
}

#[derive(Deserialize)]
struct GhLabel {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct GhIssue {
    #[serde(default)]
    number: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
}

#[derive(Deserialize)]
struct GhRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

#[derive(Deserialize)]
struct GhAdvisory {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    severity: String,
}

/// The leading semver major of a version/tag like `v2.3.1` / `1.4` → `Some(2)` / `Some(1)`.
fn major_of(v: &str) -> Option<u64> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    v.split(['.', '-', '+']).next()?.parse::<u64>().ok()
}

/// Classify a release against the current version: a higher major is `breaking`, a body/name
/// mentioning deprecation is `deprecation`, else a plain `release`.
fn classify_release(current: &str, rel: &GhRelease) -> &'static str {
    let text = format!("{} {}", rel.name, rel.body).to_lowercase();
    if text.contains("deprecat") {
        return "deprecation";
    }
    if let (Some(cur), Some(new)) = (major_of(current), major_of(&rel.tag_name)) {
        if new > cur {
            return "breaking";
        }
    }
    "release"
}

/// The upstream-issue collector: for each watch repo × label, list open issues and keep only those
/// that match a recent subsystem (`tokens`). Deduped by link across labels/repos. Degrades to
/// `degraded` only if *every* query failed; a partial failure is a note. Every outbound call is
/// leak-guarded against `private`.
pub fn collect_upstream(
    runner: &dyn CommandRunner,
    watch: &[String],
    tokens: &[String],
    private: &[String],
    limit: usize,
) -> Section<UpstreamItem> {
    let mut sec = Section::default();
    if watch.is_empty() {
        sec.notes
            .push("no [scout] watch list configured — upstream radar idle".into());
        return sec;
    }
    let mut seen_links = BTreeSet::new();
    let (mut any_ok, mut any_fail) = (false, false);
    for repo in watch {
        for label in UPSTREAM_LABELS {
            let args = upstream_issue_args(repo, label, limit);
            let leaks = leaked_identifiers(&args, private);
            if !leaks.is_empty() {
                sec.notes.push(format!(
                    "skipped {repo} [{label}] — outbound would leak: {}",
                    leaks.join(", ")
                ));
                continue;
            }
            let out = runner.gh(&as_strs(&args));
            let Some(js) = out.ok_stdout() else {
                any_fail = true;
                continue;
            };
            any_ok = true;
            match serde_json::from_str::<Vec<GhIssue>>(&js) {
                Ok(issues) => {
                    for is in issues {
                        let labels: Vec<String> = is
                            .labels
                            .iter()
                            .map(|l| l.name.clone())
                            .filter(|s| !s.is_empty())
                            .collect();
                        let matched = why_match(&is.title, &labels, tokens);
                        if matched.is_empty() {
                            continue;
                        }
                        if !seen_links.insert(is.url.clone()) {
                            continue;
                        }
                        sec.items.push(UpstreamItem {
                            repo: repo.clone(),
                            number: is.number,
                            title: clean(&is.title, 200),
                            labels,
                            link: is.url,
                            matched,
                        });
                    }
                }
                Err(e) => sec
                    .notes
                    .push(format!("{repo} [{label}]: unparseable issue json: {e}")),
            }
        }
    }
    if any_fail && !any_ok {
        sec.degraded = true;
        sec.error = Some("all upstream issue queries failed (network/auth)".into());
    } else if any_fail {
        sec.notes
            .push("some upstream queries failed — section is partial".into());
    }
    sec
}

/// The dependency-radar collector: enrich the top inventory deps with their latest release
/// (classified vs. the in-use version) and any published advisory. Degrades the whole section when
/// the inventory is absent (the Sentinel hasn't built it) — a documented, non-blocking gap.
pub fn collect_dependency_radar(
    runner: &dyn CommandRunner,
    inv: Option<&DependencyInventory>,
    private: &[String],
) -> Section<DependencyItem> {
    let mut sec = Section::default();
    let Some(inv) = inv else {
        sec.degraded = true;
        sec.error = Some(
            "dependency inventory not found — Sentinel's shared cache (#262 depends-on) not built yet"
                .into(),
        );
        return sec;
    };
    let deps = top_deps(inv, DEP_RADAR_CAP);
    if deps.is_empty() {
        sec.notes.push("dependency inventory is empty".into());
        return sec;
    }
    let (mut any_ok, mut any_fail) = (false, false);
    for dep in deps {
        if dep.repo.is_empty() {
            sec.notes.push(format!(
                "{}: no source repo in inventory — skipped",
                dep.name
            ));
            continue;
        }
        // Releases.
        let rel_args = release_args(&dep.repo);
        let leaks = leaked_identifiers(&rel_args, private);
        if !leaks.is_empty() {
            sec.notes.push(format!(
                "skipped {} — would leak: {}",
                dep.repo,
                leaks.join(", ")
            ));
            continue;
        }
        match runner.gh(&as_strs(&rel_args)).ok_stdout() {
            Some(js) => {
                any_ok = true;
                if let Ok(rels) = serde_json::from_str::<Vec<GhRelease>>(&js) {
                    if let Some(rel) = rels.into_iter().find(|r| !r.draft && !r.prerelease) {
                        if dep.current.is_empty()
                            || rel.tag_name.trim_start_matches(['v', 'V'])
                                != dep.current.trim_start_matches(['v', 'V'])
                        {
                            let kind = classify_release(&dep.current, &rel);
                            sec.items.push(DependencyItem {
                                name: dep.name.clone(),
                                repo: dep.repo.clone(),
                                current: dep.current.clone(),
                                latest: rel.tag_name.clone(),
                                kind: kind.into(),
                                summary: clean(&rel.name, 160),
                                link: rel.html_url,
                                used_by: dep.used_by.clone(),
                            });
                        }
                    }
                }
            }
            None => any_fail = true,
        }
        // Advisories.
        let adv_args = advisory_args(&dep.repo);
        if leaked_identifiers(&adv_args, private).is_empty() {
            if let Some(js) = runner.gh(&as_strs(&adv_args)).ok_stdout() {
                if let Ok(advs) = serde_json::from_str::<Vec<GhAdvisory>>(&js) {
                    for adv in advs.into_iter().take(2) {
                        sec.items.push(DependencyItem {
                            name: dep.name.clone(),
                            repo: dep.repo.clone(),
                            current: dep.current.clone(),
                            latest: String::new(),
                            kind: "advisory".into(),
                            summary: clean(&format!("[{}] {}", adv.severity, adv.summary), 160),
                            link: adv.html_url,
                            used_by: dep.used_by.clone(),
                        });
                    }
                }
            }
        }
    }
    if any_fail && !any_ok {
        sec.degraded = true;
        sec.error = Some("all dependency release queries failed (network/auth)".into());
    } else if any_fail {
        sec.notes
            .push("some dependency queries failed — section is partial".into());
    }
    sec
}

/// The tooling-delta collector: the latest release of each tool in your stack. Degrades only when
/// every lookup failed.
pub fn collect_tooling_delta(
    runner: &dyn CommandRunner,
    tools: &[String],
    private: &[String],
) -> Section<ToolingItem> {
    let mut sec = Section::default();
    if tools.is_empty() {
        sec.notes
            .push("no [scout] tools configured — tooling radar idle".into());
        return sec;
    }
    let (mut any_ok, mut any_fail) = (false, false);
    for tool in tools {
        let args = tool_release_args(tool);
        let leaks = leaked_identifiers(&args, private);
        if !leaks.is_empty() {
            sec.notes
                .push(format!("skipped {tool} — would leak: {}", leaks.join(", ")));
            continue;
        }
        match runner.gh(&as_strs(&args)).ok_stdout() {
            Some(js) => {
                any_ok = true;
                if let Ok(rel) = serde_json::from_str::<GhRelease>(&js) {
                    sec.items.push(ToolingItem {
                        tool: tool.clone(),
                        latest: rel.tag_name,
                        summary: clean(&rel.name, 160),
                        link: rel.html_url,
                    });
                }
            }
            None => any_fail = true,
        }
    }
    if any_fail && !any_ok {
        sec.degraded = true;
        sec.error = Some("all tooling release queries failed (network/auth)".into());
    } else if any_fail {
        sec.notes
            .push("some tooling queries failed — section is partial".into());
    }
    sec
}

// --- dedup application + packet assembly --------------------------------------------------------

/// Drop candidates whose dedup key was surfaced within the window; returns how many were dropped.
fn dedup_upstream(sec: &mut Section<UpstreamItem>, recent: &BTreeSet<String>) -> usize {
    let before = sec.items.len();
    sec.items
        .retain(|i| !recent.contains(&dedup_key_for(&i.link, &i.title)));
    before - sec.items.len()
}

fn dedup_dependency(sec: &mut Section<DependencyItem>, recent: &BTreeSet<String>) -> usize {
    let before = sec.items.len();
    sec.items.retain(|i| {
        let title = format!("{} {} {}", i.name, i.kind, i.latest);
        !recent.contains(&dedup_key_for(&i.link, &title))
    });
    before - sec.items.len()
}

fn dedup_tooling(sec: &mut Section<ToolingItem>, recent: &BTreeSet<String>) -> usize {
    let before = sec.items.len();
    sec.items.retain(|i| {
        let title = format!("{} {}", i.tool, i.latest);
        !recent.contains(&dedup_key_for(&i.link, &title))
    });
    before - sec.items.len()
}

/// Build the full Scout packet: run each collector (cached, degradable in isolation), compute the
/// why-you vocabulary from your recent merged-PR paths, dedup every section against the last
/// [`DEDUP_WEEKS`] of Scout cards, and attach the privacy audit. `now` is epoch seconds (for the
/// merged-PR window); `today` is `YYYY-MM-DD`.
pub fn gather_packet(
    cfg: &Config,
    runner: &dyn CommandRunner,
    scfg: &ScoutConfig,
    now: i64,
    today: &str,
) -> ScoutPacket {
    let mut notes = Vec::new();

    let inv = load_inventory(cfg);
    if inv.is_none() {
        notes.push(
            "dependency inventory absent — Sentinel's shared cache (#262 depends-on) not built; \
             dependency radar degraded"
                .into(),
        );
    }

    // Private identifiers to guard: the repos you recently merged in + every `used_by` repo in the
    // inventory. None of these may ever appear in an outbound request.
    let events = crate::events::read_all();
    let merged_repos = recent_merged_repos(&events, now, RECENT_WORK_DAYS);
    let mut private: BTreeSet<String> = merged_repos.iter().cloned().collect();
    if let Some(inv) = &inv {
        for dep in &inv.deps {
            for u in &dep.used_by {
                private.insert(u.clone());
            }
        }
    }
    let private: Vec<String> = private.into_iter().collect();

    // Why-you vocabulary from local merged-PR paths.
    let since =
        crate::util::add_days(today, -RECENT_WORK_DAYS).unwrap_or_else(|| today.to_string());
    let paths = collect_recent_paths(runner, cfg, &merged_repos, &since);
    let subsystems = subsystem_tokens(&paths);

    // Dedup window.
    let recent = recent_scout_keys(cfg, today, DEDUP_WEEKS);

    // Collectors, each cached + degradable in isolation.
    let mut dependency_radar = with_cache(cfg, "dependency-radar", today, || {
        collect_dependency_radar(runner, inv.as_ref(), &private)
    });
    let mut upstream_matches = with_cache(cfg, "upstream", today, || {
        collect_upstream(runner, &scfg.watch, &subsystems, &private, ISSUES_PER_LABEL)
    });
    let mut tooling_delta = with_cache(cfg, "tooling-delta", today, || {
        collect_tooling_delta(runner, &scfg.tools, &private)
    });

    let dropped = dedup_dependency(&mut dependency_radar, &recent)
        + dedup_upstream(&mut upstream_matches, &recent)
        + dedup_tooling(&mut tooling_delta, &recent);
    if dropped > 0 {
        notes.push(format!(
            "deduped {dropped} candidate(s) already surfaced in the last {DEDUP_WEEKS} weeks"
        ));
    }

    // Privacy audit — enumerate the planned outbound surface and confirm no private leak.
    let empty_inv = DependencyInventory::default();
    let outbound = planned_outbound(scfg, inv.as_ref().unwrap_or(&empty_inv));
    let mut outbound_tokens = BTreeSet::new();
    let mut leaks = BTreeSet::new();
    for args in &outbound {
        for t in arg_tokens(args) {
            outbound_tokens.insert(t);
        }
        for l in leaked_identifiers(args, &private) {
            leaks.insert(l.to_string());
        }
    }
    let privacy = PrivacyReport {
        outbound_tokens: outbound_tokens.into_iter().collect(),
        private_guarded: private,
        leaks: leaks.into_iter().collect(),
    };

    ScoutPacket {
        agent: "scout".into(),
        date: today.into(),
        machine: cfg.machine().to_string(),
        watchlist: scfg.watch.clone(),
        dependency_radar,
        upstream_matches,
        tooling_delta,
        recent_subsystems: subsystems,
        deduped_recent: recent.into_iter().collect(),
        max_findings: scfg.max_findings.clamp(1, DEFAULT_MAX_FINDINGS),
        notes,
        privacy,
    }
}

/// `bn staff gather scout` entry point: build the packet from real config + a live runner and
/// serialize it. The deterministic input the Scout agent is prompted with.
pub fn gather_packet_json(cfg: &Config, runner: &dyn CommandRunner, today: &str) -> String {
    let packet = gather_packet(
        cfg,
        runner,
        &cfg.scout,
        crate::util::now_epoch_secs(),
        today,
    );
    serde_json::to_string(&packet).unwrap_or_else(|_| "{}".into())
}


