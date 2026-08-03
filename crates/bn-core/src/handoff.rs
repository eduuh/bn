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
    open_questions.extend(input.questions.iter().cloned());

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

/// Compose the handoff from the note at `ctx`, write `handoff.json` + `handoff.md` beside it, and
/// return the composed packet. Errors if the note can't be read or the files can't be written.
pub fn write(
    cfg: &Config,
    ctx: &Resolved,
    written: &str,
    input: &HandoffInput,
) -> Result<Handoff, std::io::Error> {
    let note = store::read(&cfg.note_file(ctx))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;
    let handoff = compose(&note, written, input);

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
}

/// The markdown mirror written to `handoff.md` — the five layers, read-aloud friendly.
pub fn render_markdown(h: &Handoff) -> String {
    let mut out = format!("# Handoff — {} / {}\n_{}_\n", h.repo, h.branch, h.written);

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
    if let Some(g) = &h.state.goal {
        out.push_str(&format!("  Goal: {g}\n"));
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
    use crate::model::Item;
    use crate::resolver::RepoBranch;
    use crate::test_support::config_with_hostname;

    fn item(id: &str, text: &str, done: Option<bool>) -> Item {
        Item {
            id: id.into(),
            text: text.into(),
            created: "2026-06-26".into(),
            done,
            done_at: None,
            link: None,
            pct: None,
            est: None,
            actual: None,
            q: None,
            notes: Vec::new(),
        }
    }

    /// A note with a goal, progress prose, open + done todos, a blocker, a decision, and an ask.
    fn sample_note() -> Note {
        let mut note = Note::new(RepoBranch::new("bn", "feat-handoff"), "2026-06-26", "host");
        note.body =
            "# bn / feat-handoff\n\n## Goal\n\nShip the handoff packet.\n\n## Progress\n\n- scaffolded module\n- wrote compose()\n- wired the CLI"
                .into();
        note.front.todos = vec![
            item("t1", "wire the session-start hook", Some(false)),
            item("t2", "add MCP tools", Some(false)),
            item("t3", "already done", Some(true)),
        ];
        note.front.blockers = vec![item("b1", "waiting on review of #120", None)];
        note.front.decisions = vec![item("d1", "compose from the note, no #96 re-mining", None)];
        note.front.ask = vec![item("a1", "should read run in the hook by default?", None)];
        note
    }

    fn cfg(root: &Path) -> Config {
        config_with_hostname(root, "host")
    }

    #[test]
    fn compose_lifts_the_five_layers_from_the_note() {
        let note = sample_note();
        let h = compose(&note, "2026-06-27", &HandoffInput::default());

        // 1. state snapshot
        assert_eq!(h.state.goal.as_deref(), Some("Ship the handoff packet."));
        assert_eq!(h.state.progress.len(), 3);
        assert_eq!(h.state.progress.last().unwrap(), "wired the CLI");
        // open todos only — the done one is excluded
        assert_eq!(
            h.state.open_todos,
            vec!["wire the session-start hook", "add MCP tools"]
        );
        assert_eq!(h.state.blockers, vec!["waiting on review of #120"]);
        // 3. decisions made come from the note; deferred empty without input
        assert_eq!(
            h.decisions.made,
            vec!["compose from the note, no #96 re-mining"]
        );
        assert!(h.decisions.deferred.is_empty());
        // 4. open questions seeded from the note's asks
        assert_eq!(
            h.open_questions,
            vec!["should read run in the hook by default?"]
        );
        // 5. priority queue defaults to the open todos, in order
        assert_eq!(
            h.priority_queue,
            vec!["wire the session-start hook", "add MCP tools"]
        );
    }

    #[test]
    fn author_layers_override_and_extend() {
        let note = sample_note();
        let input = HandoffInput {
            narrative: Some("Composing from the note keeps a single source of truth.".into()),
            deferred: vec!["whether to push the handoff on close".into()],
            questions: vec!["does nvim need a panel?".into()],
            priority_queue: vec!["land the gate".into(), "open the PR".into()],
        };
        let h = compose(&note, "2026-06-27", &input);

        assert!(h.narrative.starts_with("Composing from the note"));
        assert_eq!(
            h.decisions.deferred,
            vec!["whether to push the handoff on close"]
        );
        // note asks + the extra question
        assert_eq!(h.open_questions.len(), 2);
        assert_eq!(h.open_questions[1], "does nvim need a panel?");
        // explicit queue wins over the open-todo default
        assert_eq!(h.priority_queue, vec!["land the gate", "open the PR"]);
    }

    #[test]
    fn progress_tail_keeps_only_the_last_five() {
        let body = "## Progress\n\n- a\n- b\n- c\n- d\n- e\n- f\n- g\n";
        let tail = progress_tail(body);
        assert_eq!(tail, vec!["c", "d", "e", "f", "g"]);
    }

    #[test]
    fn write_then_read_round_trips_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let ctx = Resolved {
            repo: "bn".into(),
            branch: "feat-handoff".into(),
        };
        // Seed the note on disk so write() can read it.
        store::save(&sample_note(), &c.note_file(&ctx)).unwrap();

        let input = HandoffInput {
            narrative: Some("why".into()),
            deferred: vec!["later".into()],
            ..Default::default()
        };
        let written = write(&c, &ctx, "2026-06-27", &input).unwrap();

        // both artifacts exist
        assert!(json_path(&c, &ctx).exists());
        assert!(md_path(&c, &ctx).exists());

        // read() reconstructs the exact packet from the JSON
        let back = read(&c, &ctx).expect("handoff.json reads back");
        assert_eq!(back, written);
        assert_eq!(back.narrative, "why");
        assert_eq!(back.decisions.deferred, vec!["later"]);

        // markdown mirror carries the layers
        let md = std::fs::read_to_string(md_path(&c, &ctx)).unwrap();
        assert!(md.contains("## State"));
        assert!(md.contains("Goal: Ship the handoff packet."));
        assert!(md.contains("## Do next"));
        assert!(md.contains("1. wire the session-start hook"));
    }

    #[test]
    fn read_is_none_when_no_handoff_written() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let ctx = Resolved {
            repo: "bn".into(),
            branch: "feat-handoff".into(),
        };
        assert!(read(&c, &ctx).is_none());
    }

    #[test]
    fn orientation_is_tight_and_ordered() {
        let note = sample_note();
        let input = HandoffInput {
            narrative: Some("First line of the why.\nSecond line ignored.".into()),
            ..Default::default()
        };
        let h = compose(&note, "2026-06-27", &input);
        let o = render_orientation(&h);
        assert!(o.contains("Goal: Ship the handoff packet."));
        assert!(o.contains("First line of the why."));
        assert!(!o.contains("Second line ignored."));
        assert!(o.contains("1. wire the session-start hook"));
        assert!(o.contains("! waiting on review of #120"));
        assert!(o.contains("Open question: should read run in the hook by default?"));
    }

    #[test]
    fn empty_handoff_is_detected_and_renders_a_none_line() {
        let blank = Note::new(RepoBranch::new("bn", "main"), "2026-06-26", "host");
        let h = compose(&blank, "2026-06-27", &HandoffInput::default());
        assert!(is_empty(&h));
        assert!(render_orientation(&h).contains("(none recorded)"));
    }

    #[test]
    fn write_replaces_handoff_json_atomically_never_torn() {
        // handoff.json is the sole resume-read source, so `write` replaces it atomically (temp +
        // rename), not truncate-in-place: a reader hammering read() while writers churn always
        // parses a complete packet, and no `.tmp` scratch survives. The old non-atomic dual write
        // could leave the authoritative json observed torn mid-write (issue #162, item #5).
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let ctx = Resolved {
            repo: "bn".into(),
            branch: "feat-handoff".into(),
        };
        store::save(&sample_note(), &c.note_file(&ctx)).unwrap();
        // Seed one handoff so the reader has a file from its very first iteration.
        write(&c, &ctx, "2026-06-27", &HandoffInput::default()).unwrap();

        let json = json_path(&c, &ctx);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (json, stop) = (json, stop.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    // A torn (half-written) json fails to parse → None; atomic replace never tears.
                    read_at(&json).expect("reader never sees a torn handoff.json");
                }
            })
        };
        for i in 0..300 {
            let input = HandoffInput {
                narrative: Some(format!("iteration {i}")),
                ..Default::default()
            };
            write(&c, &ctx, "2026-06-27", &input).unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();

        // No scratch temp files left behind beside the artifacts.
        let leftover: Vec<_> = std::fs::read_dir(c.note_dir(&ctx))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftover.is_empty(),
            "atomic write leaked temp files: {leftover:?}"
        );
    }
}
