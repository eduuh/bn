//! Remote-run tracking (#282) — surface `bn dispatch --remote` runs in `bn status` / `bn fleet`.
//!
//! `bn dispatch --remote` records a triggered GitHub Actions run as a `remote-run:` line in the
//! branch note (the durable record). This module reads those lines back, reconciles the still-open
//! ones against live `gh` (state + linked PR) via [`bn_core::remote::reconcile_remote_run`], maps
//! the outcome onto the #271 taxonomy, and renders them alongside local tmux agents. On `bn status`
//! it also persists a run's terminal state + PR URL into the note (so a run that finished while the
//! machine was off is reconciled on the next render) and emits one `remote_run` ledger event tagged
//! `backend=remote` for the economics view (#267/#381) — a column only, no cost math here.
//!
//! The note-record format + parsing live here (one source of truth); the taxonomy mapping and the
//! `gh` reconcile live in `bn_core::remote` behind the `CommandRunner` seam so both are unit-tested.

use crate::*;
use bn_core::cost::WorkClass;
use bn_core::model::Note;
use bn_core::remote::{reconcile_remote_run, RemoteRunReport};
use bn_core::runner::CommandRunner;
use bn_core::staff::RunStatus;
use bn_core::{events, store, util};
use serde::Serialize;
use serde_json::json;
use std::time::Duration;

/// The collab-section prefix marking a triggered remote-run record.
const RECORD_PREFIX: &str = "remote-run: ";

/// One parsed `remote-run:` branch-note record — the durable half of a remote dispatch. `run_id`
/// is `None` while a just-triggered run hasn't surfaced yet (recorded as `pending`); `corr` is the
/// correlation token that lets [`reconcile`](reconcile_note_runs) re-find such a pending run later.
/// `state` / `pr` are absent until a reconcile persists the terminal outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteRunRecord {
    pub run_id: Option<u64>,
    pub slug: String,
    pub workflow: String,
    pub corr: Option<String>,
    pub url: Option<String>,
    pub state: Option<RunStatus>,
    pub pr: Option<String>,
    /// The dispatch seed, already sanitized (see [`sanitize_seed`]) at record time.
    pub seed: String,
}

impl RemoteRunRecord {
    /// Render the one-line branch-note record. Optional fields (`corr` / url / `state` / `pr`) are
    /// emitted only when set, so a freshly-triggered run and a fully-reconciled one both round-trip
    /// through [`parse`](Self::parse). The seed is emitted verbatim (sanitized at record time).
    pub(crate) fn to_note_line(&self) -> String {
        let id = self
            .run_id
            .map_or_else(|| "pending".to_string(), |i| i.to_string());
        let mut s = format!(
            "{RECORD_PREFIX}run {id} · {} · {}",
            self.slug, self.workflow
        );
        if let Some(c) = &self.corr {
            s += &format!(" · corr:{c}");
        }
        if let Some(u) = &self.url {
            s += &format!(" · {u}");
        }
        if let Some(st) = self.state {
            s += &format!(" · state:{}", st.as_str());
        }
        if let Some(p) = &self.pr {
            s += &format!(" · pr:{p}");
        }
        s += &format!(" · seed: {}", self.seed);
        s
    }

    /// Parse a collab-section item back into a record, or `None` if it isn't a `remote-run:` line.
    /// Tolerant of both the freshly-triggered form and the reconciled form, and of the pre-#282
    /// lines that carry no `corr` / `state` / `pr` — a bare `http…` token is the run URL, everything
    /// after ` · seed: ` is the seed, and the first three ` · `-separated tokens are id/slug/workflow.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(RECORD_PREFIX)?;
        let (meta, seed) = match rest.split_once(" · seed: ") {
            Some((m, s)) => (m, s.to_string()),
            None => (rest, String::new()),
        };
        let mut tokens = meta.split(" · ");
        let run_tok = tokens.next()?.strip_prefix("run ")?.trim();
        let run_id = if run_tok == "pending" {
            None
        } else {
            Some(run_tok.parse::<u64>().ok()?)
        };
        let slug = tokens.next()?.trim().to_string();
        let workflow = tokens.next().unwrap_or("").trim().to_string();

        let mut rec = RemoteRunRecord {
            run_id,
            slug,
            workflow,
            corr: None,
            url: None,
            state: None,
            pr: None,
            seed,
        };
        for tok in tokens {
            let tok = tok.trim();
            if let Some(c) = tok.strip_prefix("corr:") {
                rec.corr = Some(c.to_string());
            } else if let Some(s) = tok.strip_prefix("state:") {
                rec.state = RunStatus::parse(s);
            } else if let Some(p) = tok.strip_prefix("pr:") {
                rec.pr = Some(p.to_string());
            } else if tok.starts_with("http") {
                rec.url = Some(tok.to_string());
            }
        }
        Some(rec)
    }

    /// Same underlying run as `other`? Matched on the correlation token when both carry one (stable
    /// across a pending→correlated transition), else on the run id. Used to re-locate a record for
    /// write-back after a fresh, locked re-read of the note.
    fn same_run(&self, other: &Self) -> bool {
        match (&self.corr, &other.corr) {
            (Some(a), Some(b)) => a == b,
            _ => matches!((self.run_id, other.run_id), (Some(a), Some(b)) if a == b),
        }
    }

    /// Fold a live [`RemoteRunReport`] into a new record: always adopt the resolved run id + URL,
    /// and persist the terminal taxonomy state + PR **only when it's safe to finalize** (see
    /// [`report_is_final`]). An `ok` run whose PR couldn't be confirmed (lookup `Unavailable`), or a
    /// still-running / vanished run, keeps `state`/`pr` unset so a later render re-reconciles it —
    /// we never freeze a success whose delivery we simply couldn't read, nor a run gh briefly lost.
    fn merged_with(&self, report: &RemoteRunReport) -> RemoteRunRecord {
        let mut r = self.clone();
        r.run_id = Some(report.run_id);
        r.url = Some(report.run_url.clone());
        if report_is_final(report) {
            r.state = report.state.run_status();
            if let Some(pr) = &report.pr {
                r.pr = Some(pr.url.clone());
            }
        }
        r
    }
}

/// Whether a reconcile result is safe to persist as the run's terminal outcome. A non-`ok` terminal
/// (`fail` / `auth` / `rate-limited` / `skipped`) finalizes as-is — none expects a PR. An `ok` run
/// finalizes only once its PR is confirmed: an `ok` with no PR means the lookup was `Unavailable`
/// (a genuine no-PR success was already reclassified `skipped` upstream), so it stays open and
/// retryable rather than freezing without its PR link. Running / vanished never finalize.
fn report_is_final(report: &RemoteRunReport) -> bool {
    use bn_core::remote::RemoteRunState;
    match report.state {
        RemoteRunState::Done(RunStatus::Ok) => report.pr.is_some(),
        RemoteRunState::Done(_) => true,
        RemoteRunState::Running | RemoteRunState::Vanished => false,
    }
}

/// A remote run flattened for rendering + JSON on `bn status` / `bn fleet`. `state` is the display
/// word (`pending` | `running` | `gone` | the #271 taxonomy); `run_status` carries the terminal
/// taxonomy value when finished (for coloring / counting).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RemoteRunView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
    pub slug: String,
    pub backend: &'static str,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_state: Option<String>,
    pub seed: String,
}

impl RemoteRunView {
    fn from_cache(rec: &RemoteRunRecord) -> Self {
        let (state, run_status) = match rec.state {
            Some(s) => (s.as_str().to_string(), Some(s.as_str().to_string())),
            None => ("pending".to_string(), None),
        };
        RemoteRunView {
            run_id: rec.run_id,
            slug: rec.slug.clone(),
            backend: "gh",
            state,
            run_status,
            run_url: rec.url.clone(),
            pr_url: rec.pr.clone(),
            pr_state: None,
            seed: rec.seed.clone(),
        }
    }

    fn from_report(rec: &RemoteRunRecord, report: &RemoteRunReport) -> Self {
        RemoteRunView {
            run_id: Some(report.run_id),
            slug: report.slug.clone(),
            backend: "gh",
            state: report.state.label().to_string(),
            run_status: report.state.run_status().map(|s| s.as_str().to_string()),
            run_url: Some(report.run_url.clone()),
            pr_url: report
                .pr
                .as_ref()
                .map(|p| p.url.clone())
                .or_else(|| rec.pr.clone()),
            pr_state: report.pr.as_ref().map(|p| p.state.clone()),
            seed: rec.seed.clone(),
        }
    }
}

/// The `remote-run:` records in a note's Collaboration section, in note order.
pub(crate) fn records_in(note: &Note) -> Vec<RemoteRunRecord> {
    note.front
        .collab
        .iter()
        .filter_map(|it| RemoteRunRecord::parse(&it.text))
        .collect()
}

/// Is `gh` usable right now? Gates every reconcile: without it, a `gh run view` failure is
/// indistinguishable from a genuinely-vanished run, so an offline `bn status` must render the
/// cached note state rather than mislabel live runs as `gone`.
fn gh_available(runner: &dyn CommandRunner) -> bool {
    runner.gh(&["auth", "status"]).success
}

/// Re-correlate a pending run (no id captured at dispatch) by its `corr` token: list the dispatch
/// workflow's runs and match the one whose name embeds the token — the same deterministic trick the
/// trigger uses (#281). `None` if it still hasn't surfaced.
fn recorrelate(runner: &dyn CommandRunner, slug: &str, corr: &str) -> Option<u64> {
    let args = run_list_args(slug);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let rows = parse_run_rows(&runner.gh(&refs).ok_stdout()?);
    run_matching_corr(&rows, corr)
}

/// Reconcile one record for display. Cached-terminal records and (when `gh` is unavailable) every
/// record render straight from the note with no network. Otherwise it resolves a run id — the
/// recorded one, or a re-correlated pending one — and reconciles it live. Returns the view plus, if
/// a live reconcile ran, the updated record (so the caller can write it back on `bn status`).
fn reconcile_one(
    runner: &dyn CommandRunner,
    rec: &RemoteRunRecord,
    gh_ok: bool,
) -> (RemoteRunView, Option<RemoteRunRecord>) {
    if rec.state.is_some() || !gh_ok {
        return (RemoteRunView::from_cache(rec), None);
    }
    let run_id = match rec.run_id {
        Some(id) => Some(id),
        None => rec
            .corr
            .as_deref()
            .and_then(|c| recorrelate(runner, &rec.slug, c)),
    };
    let Some(run_id) = run_id else {
        return (RemoteRunView::from_cache(rec), None);
    };
    let report = reconcile_remote_run(runner, &rec.slug, run_id);
    let view = RemoteRunView::from_report(rec, &report);
    (view, Some(rec.merged_with(&report)))
}

/// Reconcile + surface the current branch note's remote runs, persisting terminal outcomes. This is
/// the primary `bn status` path: for each open run it reconciles live `gh`, writes the run's
/// terminal state + PR URL back into the note (so the record advances once the run finishes, even if
/// the machine was off when it did), and emits one `remote_run` ledger event per newly-terminal run.
/// `gh` access routes through `runner` so the whole lifecycle is unit-testable with a `FakeRunner`.
pub(crate) fn track_current_note(
    cfg: &Config,
    ctx: &Resolved,
    runner: &dyn CommandRunner,
) -> Vec<RemoteRunView> {
    let note_md = cfg.note_file(ctx);
    let mut probe: Option<bool> = None;
    // The current note is small; reconcile all its open runs (no fleet-style cap).
    let mut budget = usize::MAX;
    reconcile_and_persist(&note_md, ctx, runner, &mut probe, &mut budget)
}

/// Reconcile + persist one note's remote runs against live `gh`, returning the rendered views. The
/// shared core behind `bn status` (its own note) and `bn fleet` (every note). `probe` memoizes the
/// one-time `gh` availability check; `budget` bounds how many open runs across a batch of notes hit
/// the network, so a large fleet never stalls the board. Open runs that reach a terminal state are
/// written back (monotonically, under a lock) and billed to the ledger once.
fn reconcile_and_persist(
    note_md: &std::path::Path,
    ctx: &Resolved,
    runner: &dyn CommandRunner,
    probe: &mut Option<bool>,
    budget: &mut usize,
) -> Vec<RemoteRunView> {
    let Ok(note) = store::read(note_md) else {
        return Vec::new();
    };
    let records = records_in(&note);
    if records.is_empty() {
        return Vec::new();
    }

    let mut views = Vec::with_capacity(records.len());
    // Updated records whose serialized line changed — applied in one locked read-modify-write.
    let mut updates: Vec<RemoteRunRecord> = Vec::new();
    for rec in &records {
        // Only open runs need `gh`, and only while the reconcile budget holds; a cached-terminal
        // run renders from the note with no network (and never re-probes gh).
        let want = rec.state.is_none() && *budget > 0;
        let gh_ok = want && *probe.get_or_insert_with(|| gh_available(runner));
        if gh_ok {
            *budget -= 1;
        }
        let (view, updated) = reconcile_one(runner, rec, gh_ok);
        if let Some(updated) = updated {
            if updated.to_note_line() != rec.to_note_line() {
                updates.push(updated);
            }
        }
        views.push(view);
    }

    if !updates.is_empty() {
        write_back(note_md, ctx, &updates);
    }
    views
}

/// Persist reconciled records into the note under a lock (guarding a concurrent writer), matching
/// each update to its collab item by run identity and merging **monotonically** into the freshly
/// re-read record — so a terminal state or PR another render wrote moments earlier is never
/// regressed, and the terminal-transition (ledger) decision reads the *fresh* state, not a stale
/// one. Emits the economics ledger line only for a run that crosses open → terminal, and only when
/// the note write lands — so each run is billed exactly once.
fn write_back(note_md: &std::path::Path, ctx: &Resolved, updates: &[RemoteRunRecord]) {
    let _lock = store::NoteLock::acquire(note_md);
    let Ok(mut note) = store::read(note_md) else {
        return;
    };
    let mut newly_terminal: Vec<RemoteRunRecord> = Vec::new();
    let mut changed = false;
    for updated in updates {
        for item in &mut note.front.collab {
            match RemoteRunRecord::parse(&item.text) {
                Some(fresh) if fresh.same_run(updated) => {
                    // Already finalized by a concurrent render — leave it, don't regress or re-bill.
                    if fresh.state.is_some() {
                        break;
                    }
                    // Fold newly-learned fields into the fresh record; never clear what it holds.
                    let mut merged = fresh.clone();
                    merged.run_id = updated.run_id.or(merged.run_id);
                    merged.url = updated.url.clone().or(merged.url);
                    if updated.state.is_some() {
                        merged.state = updated.state;
                    }
                    if updated.pr.is_some() {
                        merged.pr = updated.pr.clone();
                    }
                    let line = merged.to_note_line();
                    if item.text != line {
                        // fresh.state was None (guarded above) → a terminal merged.state is the
                        // open → terminal transition that bills the ledger.
                        if merged.state.is_some() {
                            newly_terminal.push(merged.clone());
                        }
                        item.text = line;
                        changed = true;
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    if changed {
        // Bill the ledger only if the note write actually lands — otherwise the next render
        // re-reconciles and re-bills. Keeps the note state and the ledger consistent.
        let saved = store::save(&note, note_md);
        let ok = saved.is_ok();
        util::best_effort(saved, "remote-track: write back note");
        if ok {
            for rec in &newly_terminal {
                emit_ledger(ctx, rec);
            }
        }
    }
}

/// Emit one `remote_run` economics event for a finished remote run, tagged `backend=remote` /
/// `work_class=remote` so `bn cost` (#267/#381) can pick it up as a Remote-backend ledger row. No
/// token/cost figures — a hosted run surfaces none to bn — so this is a column-only hook by design.
fn emit_ledger(ctx: &Resolved, rec: &RemoteRunRecord) {
    let Some(status) = rec.state else { return };
    let mut f = serde_json::Map::new();
    f.insert("repo".into(), json!(ctx.repo));
    f.insert("branch".into(), json!(ctx.branch));
    f.insert("slug".into(), json!(rec.slug));
    if let Some(id) = rec.run_id {
        f.insert("run_id".into(), json!(id));
    }
    f.insert("date".into(), json!(util::today()));
    f.insert("status".into(), json!(status.as_str()));
    f.insert("backend".into(), json!("remote"));
    f.insert("work_class".into(), json!(WorkClass::Remote.as_str()));
    if let Some(pr) = &rec.pr {
        f.insert("pr".into(), json!(pr));
    }
    util::best_effort(
        events::append("remote_run", f),
        "remote-track: log remote_run ledger event",
    );
}

/// Remote-run views across every local branch note, for the `bn fleet` board and `bn agent live`.
/// Reconciles open runs live (bounded by [`FLEET_RECONCILE_CAP`] so a big fleet never stalls the
/// board) and persists any that reached a terminal state — so a run finalizes even when the fleet
/// board is the only surface the operator ever opens (`bn status` isn't the sole reconcile point).
pub(crate) fn fleet_views(cfg: &Config, runner: &dyn CommandRunner) -> Vec<RemoteRunView> {
    let mut probe: Option<bool> = None;
    let mut budget = FLEET_RECONCILE_CAP;
    let mut out = Vec::new();
    for bn in rollups::all_notes(cfg) {
        let ctx = Resolved {
            repo: bn.repo,
            branch: bn.branch,
        };
        out.extend(reconcile_and_persist(
            &bn.path,
            &ctx,
            runner,
            &mut probe,
            &mut budget,
        ));
    }
    out
}

struct BoundedRunner<'a> {
    inner: &'a dyn CommandRunner,
    timeout: Duration,
}

impl CommandRunner for BoundedRunner<'_> {
    fn run(&self, program: &str, args: &[&str]) -> bn_core::runner::CommandOutput {
        self.inner.run_with_timeout(program, args, self.timeout)
    }
}

/// Bounded, read-only remote-run projection for [`crate::agent_live::FleetSnapshot`]. Open runs
/// reconcile against GitHub for current display, but learned state is never written back.
pub(crate) fn fleet_views_snapshot(cfg: &Config, runner: &dyn CommandRunner) -> Vec<RemoteRunView> {
    let bounded = BoundedRunner {
        inner: runner,
        timeout: Duration::from_secs(2),
    };
    let mut probe: Option<bool> = None;
    let mut budget = FLEET_RECONCILE_CAP;
    let mut views = Vec::new();
    for branch_note in rollups::all_notes(cfg) {
        for record in records_in(&branch_note.note) {
            let wants_live = record.state.is_none() && budget > 0;
            let gh_ok = wants_live && *probe.get_or_insert_with(|| gh_available(&bounded));
            if gh_ok {
                budget -= 1;
            }
            views.push(reconcile_one(&bounded, &record, gh_ok).0);
        }
    }
    views
}

/// Most open remote runs the fleet board reconciles live per render — keeps an at-a-glance board
/// snappy while still refreshing a realistic number of in-flight runs.
const FLEET_RECONCILE_CAP: usize = 8;

/// A one-line-per-run text rendering for `bn status` / `bn fleet` (glyph + state + PR + seed).
pub(crate) fn render_line(v: &RemoteRunView) -> String {
    let mark = match v.state.as_str() {
        "ok" => "✓ ok         ",
        "fail" => "✗ fail       ",
        "auth" => "⚠ auth       ",
        "rate-limited" => "⏳ rate-limit ",
        "skipped" => "· skipped    ",
        "running" => "… running    ",
        "gone" => "? gone       ",
        _ => "… pending    ",
    };
    let id = v.run_id.map_or_else(|| "?".to_string(), |i| i.to_string());
    let pr = match (&v.pr_url, &v.pr_state) {
        (Some(u), Some(st)) => format!("  PR {u} ({})", st.to_lowercase()),
        (Some(u), None) => format!("  PR {u}"),
        _ => String::new(),
    };
    let seed = if v.seed.is_empty() {
        String::new()
    } else {
        format!("  — {}", v.seed)
    };
    format!("  {mark} gh  run {id}  {}{pr}{seed}", v.slug)
}


