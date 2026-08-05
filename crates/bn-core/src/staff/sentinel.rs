//! The Debt Sentinel (#259) — the 22:30 estate patrol.
//!
//! A nightly, deterministic sweep for *estate rot* — aging TODO/FIXME markers, stale
//! dependencies, flaky verify history, worktree/branch cruft, and worsening note-hygiene. It runs
//! 30 minutes before the night-shift window opens so its best picks become **night-shift fuel**:
//! the Sentinel finds it at 22:30, the night shift (#250) fixes it overnight, you review a draft
//! PR at breakfast.
//!
//! The split mirrors the staff framework's "gather = deterministic, agent = triage" contract, but
//! the parts the acceptance criteria pin down are all deterministic and testable and live here:
//!
//! * **Scanners** ([`run_scanners`]) are pure functions over pre-collected inputs
//!   ([`PatrolInputs`], filled by the CLI's offline collectors). Each degrades independently — a
//!   scanner with no input simply yields nothing; the dep scanner in particular reads a *daily
//!   cache* so a missing/absent registry snapshot degrades only that scanner.
//! * **Landing** ([`patrol`]) ingests findings through the existing keyed attention path with
//!   `--replace` semantics, so rot that disappears auto-resolves on the next patrol. Its picks are
//!   promoted (`auto` attention items tagged `staff-sentinel`) — capped, allowlisted, and deduped
//!   against what's already queued — so the night shift can pull them under its own caps.
//! * **Doctor dedup** — note-hygiene overlaps what `bn doctor` already reports, so those findings
//!   carry a literal `via=doctor` key and surface *only when worsening* vs the previous patrol.
//! * **Trend** — the card headline compares the current surfaced set to the previous patrol's
//!   snapshot ("estate improving: 3 resolved, 1 new").
//!
//! An LLM triage overlay (rank/pick/prose beyond the deterministic defaults here) layers on top
//! via the standard staff `gather → dispatch → route` pipeline once #266 wires staff dispatch; the
//! deterministic patrol stands alone and is what actually lands findings nightly.

use super::{write_card, BriefingCard, CardItem};
use crate::attention::{self, IngestItem, NewAttn};
use crate::resolver::Config;
use crate::runner::CommandRunner;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// The source name every Sentinel-raised attention item carries, so `--replace` reconciles the
/// Sentinel's own keyed items and nothing else.
pub const SOURCE: &str = "staff-sentinel";

/// Default age (days) a TODO/FIXME marker must reach before the Sentinel flags it.
pub const DEFAULT_TODO_AGE_DAYS: i64 = 30;

/// A verify label is "flaky" once it has failed at least this many times in the window *and* also
/// passed at least once (an inconsistent result, not a hard/consistent break).
pub const FLAKE_MIN_FAILS: usize = 2;

/// A blocker older than this many days is note-hygiene rot worth escalating (matches the doctor's
/// spirit; the Sentinel only escalates *worsening* ones).
pub const BLOCKED_STALE_DAYS: i64 = 5;

/// The most night-shift promotions a single patrol may raise — the flood guard.
pub const MAX_PROMOTIONS: usize = 2;

// --- finding model ------------------------------------------------------------------------------

/// Which scanner produced a finding. Serialized lowercase for the packet/card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scanner {
    TodoAge,
    DepStale,
    VerifyFlake,
    BranchHygiene,
    NoteHygiene,
}

impl Scanner {
    /// Short stable tag used as the leading segment of a finding key.
    fn tag(self) -> &'static str {
        match self {
            Scanner::TodoAge => "todo",
            Scanner::DepStale => "dep",
            Scanner::VerifyFlake => "flake",
            Scanner::BranchHygiene => "branch",
            Scanner::NoteHygiene => "note",
        }
    }

    /// Whether a finding from this scanner is something the night shift can safely attempt to fix
    /// autonomously (code work), vs. hygiene a human should action. Only these are promotion-eligible.
    fn fixable(self) -> bool {
        matches!(
            self,
            Scanner::TodoAge | Scanner::DepStale | Scanner::VerifyFlake
        )
    }
}

/// Severity a finding lands at — maps 1:1 to the attention list's `info`/`warn`/`urgent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Urgent,
}

impl Severity {
    /// The attention-list string.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Urgent => "urgent",
        }
    }

    fn weight(self) -> i64 {
        match self {
            Severity::Info => 10,
            Severity::Warn => 100,
            Severity::Urgent => 1000,
        }
    }
}

/// One deterministic finding from a scanner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub scanner: Scanner,
    /// Stable natural key (unique within [`SOURCE`]). Note-hygiene keys carry a literal
    /// `via=doctor` segment so they dedup against the doctor's own reporting.
    pub key: String,
    pub title: String,
    pub severity: Severity,
    /// Bare repo the finding is about, when it maps to one (drives the allowlist gate + promotion).
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// A monotonically-worse-is-larger metric (age days, majors-behind, fail count, blocked days)
    /// used to decide escalation vs. a prior patrol and to break ranking ties.
    pub metric: i64,
    /// This finding overlaps something `bn doctor` already surfaces; it is kept quiet unless it
    /// worsened since the last patrol.
    #[serde(default)]
    pub via_doctor: bool,
}

impl Finding {
    /// Blast-radius × effort proxy: severity dominates, metric breaks ties. Higher = queue sooner.
    fn rank(&self) -> i64 {
        self.severity.weight() * 1000 + self.metric.clamp(0, 999)
    }

    fn ingest_item(&self) -> IngestItem {
        IngestItem {
            title: self.title.clone(),
            severity: Some(self.severity.as_str().to_string()),
            source: Some(SOURCE.to_string()),
            key: Some(self.key.clone()),
            link: self.link.clone(),
            repo: self.repo.clone(),
            branch: None,
            detail: self.detail.clone(),
        }
    }
}

// --- scanner inputs -----------------------------------------------------------------------------

/// A TODO/FIXME marker located by `git blame`, with the age of its introducing commit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TodoMarker {
    pub repo: String,
    pub file: String,
    pub line: u32,
    pub text: String,
    pub age_days: i64,
    /// The marker sits in a "hot" file (many recent commits) — ranked and shown first.
    #[serde(default)]
    pub hot: bool,
}

/// A dependency's staleness, from the daily registry cache vs. the lockfile.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DepStatus {
    pub repo: String,
    pub name: String,
    pub current: String,
    pub latest: String,
    pub majors_behind: u32,
    #[serde(default)]
    pub vuln: bool,
}

/// One verify-gate outcome for a labelled job/test in the trailing week.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyRun {
    pub repo: String,
    /// The job/test label (e.g. a cron job name, or a test id once the flake ledger lands).
    pub label: String,
    pub passed: bool,
}

/// A worktree/branch the hygiene scanner considers cruft.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorktreeState {
    pub repo: String,
    pub branch: String,
    /// Upstream is `[gone]` (the PR merged/branch deleted) — reclaimable.
    #[serde(default)]
    pub gone: bool,
    /// A stale non-main worktree `wt cleanup` would refuse (dirty / unmerged), so it lingers.
    #[serde(default)]
    pub cleanup_refused: bool,
    #[serde(default)]
    pub reason: String,
}

/// A branch note's hygiene state (overlaps the doctor; only worsening deltas escalate).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoteState {
    pub repo: String,
    pub branch: String,
    /// Days the oldest open blocker has been open (0 = none / not blocked).
    pub blocked_days: i64,
    /// An active note with no goal set.
    #[serde(default)]
    pub goal_less: bool,
}

/// Everything the scanners see, collected offline by the CLI. Each field is independent: an empty
/// vec (or `None` for `deps`) means that scanner degraded and simply yields nothing.
#[derive(Debug, Clone, Default)]
pub struct PatrolInputs {
    pub markers: Vec<TodoMarker>,
    /// `None` ⇒ the dep-registry daily cache was absent/unreadable, so the dep scanner degrades.
    pub deps: Option<Vec<DepStatus>>,
    pub verify_runs: Vec<VerifyRun>,
    pub worktrees: Vec<WorktreeState>,
    pub notes: Vec<NoteState>,
}

/// Tunables for one patrol.
#[derive(Debug, Clone)]
pub struct PatrolConfig {
    /// Bare repo names the patrol may scan and promote from. Empty ⇒ the patrol is a no-op (the
    /// opt-in boundary, mirroring the night allowlist).
    pub repos: Vec<String>,
    pub todo_age_days: i64,
}

impl Default for PatrolConfig {
    fn default() -> Self {
        PatrolConfig {
            repos: Vec::new(),
            todo_age_days: DEFAULT_TODO_AGE_DAYS,
        }
    }
}

impl PatrolConfig {
    fn allows(&self, repo: &str) -> bool {
        self.repos.iter().any(|r| r.eq_ignore_ascii_case(repo))
    }
}

// --- scanners (pure) ----------------------------------------------------------------------------

/// TODO/FIXME markers older than `min_age_days`, hottest files first (via [`Finding::rank`]).
pub fn scan_todo_age(markers: &[TodoMarker], min_age_days: i64) -> Vec<Finding> {
    markers
        .iter()
        .filter(|m| m.age_days >= min_age_days)
        .map(|m| Finding {
            scanner: Scanner::TodoAge,
            key: format!("todo:{}:{}:{}", m.repo, m.file, m.line),
            title: format!(
                "TODO {}d old in {} — {}",
                m.age_days,
                m.file,
                truncate(&m.text, 80)
            ),
            severity: if m.hot {
                Severity::Warn
            } else {
                Severity::Info
            },
            repo: non_empty(&m.repo),
            detail: Some(format!("{}:{} — {}", m.file, m.line, m.text)),
            link: None,
            metric: m.age_days,
            via_doctor: false,
        })
        .collect()
}

/// Dependencies majors behind, or carrying a known vuln (urgent). `None` deps ⇒ scanner degraded.
pub fn scan_dep_stale(deps: Option<&Vec<DepStatus>>) -> Vec<Finding> {
    let Some(deps) = deps else {
        return Vec::new();
    };
    deps.iter()
        .filter(|d| d.vuln || d.majors_behind >= 1)
        .map(|d| {
            let severity = if d.vuln {
                Severity::Urgent
            } else {
                Severity::Warn
            };
            let title = if d.vuln {
                format!(
                    "{} {} has a known vuln (latest {})",
                    d.name, d.current, d.latest
                )
            } else {
                format!(
                    "{} is {} major{} behind ({} → {})",
                    d.name,
                    d.majors_behind,
                    if d.majors_behind == 1 { "" } else { "s" },
                    d.current,
                    d.latest
                )
            };
            Finding {
                scanner: Scanner::DepStale,
                key: format!("dep:{}:{}", d.repo, d.name),
                title,
                severity,
                repo: non_empty(&d.repo),
                detail: Some(format!("{} {} → {}", d.name, d.current, d.latest)),
                link: None,
                // Vulns outrank version drift regardless of how many majors behind.
                metric: if d.vuln { 999 } else { d.majors_behind as i64 },
                via_doctor: false,
            }
        })
        .collect()
}

/// Verify labels that flaked (≥ [`FLAKE_MIN_FAILS`] fails *and* ≥ 1 pass) in the window.
pub fn scan_verify_flake(runs: &[VerifyRun]) -> Vec<Finding> {
    // (repo,label) → (fails, passes)
    let mut tally: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();
    for r in runs {
        let e = tally.entry((r.repo.clone(), r.label.clone())).or_default();
        if r.passed {
            e.1 += 1;
        } else {
            e.0 += 1;
        }
    }
    tally
        .into_iter()
        .filter(|(_, (fails, passes))| *fails >= FLAKE_MIN_FAILS && *passes >= 1)
        .map(|((repo, label), (fails, passes))| Finding {
            scanner: Scanner::VerifyFlake,
            key: format!("flake:{repo}:{label}"),
            title: format!("{label} flaked {fails}× this week ({passes} pass / {fails} fail)"),
            severity: Severity::Warn,
            repo: non_empty(&repo),
            detail: Some(format!(
                "verify label '{label}' produced inconsistent results ({passes} passing, {fails} failing runs)"
            )),
            link: None,
            metric: fails as i64,
            via_doctor: false,
        })
        .collect()
}

/// Stale worktrees `wt cleanup` refuses and `[gone]` branches — reclaimable cruft.
pub fn scan_branch_hygiene(worktrees: &[WorktreeState]) -> Vec<Finding> {
    worktrees
        .iter()
        .filter(|w| w.gone || w.cleanup_refused)
        .map(|w| {
            let (title, severity) = if w.gone {
                (
                    format!("{}/{} is [gone] — reclaimable", w.repo, w.branch),
                    Severity::Warn,
                )
            } else {
                (
                    format!(
                        "{}/{} lingers — wt cleanup refuses ({})",
                        w.repo,
                        w.branch,
                        if w.reason.is_empty() {
                            "stale"
                        } else {
                            &w.reason
                        }
                    ),
                    Severity::Info,
                )
            };
            Finding {
                scanner: Scanner::BranchHygiene,
                key: format!("branch:{}:{}", w.repo, w.branch),
                title,
                severity,
                repo: non_empty(&w.repo),
                detail: (!w.reason.is_empty()).then(|| w.reason.clone()),
                link: None,
                metric: 1,
                via_doctor: false,
            }
        })
        .collect()
}

/// Note-hygiene deltas: blockers older than [`BLOCKED_STALE_DAYS`] and goal-less active notes.
/// These overlap `bn doctor`, so every finding carries a `via=doctor` key and is later kept quiet
/// unless it worsened (see [`patrol`]).
pub fn scan_note_hygiene(notes: &[NoteState]) -> Vec<Finding> {
    let mut out = Vec::new();
    for n in notes {
        if n.blocked_days > BLOCKED_STALE_DAYS {
            out.push(Finding {
                scanner: Scanner::NoteHygiene,
                key: format!("note:via=doctor:{}/{}:blocked", n.repo, n.branch),
                title: format!(
                    "{}/{} blocked {}d — worsening",
                    n.repo, n.branch, n.blocked_days
                ),
                severity: Severity::Warn,
                repo: non_empty(&n.repo),
                detail: Some(format!("oldest open blocker is {}d old", n.blocked_days)),
                link: None,
                metric: n.blocked_days,
                via_doctor: true,
            });
        }
        if n.goal_less {
            out.push(Finding {
                scanner: Scanner::NoteHygiene,
                key: format!("note:via=doctor:{}/{}:no_goal", n.repo, n.branch),
                title: format!("{}/{} active but goal-less", n.repo, n.branch),
                severity: Severity::Info,
                repo: non_empty(&n.repo),
                detail: None,
                link: None,
                metric: 1,
                via_doctor: true,
            });
        }
    }
    out
}

/// Run every scanner over `inputs`, keeping only findings from allowlisted repos (or with no repo).
/// The result is unfiltered by the doctor-dedup / worsening rule — [`patrol`] applies that.
pub fn run_scanners(inputs: &PatrolInputs, cfg: &PatrolConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(scan_todo_age(&inputs.markers, cfg.todo_age_days));
    findings.extend(scan_dep_stale(inputs.deps.as_ref()));
    findings.extend(scan_verify_flake(&inputs.verify_runs));
    findings.extend(scan_branch_hygiene(&inputs.worktrees));
    findings.extend(scan_note_hygiene(&inputs.notes));
    findings.retain(|f| match &f.repo {
        Some(r) => cfg.allows(r),
        None => true,
    });
    findings
}

// --- previous-patrol snapshot -------------------------------------------------------------------

/// One remembered finding from the previous patrol — enough to compute worsening + trend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SnapFinding {
    key: String,
    metric: i64,
    /// Whether it was actually surfaced (vs. a quiet doctor-overlap baseline).
    surfaced: bool,
}

/// The previous patrol's baseline, persisted machine-locally so trend + worsening survive across
/// runs. Absent on the very first patrol.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Snapshot {
    #[serde(default)]
    date: String,
    #[serde(default)]
    findings: Vec<SnapFinding>,
}

impl Snapshot {
    fn baseline(&self, key: &str) -> Option<i64> {
        self.findings
            .iter()
            .find(|f| f.key == key)
            .map(|f| f.metric)
    }

    fn surfaced_keys(&self) -> std::collections::BTreeSet<&str> {
        self.findings
            .iter()
            .filter(|f| f.surfaced)
            .map(|f| f.key.as_str())
            .collect()
    }
}

fn snapshot_path() -> PathBuf {
    crate::resolver::state_dir()
        .join("sentinel")
        .join("patrol.json")
}

fn load_snapshot() -> Snapshot {
    std::fs::read_to_string(snapshot_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_snapshot(snap: &Snapshot) -> std::io::Result<()> {
    let path = snapshot_path();
    crate::store::save_json_atomic(&path, snap)
}

// --- trend --------------------------------------------------------------------------------------

/// How the estate moved since the previous patrol — the card's headline sentence.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Trend {
    /// Surfaced last patrol, gone now (rot resolved).
    pub resolved: usize,
    /// Surfaced now, absent last patrol.
    pub new: usize,
    /// Surfaced both patrols.
    pub carried: usize,
    /// Total surfaced this patrol.
    pub total: usize,
}

impl Trend {
    /// The one-line headline, e.g. `estate improving: 3 resolved, 1 new` / `estate clean`.
    pub fn sentence(&self) -> String {
        if self.total == 0 && self.resolved == 0 {
            return "estate clean — no debt findings".to_string();
        }
        let direction = if self.resolved > self.new {
            "estate improving"
        } else if self.new > self.resolved {
            "estate worsening"
        } else {
            "estate steady"
        };
        format!(
            "{direction}: {} finding{}, {} resolved, {} new",
            self.total,
            plural(self.total),
            self.resolved,
            self.new
        )
    }
}

fn compute_trend(prev: &Snapshot, surfaced: &[Finding]) -> Trend {
    let prev_keys = prev.surfaced_keys();
    let now_keys: std::collections::BTreeSet<&str> =
        surfaced.iter().map(|f| f.key.as_str()).collect();
    let new = now_keys.difference(&prev_keys).count();
    let carried = now_keys.intersection(&prev_keys).count();
    let resolved = prev_keys.difference(&now_keys).count();
    Trend {
        resolved,
        new,
        carried,
        total: surfaced.len(),
    }
}

// --- doctor dedup / worsening -------------------------------------------------------------------

/// Decide which candidates actually surface this patrol. Non-doctor findings always surface;
/// `via=doctor` findings surface only when they strictly worsened vs. the recorded baseline (a
/// first sighting records a quiet baseline and stays silent, so the Sentinel never just re-prints
/// what `bn doctor` already shows).
fn surface(candidates: Vec<Finding>, prev: &Snapshot) -> Vec<Finding> {
    candidates
        .into_iter()
        .filter(|f| {
            if !f.via_doctor {
                return true;
            }
            matches!(prev.baseline(&f.key), Some(base) if f.metric > base)
        })
        .collect()
}

// --- promotion ----------------------------------------------------------------------------------

/// A night-shift promotion the patrol raised (or would raise).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Promotion {
    pub key: String,
    pub repo: String,
    pub branch: String,
    pub title: String,
    /// Whether the raise was a no-op because an identical promotion was already queued.
    pub already_queued: bool,
}

/// Select up to [`MAX_PROMOTIONS`] promotable findings: fixable scanners, `warn`+ severity, from an
/// allowlisted repo, ranked by blast-radius × effort, and not already queued. Pure — [`patrol`]
/// performs the actual raise.
fn select_promotions<'a>(
    surfaced: &'a [Finding],
    cfg: &PatrolConfig,
    queued_keys: &std::collections::BTreeSet<String>,
) -> Vec<&'a Finding> {
    let mut candidates: Vec<&Finding> = surfaced
        .iter()
        .filter(|f| f.scanner.fixable())
        .filter(|f| f.severity.weight() >= Severity::Warn.weight())
        .filter(|f| f.repo.as_deref().is_some_and(|r| cfg.allows(r)))
        .filter(|f| !queued_keys.contains(&promotion_key(&f.key)))
        .collect();
    // Highest rank first; stable key tie-break keeps the pick deterministic across patrols.
    candidates.sort_by(|a, b| b.rank().cmp(&a.rank()).then_with(|| a.key.cmp(&b.key)));
    candidates.truncate(MAX_PROMOTIONS);
    candidates
}

fn promotion_key(finding_key: &str) -> String {
    format!("attn:{SOURCE}:promote:{finding_key}")
}

fn promotion_branch(finding_key: &str) -> String {
    format!("sentinel/{}", slug(finding_key))
}

fn promotion_prompt(f: &Finding) -> String {
    format!(
        "The Debt Sentinel flagged this estate-debt finding for an overnight fix:\n\n{}\n\n{}\n\nInvestigate it, apply a safe fix, and open a draft PR. If it can't be fixed safely and autonomously, leave a short note in the branch note and stop — don't force it.",
        f.title,
        f.detail.as_deref().unwrap_or("")
    )
}

// --- patrol orchestrator ------------------------------------------------------------------------

/// The outcome of one patrol — the shape the CLI prints (`--json`) and summarizes.
#[derive(Debug, Clone, Serialize)]
pub struct PatrolReport {
    pub date: String,
    pub trend: Trend,
    pub headline: String,
    pub findings: Vec<Finding>,
    /// Attention items raised this patrol (new, keyed).
    pub raised: usize,
    /// Keyed items resolved by `--replace` because their rot disappeared.
    pub resolved: usize,
    pub promotions: Vec<Promotion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_path: Option<String>,
}

/// Run the full deterministic patrol: scan → surface (doctor-dedup) → ingest (`--replace`) →
/// promote (≤ [`MAX_PROMOTIONS`], allowlisted, deduped) → write the trend card → persist the
/// snapshot. Idempotent: re-running the same inputs the same day resolves nothing extra and raises
/// no duplicates (keys are stable).
pub fn patrol(
    cfg: &Config,
    inputs: &PatrolInputs,
    pcfg: &PatrolConfig,
    today: &str,
) -> std::io::Result<PatrolReport> {
    let prev = load_snapshot();
    let candidates = run_scanners(inputs, pcfg);
    let surfaced = surface(candidates.clone(), &prev);

    // Land the findings: keyed, source-namespaced, with --replace so vanished rot auto-resolves.
    let items: Vec<IngestItem> = surfaced.iter().map(Finding::ingest_item).collect();
    let outcome = attention::ingest(&items, SOURCE, "warn", true, today)?;

    // Promote the top picks as auto attention items the night shift can pull.
    let queued_keys: std::collections::BTreeSet<String> = attention::open_items(today)
        .into_iter()
        .filter_map(|i| i.key)
        .collect();
    let picks = select_promotions(&surfaced, pcfg, &queued_keys);
    let mut promotions = Vec::new();
    for f in picks {
        let Some(repo) = f.repo.clone() else { continue };
        let branch = promotion_branch(&f.key);
        let key = promotion_key(&f.key);
        let prompt = promotion_prompt(f);
        let add = attention::add_new(
            &NewAttn {
                // `manual` is the auto-loop trust boundary (see `is_auto_eligible`): a script-
                // sourced item is never auto-run. Promoting is the deliberate elevation of a pick
                // into that queue — exactly what `bn attn promote` does — so the night shift can
                // pull it (still gated by the night allowlist + enable + window). The
                // `staff-sentinel` origin rides in the key, not the source.
                source: "manual",
                title: &f.title,
                severity: Severity::Warn.as_str(),
                repo: Some(&repo),
                branch: Some(&branch),
                link: f.link.as_deref(),
                key: Some(&key),
                mode: Some("auto"),
                prompt: Some(&prompt),
                detail: f.detail.as_deref(),
                ..Default::default()
            },
            today,
        )?;
        promotions.push(Promotion {
            key,
            repo,
            branch,
            title: f.title.clone(),
            already_queued: add.skipped,
        });
    }

    let trend = compute_trend(&prev, &surfaced);
    let headline = trend.sentence();

    let card = build_card(today, &headline, &trend, &surfaced, &promotions);
    let card_path = write_card(cfg, &card).ok().map(|p| p.display().to_string());

    save_snapshot(&Snapshot {
        date: today.to_string(),
        findings: candidates
            .iter()
            .map(|f| SnapFinding {
                key: f.key.clone(),
                metric: f.metric,
                surfaced: surfaced.iter().any(|s| s.key == f.key),
            })
            .collect(),
    })?;

    Ok(PatrolReport {
        date: today.to_string(),
        trend,
        headline,
        findings: surfaced,
        raised: outcome.raised,
        resolved: outcome.resolved,
        promotions,
        card_path,
    })
}

fn build_card(
    today: &str,
    headline: &str,
    trend: &Trend,
    surfaced: &[Finding],
    promotions: &[Promotion],
) -> BriefingCard {
    let mut by_scanner: BTreeMap<&str, usize> = BTreeMap::new();
    for f in surfaced {
        *by_scanner.entry(f.scanner.tag()).or_default() += 1;
    }
    let breakdown = by_scanner
        .iter()
        .map(|(k, n)| format!("{n} {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    let body_md = format!(
        "Estate patrol {today}: {} surfaced ({} new, {} resolved, {} carried since the last patrol).{}{}",
        trend.total,
        trend.new,
        trend.resolved,
        trend.carried,
        if breakdown.is_empty() {
            String::new()
        } else {
            format!(" Breakdown: {breakdown}.")
        },
        if promotions.is_empty() {
            String::new()
        } else {
            format!(
                " Promoted {} for tonight: {}.",
                promotions.len(),
                promotions
                    .iter()
                    .map(|p| p.title.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        },
    );
    let items = surfaced
        .iter()
        .map(|f| CardItem {
            kind: "attn".to_string(),
            severity: f.severity.as_str().to_string(),
            title: f.title.clone(),
            link: f.link.clone().unwrap_or_default(),
            repo: f.repo.clone().unwrap_or_default(),
            branch: String::new(),
        })
        .collect();
    BriefingCard {
        agent: "sentinel".to_string(),
        date: today.to_string(),
        headline: headline.to_string(),
        body_md,
        items,
    }
}

// --- gather packet (side-effect-free) -----------------------------------------------------------

/// The read-only packet `bn staff gather sentinel` prints for the triage agent (and `bn staff
/// plan`): the surfaced findings, the trend, and the deterministic promotion picks — computed
/// exactly as [`patrol`] would, but landing nothing. Kept pure so the dry-run contract holds.
pub fn gather_packet(inputs: &PatrolInputs, pcfg: &PatrolConfig, today: &str) -> serde_json::Value {
    let prev = load_snapshot();
    let candidates = run_scanners(inputs, pcfg);
    let surfaced = surface(candidates, &prev);
    let trend = compute_trend(&prev, &surfaced);
    let picks = select_promotions(&surfaced, pcfg, &Default::default());
    serde_json::json!({
        "agent": "sentinel",
        "date": today,
        "headline": trend.sentence(),
        "trend": trend,
        "findings": surfaced,
        "promotion_candidates": picks.iter().map(|f| &f.key).collect::<Vec<_>>(),
    })
}

// --- small helpers ------------------------------------------------------------------------------

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Filename/branch-safe slug of a finding key.
fn slug(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').chars().take(60).collect()
}

// --- offline collectors -------------------------------------------------------------------------
//
// The CLI native job calls these to fill [`PatrolInputs`] from the live estate. They are the I/O
// half of the "gather = deterministic scanners" contract: no network, and each degrades to empty
// independently (a git failure, an absent cache, an empty event log) so one broken scanner never
// sinks the patrol. They live here — beside the pure scanners — so the whole member is one module.

/// Verify labels to blame: cron jobs whose name looks like a verify/test gate.
fn is_verify_job(job: &str) -> bool {
    job.contains("verify") || job.contains("test")
}

/// How many markers (each individually `git blame`d for age) one patrol will inspect — a hard
/// bound so the nightly job stays cheap regardless of how much debt a repo carries.
pub const MARKER_SCAN_CAP: usize = 80;

/// A file with at least this many commits in the recent window counts as "hot".
const HOT_FILE_COMMITS: usize = 3;

/// Build the full patrol inputs offline: notes + verify history + worktrees + markers + the daily
/// dep cache. Each collector degrades to empty/`None` independently.
pub fn collect_inputs(
    cfg: &Config,
    runner: &dyn CommandRunner,
    pcfg: &PatrolConfig,
    today: &str,
) -> PatrolInputs {
    PatrolInputs {
        markers: collect_markers(cfg, runner, pcfg),
        deps: load_dep_cache(today),
        verify_runs: collect_verify_runs(7),
        worktrees: collect_worktrees(cfg, runner, pcfg),
        notes: collect_notes(cfg, today),
    }
}

/// Note-hygiene state from the branch notes (the same source `bn doctor` reads): a note with open
/// work carries a `blocked_days` proxy (idle days while it has open blockers) and a `goal_less`
/// flag. Only notes with something worth escalating are returned.
pub fn collect_notes(cfg: &Config, today: &str) -> Vec<NoteState> {
    crate::rollups::all_notes(cfg)
        .into_iter()
        .filter_map(|bn| {
            let blockers = bn.note.front.blockers.len();
            let open_todos = bn
                .note
                .front
                .todos
                .iter()
                .filter(|t| t.is_open_todo())
                .count();
            if blockers == 0 && open_todos == 0 {
                return None; // no open work — nothing to nag about (matches the doctor)
            }
            let goal_less = crate::render::goal_text(&bn.note.body).is_none();
            let idle_days =
                crate::util::idle_days_since(&crate::doctor::last_activity(&bn.note), today);
            let blocked_days = if blockers > 0 { idle_days } else { 0 };
            if blocked_days == 0 && !goal_less {
                return None;
            }
            Some(NoteState {
                repo: bn.repo,
                branch: bn.branch,
                blocked_days,
                goal_less,
            })
        })
        .collect()
}

/// Verify-gate history from the machine-local event log: every `cron_run` for a verify/test job in
/// the trailing `window_days`, as pass/fail rows the flake scanner tallies. Offline (event log).
/// The per-*test* granularity the issue envisions arrives with the flake ledger; until then this
/// blames whole verify jobs (the precursor).
pub fn collect_verify_runs(window_days: i64) -> Vec<VerifyRun> {
    let cutoff = crate::util::now_epoch_secs() - window_days * 86_400;
    crate::events::read_all()
        .into_iter()
        .filter(|e| e.kind == "cron_run" && e.ts >= cutoff)
        .filter_map(|e| {
            let job = e.fields.get("job").and_then(Value::as_str)?;
            if !is_verify_job(job) {
                return None;
            }
            let passed = e
                .fields
                .get("ok")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| e.fields.get("exit_code").and_then(Value::as_i64) == Some(0));
            let repo = e
                .fields
                .get("repo")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(VerifyRun {
                repo,
                label: job.to_string(),
                passed,
            })
        })
        .collect()
}

/// Branch/worktree cruft from the worktrees rollup: `[gone]` upstreams (merged/deleted) and stale
/// non-main worktrees `wt cleanup` would refuse (dirty). One cheap `git status -sb` per known
/// worktree path; anything unreadable is skipped.
pub fn collect_worktrees(
    cfg: &Config,
    runner: &dyn CommandRunner,
    pcfg: &PatrolConfig,
) -> Vec<WorktreeState> {
    crate::rollups::worktrees(cfg)
        .into_iter()
        .filter(|w| pcfg.allows(&w.repo))
        .filter_map(|w| {
            let path = w.worktree.clone()?;
            let out = runner
                .git(&["-C", &path, "status", "-sb"])
                .ok_stdout()
                .unwrap_or_default();
            let mut lines = out.lines();
            let header = lines.next().unwrap_or("");
            let gone = header.contains("[gone]");
            let dirty = lines.any(|l| !l.trim().is_empty());
            let is_main = crate::agent::is_reserved_branch(&w.branch);
            let cleanup_refused = dirty && !is_main;
            if !gone && !cleanup_refused {
                return None;
            }
            Some(WorktreeState {
                repo: w.repo,
                branch: w.branch,
                gone,
                cleanup_refused,
                reason: if dirty {
                    "uncommitted changes".to_string()
                } else {
                    String::new()
                },
            })
        })
        .collect()
}

/// TODO/FIXME markers across the allowlisted repos' checked-out worktrees, aged by `git blame` and
/// flagged hot when their file churns. Bounded by [`MARKER_SCAN_CAP`]; all git is best-effort, so a
/// repo whose git errors simply contributes nothing.
pub fn collect_markers(
    cfg: &Config,
    runner: &dyn CommandRunner,
    pcfg: &PatrolConfig,
) -> Vec<TodoMarker> {
    let mut out = Vec::new();
    let mut seen_repo = HashSet::new();
    for w in crate::rollups::worktrees(cfg) {
        if out.len() >= MARKER_SCAN_CAP {
            break;
        }
        if !pcfg.allows(&w.repo) || !seen_repo.insert(w.repo.clone()) {
            continue;
        }
        let Some(path) = w.worktree else { continue };
        let budget = MARKER_SCAN_CAP - out.len();
        out.extend(markers_in_repo(runner, &w.repo, &path, budget));
    }
    out
}

fn markers_in_repo(
    runner: &dyn CommandRunner,
    repo: &str,
    path: &str,
    budget: usize,
) -> Vec<TodoMarker> {
    let grep = match runner
        .git(&["-C", path, "grep", "-nI", "-E", "TODO|FIXME"])
        .ok_stdout()
    {
        Some(s) => s,
        None => return Vec::new(), // no markers (or git errored) — degrade quietly
    };
    let hot = hot_files(runner, path);
    let now = crate::util::now_epoch_secs();
    grep.lines()
        .filter_map(parse_grep_line)
        .take(budget)
        .filter_map(|(file, line, text)| {
            let age_days = blame_age_days(runner, path, &file, line, now)?;
            Some(TodoMarker {
                repo: repo.to_string(),
                hot: hot.contains(&file),
                file,
                line,
                text,
                age_days,
            })
        })
        .collect()
}

/// Parse one `git grep -n` row (`path:line:content`) into its parts.
fn parse_grep_line(l: &str) -> Option<(String, u32, String)> {
    let mut it = l.splitn(3, ':');
    let file = it.next()?.to_string();
    let line: u32 = it.next()?.parse().ok()?;
    let text = it.next()?.trim().to_string();
    Some((file, line, text))
}

/// The age (days) of the commit that introduced `file:line`, via a single `git blame` on that line.
fn blame_age_days(
    runner: &dyn CommandRunner,
    path: &str,
    file: &str,
    line: u32,
    now: i64,
) -> Option<i64> {
    let spec = format!("{line},{line}");
    let out = runner
        .git(&[
            "-C",
            path,
            "blame",
            "-L",
            &spec,
            "--line-porcelain",
            "--",
            file,
        ])
        .ok_stdout()?;
    let ts: i64 = out
        .lines()
        .find_map(|l| l.strip_prefix("author-time "))
        .and_then(|s| s.trim().parse().ok())?;
    Some(((now - ts).max(0)) / 86_400)
}

/// Files with many recent commits (a churn/"hot" proxy), from one `git log` over the window.
fn hot_files(runner: &dyn CommandRunner, path: &str) -> HashSet<String> {
    let out = runner
        .git(&[
            "-C",
            path,
            "log",
            "--since=90.days.ago",
            "--name-only",
            "--pretty=format:",
        ])
        .ok_stdout()
        .unwrap_or_default();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for f in out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(5000)
    {
        *counts.entry(f.to_string()).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= HOT_FILE_COMMITS)
        .map(|(f, _)| f)
        .collect()
}

/// The daily dependency-staleness cache written by the (network-bound) registry-refresh job. Only a
/// cache dated *today* is trusted — a missing or stale cache degrades the dep scanner alone
/// (returns `None`), never the whole patrol. The registry-refresh writer is a follow-up; until it
/// lands this returns `None` on real machines and the dep scanner is simply quiet.
pub fn load_dep_cache(today: &str) -> Option<Vec<DepStatus>> {
    #[derive(Deserialize)]
    struct DepCache {
        #[serde(default)]
        date: String,
        #[serde(default)]
        deps: Vec<DepStatus>,
    }
    let path = crate::resolver::state_dir()
        .join("sentinel")
        .join("deps.json");
    let cache: DepCache = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (cache.date == today).then_some(cache.deps)
}
