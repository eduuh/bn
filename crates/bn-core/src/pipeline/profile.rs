//! Per-stage profile resolution — what runs at a stage, and whether it waits for a human.
//!
//! One [`StageProfile`] answers every question a launcher has about a stage: which model, effort
//! and harness, which workflow skill, which references, and whether the transition into it is held
//! for review. Six layers fold into it, low → high:
//!
//! ```text
//! built-in default → [pipeline.stages.S] → [repo.R.pipeline.stages.S] → task meta → note decision → CLI flag
//! ```
//!
//! [`layer`] is pure: the caller gathers the three non-config layers into an [`Overrides`] and the
//! folding itself touches no store. [`resolve`] is the thin IO wrapper that reads the governing
//! task and the branch note to build that `Overrides`. Keeping the split means the precedence
//! rules — the part that is easy to get subtly wrong — are unit-testable without a tempdir.
//!
//! # The gate ratchet
//!
//! `gate` does **not** follow the precedence above. Every layer below the CLI may only tighten it
//! (`auto → hold`), never loosen it. This is load-bearing, not fastidiousness: `bn_add` exposes
//! `decision` as a writable section over MCP, so any dispatched agent can append note decisions. A
//! plain last-writer-wins precedence would let an agent that gets held write `pipeline: execute
//! gate=auto` and release itself — the gate deleting itself. Only an explicit `--gate` typed by a
//! human on the invocation can loosen a hold. `skill` and `references` keep the full precedence,
//! where the worst case is an agent picking its own workflow: annoying, not a hole.

use super::stage::Stage;
use crate::model::Note;
use crate::resolver::{Config, RepoBranch, StageConfig};
use crate::todo::Task;

/// The prefix a note decision uses to pin part of a stage profile: `pipeline: <stage> key=value …`.
const DECISION_PREFIX: &str = "pipeline:";

/// Whether entering a stage waits for a human.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GatePolicy {
    /// Pass straight through — the default for every stage, so nothing changes until asked.
    #[default]
    Auto,
    /// Refuse the transition, raise an attention item, and wait for `bn pipeline resume`.
    Hold,
}

impl GatePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            GatePolicy::Auto => "auto",
            GatePolicy::Hold => "hold",
        }
    }

    /// Parse a raw config/meta value. Unknown values are `None` so a typo inherits rather than
    /// silently choosing a policy the author didn't write.
    pub fn parse(raw: &str) -> Option<GatePolicy> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(GatePolicy::Auto),
            "hold" => Some(GatePolicy::Hold),
            _ => None,
        }
    }
}

impl std::fmt::Display for GatePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which references a stage attaches to its seed.
///
/// A distinct type rather than a bare `Vec`, because the CLI's historical encoding (empty slice
/// means *all*) reads backwards in TOML, where `references = []` obviously means *none*.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "names")]
pub enum RefSelection {
    /// Every reference in the store — the dispatch default, written `["*"]` in config.
    All,
    /// Exactly these names; empty attaches none.
    Named(Vec<String>),
}

impl RefSelection {
    /// Interpret raw tokens. `*` (or the CLI's legacy `+`) anywhere selects everything; the CLI's
    /// legacy `-` selects nothing. Names are trimmed and blanks dropped.
    pub fn parse(tokens: &[String]) -> RefSelection {
        if tokens.iter().any(|t| matches!(t.trim(), "*" | "+")) {
            return RefSelection::All;
        }
        if tokens.iter().any(|t| t.trim() == "-") {
            return RefSelection::Named(Vec::new());
        }
        RefSelection::Named(
            tokens
                .iter()
                .flat_map(|t| t.split(','))
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }
}

/// Which layer supplied a resolved field — what `bn pipeline preview` prints so a surprising
/// value can be traced to the line that set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layer {
    Builtin,
    Global,
    Repo,
    TaskMeta,
    NoteDecision,
    Cli,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Builtin => "built-in",
            Layer::Global => "[pipeline.stages]",
            Layer::Repo => "[repo.*.pipeline.stages]",
            Layer::TaskMeta => "task meta",
            Layer::NoteDecision => "note decision",
            Layer::Cli => "cli flag",
        }
    }
}

/// Where each resolved field came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Provenance {
    pub model: Layer,
    pub effort: Layer,
    pub harness: Layer,
    pub skill: Layer,
    pub references: Layer,
    pub gate: Layer,
}

/// The fully resolved per-stage profile.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StageProfile {
    pub stage: Stage,
    pub model: String,
    pub effort: String,
    /// `None` when no layer pinned one; the launcher then uses the `--agent`/global default.
    pub harness: Option<String>,
    /// `None` when no layer pinned one; the launcher falls back to its built-in stage default.
    pub skill: Option<String>,
    pub references: RefSelection,
    pub gate: GatePolicy,
    pub provenance: Provenance,
    /// Set when the gate ratchet blocked a layer from loosening a hold — surfaced by `preview` so
    /// a pin that looks ignored is explained rather than mysterious.
    pub gate_ratcheted_from: Option<Layer>,
}

impl StageProfile {
    /// Whether entering this stage waits for a human.
    pub fn is_held(&self) -> bool {
        self.gate == GatePolicy::Hold
    }
}

/// The three non-config layers, gathered by the caller so [`layer`] stays pure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub task: StageConfig,
    pub note: StageConfig,
    pub cli: StageConfig,
}

/// The built-in model/effort for a stage when nothing overrides it.
fn builtin(stage: Stage) -> (&'static str, &'static str) {
    match stage {
        Stage::Plan | Stage::Critique => ("claude-opus-4.8", "max"),
        Stage::Investigate => ("gpt-5.4", "high"),
        _ => ("gpt-5.4-mini", "medium"),
    }
}

/// A non-blank override value, or `None`. Blank/whitespace entries inherit rather than clearing,
/// so `model = ""` in a repo block doesn't wipe the global choice.
fn present(value: Option<&String>) -> Option<&str> {
    value.map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// Fold `Config`'s global then repo entry for `stage` into `(global, repo)` layers.
fn config_layers<'a>(
    cfg: &'a Config,
    repo: &str,
    stage: Stage,
) -> (Option<&'a StageConfig>, Option<&'a StageConfig>) {
    let global = cfg.pipeline.stages.get(&stage);
    let base = repo.rsplit('/').next().unwrap_or(repo);
    let by = |want: &str| {
        cfg.repos
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(want))
            .map(|(_, value)| value)
    };
    let repo_entry = by(repo)
        .or_else(|| by(base))
        .and_then(|r| r.pipeline.as_ref())
        .and_then(|p| p.stages.get(&stage));
    (global, repo_entry)
}

/// Resolve a stage profile from config plus the caller-gathered override layers. **Pure.**
///
/// Precedence runs low → high as listed in the module docs, per field independently: a repo entry
/// that sets only `model` leaves an inherited `skill` alone. `gate` is the exception — see the
/// ratchet note in the module docs.
pub fn layer(cfg: &Config, repo: &str, stage: Stage, ov: &Overrides) -> StageProfile {
    let (default_model, default_effort) = builtin(stage);
    let (global, repo_entry) = config_layers(cfg, repo, stage);

    // Low → high. `gate` deliberately does not use this ordering.
    let ordered: [(Layer, Option<&StageConfig>); 5] = [
        (Layer::Global, global),
        (Layer::Repo, repo_entry),
        (Layer::TaskMeta, Some(&ov.task)),
        (Layer::NoteDecision, Some(&ov.note)),
        (Layer::Cli, Some(&ov.cli)),
    ];

    let mut model = (default_model.to_string(), Layer::Builtin);
    let mut effort = (default_effort.to_string(), Layer::Builtin);
    let mut harness: (Option<String>, Layer) = (None, Layer::Builtin);
    let mut skill: (Option<String>, Layer) = (None, Layer::Builtin);
    let mut references = (RefSelection::All, Layer::Builtin);

    for (which, entry) in ordered {
        let Some(entry) = entry else { continue };
        if let Some(v) = present(entry.model.as_ref()) {
            model = (v.to_string(), which);
        }
        if let Some(v) = present(entry.effort.as_ref()) {
            effort = (v.to_string(), which);
        }
        if let Some(v) = present(entry.harness.as_ref()) {
            harness = (Some(v.to_string()), which);
        }
        if let Some(v) = present(entry.skill.as_ref()) {
            skill = (Some(v.to_string()), which);
        }
        if let Some(tokens) = entry.references.as_ref() {
            references = (RefSelection::parse(tokens), which);
        }
    }

    // The ratchet: below the CLI, a layer may only tighten. `Hold > Auto` by derived ordering.
    let mut gate = (GatePolicy::Auto, Layer::Builtin);
    let mut ratcheted_from = None;
    for (which, entry) in ordered {
        let Some(entry) = entry else { continue };
        let Some(raw) = present(entry.gate.as_ref()) else {
            continue;
        };
        let Some(want) = GatePolicy::parse(raw) else {
            continue;
        };
        if which == Layer::Cli {
            gate = (want, which); // an explicit human flag is the only way down
        } else if want > gate.0 {
            gate = (want, which);
        } else if want < gate.0 {
            ratcheted_from = Some(which); // a lower layer tried to loosen; record and ignore
        }
    }

    StageProfile {
        stage,
        model: model.0,
        effort: effort.0,
        harness: harness.0,
        skill: skill.0,
        references: references.0,
        gate: gate.0,
        provenance: Provenance {
            model: model.1,
            effort: effort.1,
            harness: harness.1,
            skill: skill.1,
            references: references.1,
            gate: gate.1,
        },
        gate_ratcheted_from: ratcheted_from,
    }
}

/// Per-stage pins a task carries as open-ended `key:value` meta: `plan-skill:lavish-plan`,
/// `plan-gate:hold`, `plan-refs:rust,api`. **Pure.**
///
/// The `<stage>-<facet>` shape is forced by the todo grammar, which rejects `.` in a meta key, and
/// by `stage:` already meaning the task's *current* stage.
pub fn from_task_meta(task: &Task, stage: Stage) -> StageConfig {
    let key = |facet: &str| format!("{}-{facet}", stage.as_str());
    let get = |facet: &str| task.meta(&key(facet)).map(str::to_string);
    StageConfig {
        model: get("model"),
        effort: get("effort"),
        harness: get("harness").or_else(|| get("agent")),
        skill: get("skill"),
        references: get("refs").or_else(|| get("references")).map(|v| vec![v]),
        gate: get("gate"),
    }
}

/// Per-stage pins recorded in the note as decisions. **Pure.**
///
/// Format: `pipeline: <stage> skill=lavish-plan refs=rust,api gate=hold`. Decisions are appended
/// rather than keyed, so several may name the same stage; the **last** one wins, per field, which
/// is what lets a pin be changed at all (the note store has no delete path).
pub fn from_note_decisions(note: &Note, stage: Stage) -> StageConfig {
    let mut out = StageConfig::default();
    for item in &note.front.decisions {
        let Some(rest) = item.text.trim().strip_prefix(DECISION_PREFIX) else {
            continue;
        };
        let mut words = rest.split_whitespace();
        if words.next().map(str::trim) != Some(stage.as_str()) {
            continue;
        }
        for pair in words {
            let Some((k, v)) = pair.split_once('=') else {
                continue;
            };
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            match k.trim() {
                "model" => out.model = Some(v.to_string()),
                "effort" => out.effort = Some(v.to_string()),
                "harness" | "agent" => out.harness = Some(v.to_string()),
                "skill" => out.skill = Some(v.to_string()),
                "refs" | "references" => out.references = Some(vec![v.to_string()]),
                "gate" => out.gate = Some(v.to_string()),
                _ => {}
            }
        }
    }
    out
}

/// Read the governing task and branch note, then [`layer`] them under `cli`.
///
/// Tolerant by design: a missing task, an unreadable note, or an absent store each contribute
/// nothing rather than failing. A profile must always resolve — a launcher that can't determine
/// its model because the note is briefly locked is worse than one that uses the configured value.
pub fn resolve(cfg: &Config, rb: RepoBranch<'_>, stage: Stage, cli: &StageConfig) -> StageProfile {
    let task = super::gate::governing_task(cfg, rb.repo, rb.branch)
        .map(|t| from_task_meta(&t, stage))
        .unwrap_or_default();
    let note = crate::store::read(&cfg.note_file(&crate::resolver::Resolved {
        repo: rb.repo.to_string(),
        branch: rb.branch.to_string(),
    }))
    .ok()
    .map(|n| from_note_decisions(&n, stage))
    .unwrap_or_default();
    layer(
        cfg,
        rb.repo,
        stage,
        &Overrides {
            task,
            note,
            cli: cli.clone(),
        },
    )
}


