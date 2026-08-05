//! Self-describing info agents (`agents/<name>.md`).
//!
//! One `info-gather` cron replaces the fleet of per-info timers: every 15 minutes it loads these
//! markdown agents, runs the ones that are **due** (their `on_calendar` fired since their last
//! successful run), and dispatches each into its own tmux window — reusing the window in place.
//! An agent can also declare `trigger: startup` so `bn startup` fires it once when the tmux server
//! boots. The gather itself still runs via `bn run <agent.name>`, so the `.md` is metadata +
//! placement + description, never a reimplementation of the existing native/staff/script job.
//!
//! Parsing routes through [`crate::store::split_frontmatter`] (the crate's single frontmatter
//! parser) and the "due" check composes [`crate::staff_schedule::previous_occurrence`] over the
//! same `OnCalendar` subset the cron generator understands — no second schedule parser. Discovery
//! mirrors [`crate::skills::discover`] / [`crate::resolver::agent_dirs`]: branch → repo → global,
//! first tier wins by name.

use crate::crons::CronClass;
use crate::events::CronRunDetail;
use crate::resolver::{self, Config, JobScope, Resolved};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// An explicit trigger declared in an agent's frontmatter. Cron eligibility is derived from the
/// presence of `on_calendar` rather than this list, so `startup` is the one trigger that adds
/// behaviour: fire once when the tmux server boots (in addition to any schedule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    /// Fire on the `on_calendar` schedule (the implicit default when a schedule is present).
    Cron,
    /// Fire once when the tmux server starts, via `bn startup`.
    Startup,
}

/// A `trigger:` field that accepts either a single value (`trigger: startup`) or a list
/// (`trigger: [cron, startup]`), so the manifest reads naturally either way.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum TriggerSpec {
    One(Trigger),
    Many(Vec<Trigger>),
}

impl TriggerSpec {
    fn into_vec(self) -> Vec<Trigger> {
        match self {
            TriggerSpec::One(t) => vec![t],
            TriggerSpec::Many(v) => v,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_info_class() -> CronClass {
    CronClass::Info
}

/// A roster / MCP name must stay filename-safe so frontmatter cannot escape config dirs.
pub(crate) fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A reusable skill path may contain `/`, but never an absolute path or `..`.
pub(crate) fn safe_skill(s: &str) -> bool {
    !s.is_empty()
        && !s.contains("..")
        && !s.starts_with('/')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/')
}

fn validate_agent_frontmatter(
    front: &AgentFrontmatter,
    name: &str,
    path: &Path,
) -> Result<(), String> {
    if !safe_name(name) {
        return Err(format!(
            "{}: agent name '{}' must be alphanumeric/-/_",
            path.display(),
            name
        ));
    }
    if front.gather.as_deref().is_some_and(|g| g.trim().is_empty()) {
        return Err(format!(
            "{}: agent '{}': gather command is empty",
            path.display(),
            name
        ));
    }
    for s in &front.sinks {
        if !crate::staff::SINKS.contains(&s.as_str()) {
            return Err(format!(
                "{}: agent '{}': unknown sink '{}' (allowed: {})",
                path.display(),
                name,
                s,
                crate::staff::SINKS.join(" | ")
            ));
        }
    }
    for sk in &front.skills {
        if !safe_skill(sk) {
            return Err(format!(
                "{}: agent '{}': unsafe skill path '{}'",
                path.display(),
                name,
                sk
            ));
        }
    }
    for m in &front.mcps {
        if !safe_name(m) {
            return Err(format!(
                "{}: agent '{}': mcp name '{}' must be alphanumeric/-/_",
                path.display(),
                name,
                m
            ));
        }
    }
    Ok(())
}

/// Raw frontmatter as declared in `agents/<name>.md`, before normalization into [`AgentDef`].
#[derive(Debug, Deserialize)]
struct AgentFrontmatter {
    name: Option<String>,
    #[serde(default = "default_info_class")]
    class: CronClass,
    #[serde(default)]
    on_calendar: Option<String>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    window: Option<String>,
    #[serde(default)]
    network: bool,
    #[serde(default = "default_true")]
    reuse: bool,
    #[serde(default)]
    trigger: Option<TriggerSpec>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    gather: Option<String>,
    #[serde(default)]
    sinks: Vec<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    mcps: Vec<String>,
    #[serde(default)]
    link_domains: Vec<String>,
    #[serde(default)]
    harness: Option<String>,
    // --- specialist fields (a specialist is an agent with `repo:` set) ---
    /// Repo this specialist is gated to. Presence marks the agent as a review specialist that
    /// compiles to native Copilot/Claude custom agents rather than a cron/staff gather agent.
    #[serde(default)]
    repo: Option<String>,
    /// bn reference docs whose content is inlined into the compiled native agent body.
    #[serde(default)]
    references: Vec<String>,
    /// ADO repo the `bn agents bootstrap` mining run harvests review comments from.
    #[serde(default)]
    source_repo: Option<String>,
    /// Native worlds to compile into (`copilot`, `claude`). Empty = both.
    #[serde(default)]
    worlds: Vec<String>,
    /// Native tool aliases granted to the compiled Copilot agent
    /// (`read`/`edit`/`search`/`execute`/`agent`/`web`/`todo`, or `*`).
    #[serde(default)]
    tools: Vec<String>,
    /// Model override written into the compiled Copilot agent frontmatter.
    #[serde(default)]
    model: Option<String>,
}

/// A resolved agent definition: its metadata, the tier it was discovered in, and its body text.
#[derive(Debug, Clone)]
pub struct AgentDef {
    /// The `bn run <name>` job this agent gathers with — also the default window name.
    pub name: String,
    /// Visibility class of the dispatched window (`info` = prints its result, the default).
    pub class: CronClass,
    /// systemd `OnCalendar` schedule; present for a cron agent, absent for a startup-only one.
    pub on_calendar: Option<String>,
    /// The shell command this agent runs in its window when due. Declared in frontmatter as
    /// `run:`; defaults to `bn run <name>` so a file that names an existing job still runs. This
    /// is the whole contract — drop a `.md` with a `run:` command and it is dispatched on schedule.
    pub run: String,
    /// tmux placement: `session:window`, `:window` (the resolved cron/fleet session + this window),
    /// `session` (window defaults to `name`), or absent (the cron session, window = `name`).
    /// Resolved by [`placement`].
    pub window: Option<String>,
    /// The gather hits the network (informational; the schedule generator carries the real gate).
    pub network: bool,
    /// Reuse an existing target window in place (`respawn-window -k`) instead of a fresh window.
    pub reuse: bool,
    /// Explicit triggers declared in frontmatter (only `startup` adds behaviour — see [`Trigger`]).
    pub triggers: Vec<Trigger>,
    /// One-line description of what this agent gathers.
    pub description: Option<String>,
    /// Deterministic packet command. Presence marks this as an LLM/staff agent.
    pub gather: Option<String>,
    /// Per-agent sink allowlist for routed briefing-card items.
    pub sinks: Vec<String>,
    /// Reusable bn skills available to this agent, referred to by name (loaded natively in its
    /// harness — not invoked as slash-command prompts).
    pub skills: Vec<String>,
    /// MCP servers/tools this agent connects to.
    pub mcps: Vec<String>,
    /// Extra card-link domains allowed for this agent.
    pub link_domains: Vec<String>,
    /// Headless harness override (`copilot` / `claude`).
    pub harness: Option<String>,
    /// Repo this specialist is gated to; `Some` marks the agent as a review specialist.
    pub repo: Option<String>,
    /// bn reference docs inlined into the compiled native agent.
    pub references: Vec<String>,
    /// ADO repo the `bn agents bootstrap` mining run harvests review comments from.
    pub source_repo: Option<String>,
    /// Native worlds this specialist compiles into (`copilot`/`claude`); empty = both.
    pub worlds: Vec<String>,
    /// Native tool aliases for the compiled Copilot agent.
    pub tools: Vec<String>,
    /// Model override for the compiled Copilot agent.
    pub model: Option<String>,
    /// Markdown body after the frontmatter fence — the prompt for gather agents.
    pub body: String,
    /// The tier this definition was discovered in (branch shadows repo shadows global).
    pub scope: JobScope,
    /// Absolute path to the source `.md` file.
    pub path: PathBuf,
}

impl AgentDef {
    /// True when this agent participates in the `info-gather` cron (it has a schedule).
    pub fn is_cron(&self) -> bool {
        self.on_calendar.is_some()
    }

    /// True when this agent should also fire once on tmux-server startup (`bn startup`).
    pub fn is_startup(&self) -> bool {
        self.triggers.contains(&Trigger::Startup)
    }

    /// True when this agent runs through the LLM staff pipeline.
    pub fn is_staff(&self) -> bool {
        self.gather.is_some()
    }

    /// True when this agent is a repo-scoped review specialist — an agent with `repo:` set that
    /// compiles to native Copilot/Claude custom agents (and is skipped by every cron/staff path).
    pub fn is_specialist(&self) -> bool {
        self.repo.is_some()
    }

    /// Whether this agent is DUE now: never run, last run failed, or a scheduled occurrence has
    /// passed since the last successful run. A startup-only agent (no `on_calendar`) is never due
    /// on the cron path. `last_run` is the agent's most recent recorded run of its `bn run` job.
    pub fn due(&self, last_run: Option<&CronRunDetail>, now: i64) -> bool {
        match self.on_calendar.as_deref() {
            Some(cal) => due(cal, last_run, now),
            None => false,
        }
    }

    /// Whether this agent may be launched *now* by the shared window/startup dispatch path. A
    /// **scheduled** agent (has `on_calendar`) is launchable only when its schedule is [`due`]
    /// (never-run / last-failed still due, per `due()`); an **unscheduled** startup-class agent
    /// (no `on_calendar`) is always launchable, so `bn startup` still fires it on every boot. This
    /// is the single gate that keeps a weekly agent from opening its window off-schedule
    /// regardless of which path (info-gather cron, `bn startup`, …) reaches the dispatcher.
    pub fn launchable(&self, last_run: Option<&CronRunDetail>, now: i64) -> bool {
        self.on_calendar.is_none() || self.due(last_run, now)
    }
}

/// Whether a scheduled occurrence of `on_calendar` has passed since the last successful run. Pure
/// (no I/O) so it is unit-testable: a never-run or last-FAILED agent is always due; otherwise it
/// is due when the most recent scheduled slot at or before `now` is newer than the last run. A
/// schedule with no enumerable slot (unmodeled shape) errs toward due, matching the degrade-loud
/// contract elsewhere — better a spurious refresh than a silently stalled agent.
pub fn due(on_calendar: &str, last_run: Option<&CronRunDetail>, now: i64) -> bool {
    match last_run {
        None => return true,
        Some(r) if r.exit_code != 0 => return true,
        Some(_) => {}
    }
    let last_ts = last_run.map(|r| r.ts).unwrap_or(0);
    match crate::staff_schedule::previous_occurrence(on_calendar, now) {
        Some(slot) => slot > last_ts,
        None => true,
    }
}

/// Resolved tmux placement: which session and window an agent's gather window lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub session: String,
    pub window: String,
}

/// Resolve an agent's `window` field into a concrete `session:window`:
/// * `Some("session:window")` → that exact session + window;
/// * `Some(":window")` → the `fallback_session` (the resolved cron/fleet session) + that window —
///   the form agents use to name their window while still following the `[jobs].session` knob;
/// * `Some("session")` → that session, window defaults to `name`;
/// * `None` / empty → the `fallback_session` (today's cron session), window = `name`.
///
/// Pure so placement precedence is unit-testable without tmux.
pub fn placement(window: Option<&str>, name: &str, fallback_session: &str) -> Placement {
    let field = window.map(str::trim).filter(|s| !s.is_empty());
    match field {
        None => Placement {
            session: fallback_session.to_string(),
            window: name.to_string(),
        },
        Some(w) => match w.split_once(':') {
            Some((sess, win)) => {
                let sess = sess.trim();
                let win = win.trim();
                Placement {
                    session: if sess.is_empty() {
                        fallback_session.to_string()
                    } else {
                        sess.to_string()
                    },
                    window: if win.is_empty() {
                        name.to_string()
                    } else {
                        win.to_string()
                    },
                }
            }
            None => Placement {
                session: w.to_string(),
                window: name.to_string(),
            },
        },
    }
}

/// Parse one `agents/<name>.md`. `Ok(None)` when there is no frontmatter fence; errors are
/// configuration hard-failures (invalid YAML, unsafe names, unknown sinks). `name` falls back to
/// the file stem, mirroring [`crate::skills::parse_skill`].
pub fn parse_agent(
    text: &str,
    stem: &str,
    scope: JobScope,
    path: PathBuf,
) -> Result<Option<AgentDef>, String> {
    let Some((yaml, body)) = crate::store::split_frontmatter(text) else {
        return Ok(None);
    };
    let front: AgentFrontmatter =
        serde_yaml::from_str(yaml).map_err(|e| format!("{}: {e}", path.display()))?;
    let triggers = front
        .trigger
        .clone()
        .map(TriggerSpec::into_vec)
        .unwrap_or_default();
    let name = front.name.clone().unwrap_or_else(|| stem.to_string());
    validate_agent_frontmatter(&front, &name, &path)?;
    let run = front
        .run
        .clone()
        .unwrap_or_else(|| format!("bn run {name}"));
    Ok(Some(AgentDef {
        name,
        class: front.class,
        on_calendar: front.on_calendar,
        run,
        window: front.window,
        network: front.network,
        reuse: front.reuse,
        triggers,
        description: front.description,
        gather: front.gather,
        sinks: front.sinks,
        skills: front.skills,
        mcps: front.mcps,
        link_domains: front.link_domains,
        harness: front.harness,
        repo: front.repo,
        references: front.references,
        source_repo: front.source_repo,
        worlds: front.worlds,
        tools: front.tools,
        model: front.model,
        body: body.trim_start().to_string(),
        scope,
        path,
    }))
}

fn load_agent_files(dir: &Path, scope: JobScope) -> Vec<AgentDef> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("agent");
        match parse_agent(&text, stem, scope, path.clone()) {
            Ok(Some(def)) => out.push(def),
            Ok(None) => {}
            Err(e) => eprintln!("bn: skipping malformed agent {e}"),
        }
    }
    out
}

/// Every agent across all tiers for `ctx`, folded first-tier-wins by name (a branch `agents/`
/// shadows a repo or shipped default), then sorted by name for deterministic dispatch order.
pub fn discover(cfg: &Config, ctx: Option<&Resolved>) -> Vec<AgentDef> {
    let mut out: Vec<AgentDef> = Vec::new();
    for (scope, dir) in resolver::agent_dirs(cfg, ctx) {
        for def in load_agent_files(&dir, scope) {
            if out.iter().any(|a| a.name == def.name) {
                continue;
            }
            out.push(def);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(exit_code: i32, ts: i64) -> CronRunDetail {
        CronRunDetail {
            job: "x".into(),
            repo: None,
            branch: None,
            ts,
            exit_code,
            duration_ms: 0,
            output_tail: String::new(),
        }
    }

    #[test]
    fn parse_reads_frontmatter_and_body() {
        let text = "---\nname: pr-queue\nclass: info\non_calendar: \"*:0/15\"\nwindow: planning:pr-queue\nnetwork: true\n---\nFetch the PR queue.\n";
        let a = parse_agent(text, "stem", JobScope::Global, PathBuf::from("/x.md"))
            .unwrap()
            .unwrap();
        assert_eq!(a.name, "pr-queue");
        assert_eq!(a.class, CronClass::Info);
        assert_eq!(a.on_calendar.as_deref(), Some("*:0/15"));
        assert_eq!(a.window.as_deref(), Some("planning:pr-queue"));
        assert!(a.network);
        assert!(a.reuse); // default true
        assert!(a.is_cron());
        assert!(!a.is_startup());
        assert_eq!(a.body, "Fetch the PR queue.\n");
    }

    #[test]
    fn parse_name_falls_back_to_stem_and_defaults() {
        let text = "---\non_calendar: daily\n---\nbody\n";
        let a = parse_agent(text, "meetings", JobScope::Global, PathBuf::from("/m.md"))
            .unwrap()
            .unwrap();
        assert_eq!(a.name, "meetings");
        assert_eq!(a.class, CronClass::Info); // agents default to info, not housekeeping
        assert!(a.reuse);
        assert!(!a.network);
        assert!(a.triggers.is_empty());
    }

    #[test]
    fn parse_trigger_scalar_and_list() {
        let scalar = parse_agent(
            "---\nname: boot\ntrigger: startup\n---\nb\n",
            "boot",
            JobScope::Global,
            PathBuf::from("/b.md"),
        )
        .unwrap()
        .unwrap();
        assert!(scalar.is_startup());
        assert!(!scalar.is_cron()); // no on_calendar

        let list = parse_agent(
            "---\nname: both\non_calendar: \"*:0/15\"\ntrigger: [cron, startup]\n---\nb\n",
            "both",
            JobScope::Global,
            PathBuf::from("/b.md"),
        )
        .unwrap()
        .unwrap();
        assert!(list.is_startup());
        assert!(list.is_cron());
    }

    #[test]
    fn parse_requires_frontmatter() {
        assert!(
            parse_agent("no fence\n", "x", JobScope::Global, PathBuf::from("/x.md"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn due_when_never_run_or_failed() {
        let now = 1_000_000;
        assert!(due("*:0/15", None, now), "never run → due");
        assert!(
            due("*:0/15", Some(&detail(1, now - 60)), now),
            "last run failed → due"
        );
    }

    #[test]
    fn due_interval_respects_last_success() {
        // A 15-minute interval agent that succeeded 20 minutes ago is due again; one that
        // succeeded 5 minutes ago (after the most recent :00/:15/:30/:45 slot) is not.
        let now = 1_700_000_400; // an arbitrary wall-clock instant
        let twenty_min_ago = now - 20 * 60;
        let five_min_ago = now - 5 * 60;
        assert!(due("*:0/15", Some(&detail(0, twenty_min_ago)), now));
        // The previous 15-min slot is within the last 15 minutes; a run 5 min ago is newer than it.
        let slot = crate::staff_schedule::previous_occurrence("*:0/15", now).unwrap();
        assert_eq!(
            due("*:0/15", Some(&detail(0, five_min_ago)), now),
            slot > five_min_ago
        );
    }

    #[test]
    fn due_daily_fires_once_per_slot() {
        // A daily-at-time agent whose last success predates today's slot is due; re-running
        // immediately after (last success just after the slot) is not.
        let cal = "*-*-* 05:30:00";
        let now = crate::util::now_epoch_secs();
        let Some(slot) = crate::staff_schedule::previous_occurrence(cal, now) else {
            return; // no slot within the lookback (unreachable for a daily spec)
        };
        assert!(
            due(cal, Some(&detail(0, slot - 1)), now),
            "ran before slot → due"
        );
        assert!(
            !due(cal, Some(&detail(0, slot + 1)), now),
            "ran after slot → not due"
        );
    }

    #[test]
    fn launchable_gates_scheduled_agent_by_calendar() {
        use chrono::{Local, TimeZone};
        // scout's real schedule: Saturdays at 10:00 local.
        let sat = parse_agent(
            "---\nname: scout\non_calendar: \"Sat *-*-* 10:00:00\"\n---\nb\n",
            "scout",
            JobScope::Global,
            PathBuf::from("/scout.md"),
        )
        .unwrap()
        .unwrap();
        // 2022-01-08 and 2022-01-15 are Saturdays; 2022-01-14 is the Friday between them. Built in
        // the local zone so the weekday matches what `previous_occurrence` derives from the date.
        let at = |y, m, d, h, mi, s| {
            Local
                .with_ymd_and_hms(y, m, d, h, mi, s)
                .single()
                .expect("unambiguous local time")
                .timestamp()
        };
        let ran_last_sat = detail(0, at(2022, 1, 8, 10, 0, 0));
        let friday = at(2022, 1, 14, 12, 0, 0);
        let saturday = at(2022, 1, 15, 10, 0, 1);
        // Friday: the most recent Saturday slot is last week's, already run → NOT launchable.
        assert!(
            !sat.launchable(Some(&ran_last_sat), friday),
            "Sat-only agent must not open its window on a Friday"
        );
        // Saturday after 10:00: a fresh slot has passed since last week's run → launchable.
        assert!(
            sat.launchable(Some(&ran_last_sat), saturday),
            "Sat-only agent must launch on Saturday once its slot has passed"
        );
    }

    #[test]
    fn launchable_always_true_for_unscheduled_startup_agent() {
        let boot = parse_agent(
            "---\nname: boot\ntrigger: startup\n---\nb\n",
            "boot",
            JobScope::Global,
            PathBuf::from("/boot.md"),
        )
        .unwrap()
        .unwrap();
        assert!(boot.on_calendar.is_none());
        let now = crate::util::now_epoch_secs();
        // No schedule ⇒ `bn startup` fires it regardless of run history (never-run and just-ran),
        // so gating the dispatch path on the calendar must not regress startup-class agents.
        assert!(boot.launchable(None, now));
        assert!(boot.launchable(Some(&detail(0, now - 5)), now));
    }

    #[test]
    fn placement_variants() {
        assert_eq!(
            placement(Some("planning:pr-queue"), "pr-queue", "cron"),
            Placement {
                session: "planning".into(),
                window: "pr-queue".into()
            }
        );
        assert_eq!(
            placement(Some("planning"), "meetings", "cron"),
            Placement {
                session: "planning".into(),
                window: "meetings".into()
            }
        );
        assert_eq!(
            placement(None, "sentinel", "cron"),
            Placement {
                session: "cron".into(),
                window: "sentinel".into()
            }
        );
        // A trailing colon falls back to the agent name for the window.
        assert_eq!(
            placement(Some("planning:"), "today-agent", "cron"),
            Placement {
                session: "planning".into(),
                window: "today-agent".into()
            }
        );
        // A leading colon (empty session) inherits the fallback session but keeps the window name —
        // the form agents use to follow the `[jobs].session` knob while naming their window.
        assert_eq!(
            placement(Some(":pr-queue"), "pr-queue", "cron"),
            Placement {
                session: "cron".into(),
                window: "pr-queue".into()
            }
        );
        assert_eq!(
            placement(Some(":agent-attention-list"), "attention-agent", "fleet"),
            Placement {
                session: "fleet".into(),
                window: "agent-attention-list".into()
            }
        );
    }

    #[test]
    fn shipped_agents_parse_and_have_enumerable_schedules() {
        // The ten info agents' schedules moved out of the launchd/systemd generator (guarded by
        // `cron_gen`'s `every_shipped_cron_translates_for_launchd`) and into this in-process
        // `previous_occurrence` interpreter, which now drives their `info-gather` cadence. This is
        // the runtime successor of that guard: every shipped `config/agents/<name>.md` must parse,
        // and any schedule it declares must be enumerable. An unmodeled schedule returns `None`,
        // which `due()` treats as always-due — silently firing the agent every 15 min instead of
        // on its real cadence — so we fail loudly here instead.
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/agents");
        let now = crate::util::now_epoch_secs();
        let mut scheduled = 0;
        for entry in std::fs::read_dir(dir)
            .expect("shipped config/agents/ dir exists")
            .flatten()
        {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read shipped agent");
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("agent");
            let a = parse_agent(&text, stem, JobScope::Global, path.clone())
                .unwrap_or_else(|e| panic!("shipped agent {} must parse: {e}", path.display()))
                .unwrap_or_else(|| {
                    panic!("shipped agent {} must have frontmatter", path.display())
                });
            if let Some(cal) = a.on_calendar.as_deref() {
                assert!(
                    crate::staff_schedule::previous_occurrence(cal, now).is_some(),
                    "shipped agent '{}' schedule '{cal}' is not enumerable by previous_occurrence",
                    a.name,
                );
                scheduled += 1;
            }
        }
        assert!(
            scheduled >= 10,
            "expected the ten shipped info agents to declare schedules, saw {scheduled}"
        );
    }

    #[test]
    fn every_shipped_agent_follows_the_fallback_session() {
        // The fleet's contract: with no per-agent session override, every shipped `agents/*.md`
        // lands in the resolved cron/fleet session (the `[jobs].session` knob), never a hardcoded
        // one. A `.md` that pins `window: some-session:win` (a real session before the colon) would
        // escape the knob and scatter the fleet — fail loudly here so `:win` (session-inheriting)
        // stays the norm.
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/agents");
        for entry in std::fs::read_dir(dir)
            .expect("shipped config/agents/ dir exists")
            .flatten()
        {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read shipped agent");
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("agent");
            let Some(a) = parse_agent(&text, stem, JobScope::Global, path.clone())
                .unwrap_or_else(|e| panic!("shipped agent {} must parse: {e}", path.display()))
            else {
                continue;
            };
            let place = placement(a.window.as_deref(), &a.name, "cron-sentinel");
            assert_eq!(
                place.session, "cron-sentinel",
                "shipped agent '{}' (window: {:?}) must follow the fallback session, not pin its own",
                a.name, a.window,
            );
        }
    }
}
