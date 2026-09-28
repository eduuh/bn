//! `bn handoff` — a structured session resume packet (#120). The forward-looking counterpart to
//! the note: where the note records *what is*, the handoff records *what's next* so the next
//! session (human or agent, especially in multi-agent builds) is oriented in one read instead of
//! re-deriving "where was I / what's next" from scratch.
//!
//! AI-agent memory research converges on five layers for durable continuity:
//!   1. **state snapshot** — goal + the latest progress + what's open right now,
//!   2. **narrative** — why it looks this way (a few sentences),
//!   3. **decisions** — made *and* deferred,
//!   4. **open questions** — what's unresolved,
//!   5. **priority queue** — do X first, Y second.
//!
//! This composes the layers from the existing [`Note`](crate::model::Note) — it does **not**
//! re-mine agent sessions (#96 owns that; `bn agent digest` feeds the note, which feeds here).
//! The state snapshot, the *made* decisions, the open questions, and the default priority queue
//! are read straight off the note; the narrative and *deferred* decisions are author-supplied (an
//! agent writes its own at session end), with the priority queue overridable. Persisted beside the
//! note as `handoff.json` (the authoritative round-trip) plus `handoff.md` (the human read).

use crate::model::Note;
use crate::resolver::Config;
use crate::store;
use crate::{render, resolver::Resolved};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `<note_dir>/handoff.json` — the authoritative resume packet (round-trips through serde).
pub fn json_path(cfg: &Config, ctx: &Resolved) -> PathBuf {
    cfg.note_dir(ctx).join("handoff.json")
}

/// `<note_dir>/handoff.md` — the human-readable mirror of the packet.
pub fn md_path(cfg: &Config, ctx: &Resolved) -> PathBuf {
    cfg.note_dir(ctx).join("handoff.md")
}

/// The state snapshot layer: where things stand right now, lifted from the note.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// The note's one-line goal (`## Goal`), if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// The most recent `## Progress` bullets (newest last), capped — the "you are here" line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<String>,
    /// Open todos still on the note — what's in flight.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_todos: Vec<String>,
    /// Open blockers — what's in the way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
}

/// The decision layer: choices already made and choices deliberately left for the next session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Decisions {
    /// Decisions recorded on the note — the *why it looks this way*, settled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub made: Vec<String>,
    /// Choices deferred to the next session — author-supplied (the note has no "deferred" section).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred: Vec<String>,
}

/// The full resume packet: the five layers plus the identity/stamp that date it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Handoff {
    pub repo: String,
    pub branch: String,
    /// When this packet was written (`YYYY-MM-DD`).
    pub written: String,
    /// Layer 1: where things stand.
    pub state: StateSnapshot,
    /// Layer 2: why it looks this way — a few sentences. Empty until an author writes it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub narrative: String,
    /// Layer 3: decisions made + deferred.
    pub decisions: Decisions,
    /// Layer 4: open questions — what's unresolved (seeded from the note's `## To Ask`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<String>,
    /// Layer 5: the priority queue — do these in order. Defaults to the open todos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub priority_queue: Vec<String>,
    /// Standing instructions for this branch (the note's `## Directives`).
    ///
    /// The other layers describe *what* to do; these constrain *how*. They ride in the packet
    /// because a `bn agent relaunch --hard` seeds a fresh session from it — which is precisely the
    /// moment an instruction given by hand an hour ago would otherwise be lost, since the new
    /// session starts with none of the context it was said in. `#[serde(default)]` keeps every
    /// packet written before the layer existed valid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directives: Vec<String>,
    /// The author-supplied questions alone (layer 4 minus the note's asks). `open_questions` folds
    /// both together for reading; this keeps the author's half separate so the next write can
    /// carry it forward without also carrying asks the note has since closed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_questions: Vec<String>,
    /// Whether `priority_queue` was set by an author, rather than defaulted from the open todos.
    /// Only an explicit queue is carried forward; a defaulted one is recomposed from the note.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queue_explicit: bool,
    /// When some author layer was carried forward from an earlier packet rather than written this
    /// time, the date it was first written. Lets a reader see that a narrative or deferred list is
    /// older than the `written` stamp suggests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried: Option<String>,
}

/// The author-supplied layers — what an agent writing its own handoff fills in. Everything else is
/// composed from the note. All optional: a bare `bn handoff write` snapshots the note as-is.
#[derive(Debug, Clone, Default)]
pub struct HandoffInput {
    /// The narrative (layer 2): why it looks this way.
    pub narrative: Option<String>,
    /// Decisions deferred to the next session (layer 3, the half the note can't supply).
    pub deferred: Vec<String>,
    /// Extra open questions beyond the note's `## To Ask` (layer 4).
    pub questions: Vec<String>,
    /// An explicit priority queue (layer 5). When empty, defaults to the note's open todos.
    pub priority_queue: Vec<String>,
    /// Write exactly these layers and drop whatever the previous packet's author wrote. Without
    /// it, [`write`] carries forward any author layer this input leaves empty (see [`merge`]).
    pub replace: bool,
}

/// Fill the author layers `input` leaves empty from the `prior` packet, so a write that only has
/// something to say about one layer doesn't erase the others. `bn agent relaunch --hard` is the
/// case that needs it: it writes a narrative and nothing else, and used to wipe the previous
/// session's deferred decisions, questions and explicit queue at exactly the moment the next
/// session needs them.
///
/// A non-empty field in `input` replaces the prior one; `input.replace` (or no prior packet)
/// skips the merge. Returns the effective input and, when anything was carried, the date the
/// carried layers were first written. Pure.
pub fn merge(prior: Option<&Handoff>, input: &HandoffInput) -> (HandoffInput, Option<String>) {
    let mut out = input.clone();
    let Some(prior) = prior.filter(|_| !input.replace) else {
        return (out, None);
    };
    let mut carried = false;
    let blank = input
        .narrative
        .as_deref()
        .is_none_or(|n| n.trim().is_empty());
    if blank && !prior.narrative.is_empty() {
        out.narrative = Some(prior.narrative.clone());
        carried = true;
    }
    if out.deferred.is_empty() && !prior.decisions.deferred.is_empty() {
        out.deferred = prior.decisions.deferred.clone();
        carried = true;
    }
    if out.questions.is_empty() && !prior.extra_questions.is_empty() {
        out.questions = prior.extra_questions.clone();
        carried = true;
    }
    if out.priority_queue.is_empty() && prior.queue_explicit {
        out.priority_queue = prior.priority_queue.clone();
        carried = true;
    }
    let since = carried.then(|| {
        prior
            .carried
            .clone()
            .unwrap_or_else(|| prior.written.clone())
    });
    (out, since)
}

/// How many trailing `## Progress` bullets the snapshot keeps — enough to orient, not the whole log.
const PROGRESS_TAIL: usize = 5;

/// Compose a [`Handoff`] from the note plus the author-supplied layers. Pure: reads the note value,
/// writes nothing. The state snapshot, made-decisions, open-questions and default priority queue
/// come from the note; the narrative, deferred decisions, and extra questions come from `input`.
pub fn compose(note: &Note, written: &str, input: &HandoffInput) -> Handoff {
    let open_todos: Vec<String> = note
        .front
        .todos
        .iter()
        .filter(|t| t.is_open_todo())
        .map(|t| t.text.clone())
        .collect();

    let state = StateSnapshot {
        goal: render::goal_text(&note.body),
        progress: progress_tail(&note.body),
        open_todos: open_todos.clone(),
        blockers: note.front.blockers.iter().map(|b| b.text.clone()).collect(),
    };

    let mut open_questions: Vec<String> = note.front.ask.iter().map(|a| a.text.clone()).collect();
    // An author question the note now asks itself is not "extra" any more; listing it twice is
    // how an editor pre-filled with `open_questions` would otherwise double every ask per save.
    let extra_questions: Vec<String> = input
        .questions
        .iter()
        .filter(|q| !open_questions.contains(q))
        .cloned()
        .collect();
    open_questions.extend(extra_questions.iter().cloned());

    // Priority queue: an explicit list wins; otherwise the open todos are the default ordering.
    let priority_queue = if input.priority_queue.is_empty() {
        open_todos
    } else {
        input.priority_queue.clone()
    };

    Handoff {
        repo: note.front.repo.clone(),
        branch: note.front.branch.clone(),
        written: written.to_string(),
        state,
        narrative: input.narrative.clone().unwrap_or_default(),
        decisions: Decisions {
            made: note
                .front
                .decisions
                .iter()
                .map(|d| d.text.clone())
                .collect(),
            deferred: input.deferred.clone(),
        },
        open_questions,
        priority_queue,
        directives: note
            .front
            .directives
            .iter()
            .map(|d| d.text.clone())
            .collect(),
        extra_questions,
        queue_explicit: !input.priority_queue.is_empty(),
        carried: None,
    }
}

/// The last [`PROGRESS_TAIL`] bullets under `## Progress`, oldest-first. Empty when the section is
/// absent or has no bullets.
fn progress_tail(body: &str) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    let mut in_section = false;
    for (line, code) in crate::markdown::scan(body) {
        if !code && line.starts_with("## ") {
            in_section = line == "## Progress";
            continue;
        }
        // A `- ` line inside a fenced block is sample text, not a progress bullet.
        if in_section && !code {
            if let Some(rest) = line.trim_start().strip_prefix("- ") {
                let rest = rest.trim();
                if !rest.is_empty() {
                    all.push(rest.to_string());
                }
            }
        }
    }
    let start = all.len().saturating_sub(PROGRESS_TAIL);
    all[start..].to_vec()
}

/// Compose the handoff from the note at `ctx`, carrying forward the previous packet's author layers
/// where `input` leaves them empty ([`merge`]), write `handoff.json` + `handoff.md` beside it, and
/// return the composed packet. Errors if the note can't be read or the files can't be written.
pub fn write(
    cfg: &Config,
    ctx: &Resolved,
    written: &str,
    input: &HandoffInput,
) -> Result<Handoff, std::io::Error> {
    let note = store::read(&cfg.note_file(ctx))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;
    let prior = if input.replace { None } else { read(cfg, ctx) };
    let (effective, carried) = merge(prior.as_ref(), input);
    let mut handoff = compose(&note, written, &effective);
    handoff.carried = carried;

    let dir = cfg.note_dir(ctx);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(&handoff)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    // Both artifacts are written atomically (temp + rename), not truncate-in-place: a mid-write
    // crash or a concurrent `read` can never observe a torn `handoff.json` — the authoritative,
    // sole resume-read source. The `.md` mirror gets the same guard for consistency. The two files
    // can still momentarily diverge (json new, md briefly stale) between the two renames, but that
    // is harmless — resume reads only the json, and the md is a human mirror.
    store::write_atomic(&json_path(cfg, ctx), format!("{json}\n").as_bytes())?;
    store::write_atomic(&md_path(cfg, ctx), render_markdown(&handoff).as_bytes())?;
    Ok(handoff)
}

/// Read the resume packet beside the note at `ctx`. Prefers `handoff.json` (authoritative); `None`
/// when no handoff has been written (or the JSON is unreadable — callers degrade to the note).
pub fn read(cfg: &Config, ctx: &Resolved) -> Option<Handoff> {
    read_at(&json_path(cfg, ctx))
}

/// [`read`] from an explicit path — the testable core.
pub fn read_at(json: &Path) -> Option<Handoff> {
    let text = std::fs::read_to_string(json).ok()?;
    serde_json::from_str(&text).ok()
}

/// True when the packet carries nothing worth printing — drives a quiet session-start (no packet
/// banner when there's no goal, narrative, queue, or open work to resume into).
pub fn is_empty(h: &Handoff) -> bool {
    h.state.goal.is_none()
        && h.state.progress.is_empty()
        && h.state.open_todos.is_empty()
        && h.state.blockers.is_empty()
        && h.narrative.is_empty()
        && h.decisions.made.is_empty()
        && h.decisions.deferred.is_empty()
        && h.open_questions.is_empty()
        && h.priority_queue.is_empty()
        && h.directives.is_empty()
}

/// The markdown mirror written to `handoff.md` — the five layers, read-aloud friendly.
pub fn render_markdown(h: &Handoff) -> String {
    let mut out = format!("# Handoff — {} / {}\n_{}_\n", h.repo, h.branch, h.written);
    if let Some(since) = &h.carried {
        out.push_str(&format!(
            "_Some author notes below are carried from {since}; re-write any that no longer hold._\n"
        ));
    }

    // Directives lead. They are constraints on everything below, and a session that reads them
    // after the task list has already started planning against the wrong rules.
    if !h.directives.is_empty() {
        out.push_str("\n## Standing instructions\nFollow these for all work on this branch.\n");
        for d in &h.directives {
            out.push_str(&format!("- {d}\n"));
        }
    }

    out.push_str("\n## State\n");
    match &h.state.goal {
        Some(g) => out.push_str(&format!("Goal: {g}\n")),
        None => out.push_str("Goal: (unset)\n"),
    }
    if !h.state.progress.is_empty() {
        out.push_str("\nRecent progress\n");
        for p in &h.state.progress {
            out.push_str(&format!("- {p}\n"));
        }
    }
    if !h.state.open_todos.is_empty() {
        out.push_str("\nOpen\n");
        for t in &h.state.open_todos {
            out.push_str(&format!("- [ ] {t}\n"));
        }
    }
    if !h.state.blockers.is_empty() {
        out.push_str("\nBlocked\n");
        for b in &h.state.blockers {
            out.push_str(&format!("- ! {b}\n"));
        }
    }

    if !h.narrative.is_empty() {
        out.push_str(&format!("\n## Narrative\n{}\n", h.narrative.trim_end()));
    }

    if !h.decisions.made.is_empty() || !h.decisions.deferred.is_empty() {
        out.push_str("\n## Decisions\n");
        for d in &h.decisions.made {
            out.push_str(&format!("- {d}\n"));
        }
        for d in &h.decisions.deferred {
            out.push_str(&format!("- (deferred) {d}\n"));
        }
    }

    if !h.open_questions.is_empty() {
        out.push_str("\n## Open questions\n");
        for q in &h.open_questions {
            out.push_str(&format!("- {q}\n"));
        }
    }

    if !h.priority_queue.is_empty() {
        out.push_str("\n## Do next\n");
        for (i, p) in h.priority_queue.iter().enumerate() {
            out.push_str(&format!("{}. {p}\n", i + 1));
        }
    }
    out
}

/// The session-start orientation — the one-read banner. Tighter than the full markdown: goal, the
/// top of the priority queue, blockers, and the lead open question, so the next session lands
/// oriented. Empty packet → a single "no handoff" line the caller can choose to suppress.
pub fn render_orientation(h: &Handoff) -> String {
    if is_empty(h) {
        return format!("Handoff — {} / {}: (none recorded)\n", h.repo, h.branch);
    }
    let mut out = format!("Handoff — {} / {} ({})\n", h.repo, h.branch, h.written);
    if let Some(since) = &h.carried {
        out.push_str(&format!("  (author notes carried from {since})\n"));
    }
    if let Some(g) = &h.state.goal {
        out.push_str(&format!("  Goal: {g}\n"));
    }
    // Every standing instruction, never a truncated few: a constraint you only half-see is worse
    // than one you don't see at all, because it reads as the complete set.
    for d in &h.directives {
        out.push_str(&format!("  Standing instruction: {d}\n"));
    }
    if !h.narrative.is_empty() {
        out.push_str(&format!("  {}\n", first_line(&h.narrative)));
    }
    if !h.priority_queue.is_empty() {
        out.push_str("  Do next:\n");
        for (i, p) in h.priority_queue.iter().take(3).enumerate() {
            out.push_str(&format!("    {}. {p}\n", i + 1));
        }
    }
    if !h.state.blockers.is_empty() {
        out.push_str("  Blocked:\n");
        for b in &h.state.blockers {
            out.push_str(&format!("    ! {b}\n"));
        }
    }
    if let Some(q) = h.open_questions.first() {
        out.push_str(&format!("  Open question: {q}\n"));
    }
    out
}

/// The first non-blank line of a possibly-multiline string (for the orientation's one-line narrative).
fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, Note};
    use crate::resolver::{RepoBranch, Resolved};

    fn note() -> Note {
        Note::new(
            RepoBranch {
                repo: "repo",
                branch: "feat",
            },
            "2026-09-01",
            "host",
        )
    }

    fn item(id: &str, text: &str) -> Item {
        serde_json::from_value(serde_json::json!({"id": id, "text": text, "created": "2026-09-01"}))
            .expect("item")
    }

    fn authored() -> HandoffInput {
        HandoffInput {
            narrative: Some("why it looks this way".into()),
            deferred: vec!["pick a storage engine".into()],
            questions: vec!["is the cache shared?".into()],
            priority_queue: vec!["land A0".into()],
            replace: false,
        }
    }

    fn packet(input: &HandoffInput, written: &str) -> Handoff {
        compose(&note(), written, input)
    }

    #[test]
    fn a_narrative_only_write_keeps_the_other_author_layers() {
        // The `relaunch --hard` shape: a narrative and nothing else.
        let prior = packet(&authored(), "2026-09-20");
        let relaunch = HandoffInput {
            narrative: Some("relaunched: context wall".into()),
            ..Default::default()
        };
        let (eff, carried) = merge(Some(&prior), &relaunch);
        assert_eq!(eff.narrative.as_deref(), Some("relaunched: context wall"));
        assert_eq!(eff.deferred, vec!["pick a storage engine"]);
        assert_eq!(eff.questions, vec!["is the cache shared?"]);
        assert_eq!(eff.priority_queue, vec!["land A0"]);
        assert_eq!(carried.as_deref(), Some("2026-09-20"));
    }

    #[test]
    fn replace_drops_the_prior_author_layers() {
        let prior = packet(&authored(), "2026-09-20");
        let input = HandoffInput {
            replace: true,
            ..Default::default()
        };
        let (eff, carried) = merge(Some(&prior), &input);
        assert!(eff.narrative.is_none() && eff.deferred.is_empty() && eff.questions.is_empty());
        assert!(eff.priority_queue.is_empty());
        assert_eq!(carried, None);
    }

    #[test]
    fn a_defaulted_queue_is_recomposed_not_carried() {
        let prior = packet(&HandoffInput::default(), "2026-09-20");
        assert!(!prior.queue_explicit);
        let (eff, carried) = merge(Some(&prior), &HandoffInput::default());
        assert!(eff.priority_queue.is_empty());
        assert_eq!(carried, None, "nothing authored, nothing carried");
    }

    #[test]
    fn the_carried_date_is_the_oldest_write_not_the_last() {
        let mut prior = packet(&authored(), "2026-09-20");
        prior.carried = Some("2026-09-10".into());
        let (_, carried) = merge(Some(&prior), &HandoffInput::default());
        assert_eq!(carried.as_deref(), Some("2026-09-10"));
    }

    #[test]
    fn an_author_question_the_note_now_asks_is_not_listed_twice() {
        let mut n = note();
        n.front.ask.push(item("a1", "is the cache shared?"));
        let h = compose(&n, "2026-09-20", &authored());
        assert_eq!(h.open_questions, vec!["is the cache shared?"]);
        assert!(h.extra_questions.is_empty());
    }

    #[test]
    fn a_packet_written_before_the_new_fields_still_loads() {
        let old = r#"{"repo":"r","branch":"b","written":"2026-01-01","state":{},
            "decisions":{"deferred":["x"]},"open_questions":["q"]}"#;
        let h: Handoff = serde_json::from_str(old).expect("old packet parses");
        assert!(h.extra_questions.is_empty() && !h.queue_explicit && h.carried.is_none());
        // Its deferred list still carries forward.
        let (eff, _) = merge(Some(&h), &HandoffInput::default());
        assert_eq!(eff.deferred, vec!["x"]);
    }

    #[test]
    fn write_carries_forward_through_the_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cfg = crate::test_support::config(temp.path());
        let ctx = Resolved {
            repo: "repo".into(),
            branch: "feat".into(),
        };
        std::fs::create_dir_all(cfg.note_dir(&ctx)).expect("note dir");
        store::save(&note(), &cfg.note_file(&ctx)).expect("save note");

        write(&cfg, &ctx, "2026-09-20", &authored()).expect("first write");
        let relaunch = HandoffInput {
            narrative: Some("relaunched".into()),
            ..Default::default()
        };
        let h = write(&cfg, &ctx, "2026-09-24", &relaunch).expect("second write");
        assert_eq!(h.decisions.deferred, vec!["pick a storage engine"]);
        assert_eq!(h.carried.as_deref(), Some("2026-09-20"));
        assert!(render_orientation(&h).contains("carried from 2026-09-20"));
        assert_eq!(read(&cfg, &ctx).expect("read back"), h);
    }
}
