//! Item verbs: `bn add <section>` (plus its `--est` / `--q` variants) and
//! `bn done <id|substring>`.

use super::error::{emit_metric, CmdError};
use crate::id::next_id;
use crate::model::{Item, Note, Section};
use crate::resolver::{Config, Resolved};
use crate::store;
use serde_json::json;

/// Result of `add`: the new item's section, id, text, and frontmatter line. When
/// `skipped` is true an `if_absent` key already matched an existing item, so nothing
/// was written and `id`/`line` point at that pre-existing item.
#[derive(Debug)]
pub struct AddOutcome {
    pub section: Section,
    pub id: String,
    pub text: String,
    pub line: usize,
    pub skipped: bool,
}

/// Result of `done`: the completed todo's id and text.
#[derive(Debug)]
pub struct DoneOutcome {
    pub id: String,
    pub text: String,
    /// Recorded partial progress (1–99) when the item was advanced rather than closed
    /// (`bn done --pct N`, #213); `None` means it completed.
    pub pct: Option<u8>,
    /// Actual duration stamped from the punch log at close (calibration), when the todo carried
    /// an `est` and this branch's punch intervals summed to more than zero — else `None`.
    pub actual: Option<String>,
}

/// `bn add <section> <text>` — append an item with a fresh id, persist, return its line.
/// Creates the note (single-file) if it doesn't exist yet (ensure-note).
pub fn add(
    cfg: &Config,
    ctx: &Resolved,
    section_kw: &str,
    text: &str,
    if_absent: Option<&str>,
    today: &str,
) -> Result<AddOutcome, CmdError> {
    add_full(cfg, ctx, section_kw, text, if_absent, None, None, today)
}

/// [`add`] with an optional `--est` estimate (a `bn focus start` duration, validated by the
/// caller) stamped onto the new item — the estimate half of calibration (#245). `est = None` is
/// the plain `add` behaviour, so every existing caller is unchanged.
#[allow(clippy::too_many_arguments)]
pub fn add_est(
    cfg: &Config,
    ctx: &Resolved,
    section_kw: &str,
    text: &str,
    if_absent: Option<&str>,
    est: Option<&str>,
    today: &str,
) -> Result<AddOutcome, CmdError> {
    add_full(cfg, ctx, section_kw, text, if_absent, est, None, today)
}

/// Like [`add`], but tags the new item with a quarter-goal key (`bn add todo … --q <key>`),
/// linking shipped work to a `bn quarter` goal so the impact agent can score it. `q` rides on
/// the item's optional `q` field (omitted from YAML when absent), round-tripping through parsers.
#[allow(clippy::too_many_arguments)]
pub fn add_tagged(
    cfg: &Config,
    ctx: &Resolved,
    section_kw: &str,
    text: &str,
    if_absent: Option<&str>,
    q: Option<&str>,
    today: &str,
) -> Result<AddOutcome, CmdError> {
    add_full(cfg, ctx, section_kw, text, if_absent, None, q, today)
}

/// Shared implementation for [`add`], [`add_est`], and [`add_tagged`]: appends an item,
/// optionally stamping an `est` estimate (calibration, #245) and/or a `q` quarter-goal key
/// (#252). Both default to `None`, so the plain `add` path is unchanged.
#[allow(clippy::too_many_arguments)]
pub fn add_full(
    cfg: &Config,
    ctx: &Resolved,
    section_kw: &str,
    text: &str,
    if_absent: Option<&str>,
    est: Option<&str>,
    q: Option<&str>,
    today: &str,
) -> Result<AddOutcome, CmdError> {
    let section = Section::from_keyword(section_kw)
        .ok_or_else(|| CmdError::UnknownSection(section_kw.into()))?;
    let text = text.trim();
    if text.is_empty() {
        return Err(CmdError::EmptyText);
    }

    let note_md = cfg.note_file(ctx);
    // Serialize this read-modify-write against a concurrent writer on the same note (atomic save
    // prevents corruption; this prevents a lost update when two processes edit at once).
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = if note_md.exists() {
        store::read(&note_md)?
    } else {
        Note::new(ctx.rb(), today, &cfg.hostname)
    };

    // Idempotent add: if any existing item in this section (open or done) already
    // contains the dedup key, report it as skipped and write nothing.
    if let Some(key) = if_absent.map(str::trim).filter(|k| !k.is_empty()) {
        if let Some(existing) = note
            .front
            .items(section)
            .iter()
            .find(|it| it.text.contains(key))
        {
            return Ok(AddOutcome {
                section,
                id: existing.id.clone(),
                text: text.to_string(),
                line: 0,
                skipped: true,
            });
        }
    }

    let id = next_id(note.front.items(section), section.id_prefix());
    let item = Item {
        id: id.clone(),
        text: text.to_string(),
        created: today.to_string(),
        done: if section.tracks_done() {
            Some(false)
        } else {
            None
        },
        done_at: None,
        link: None,
        pct: None,
        est: est.map(str::to_string),
        actual: None,
        q: q.map(str::to_string),
        notes: Vec::new(),
    };
    note.front.items_mut(section).push(item);
    note.front.record_session();
    store::save(&note, &note_md)?;
    emit_metric(
        cfg,
        ctx,
        "activity",
        vec![
            ("action", json!("add")),
            ("section", json!(section_kw)),
            ("item", json!(id)),
        ],
    );

    let rendered = store::render(&note)?;
    let line = store::item_line_map(&rendered)
        .get(&id)
        .copied()
        .unwrap_or(0);
    Ok(AddOutcome {
        section,
        id,
        text: text.to_string(),
        line,
        skipped: false,
    })
}

/// `bn done <id|substring>` — mark a todo done (todos only). An `t<N>` query is an
/// explicit id lookup; otherwise a case-insensitive substring over OPEN todo text,
/// erroring on zero or multiple matches. `pct` (1–99) records partial progress instead of
/// closing — the item stays open with its percentage visible (#213); 100+ completes as usual.
pub fn done(
    cfg: &Config,
    ctx: &Resolved,
    query: &str,
    today: &str,
    pct: Option<u8>,
) -> Result<DoneOutcome, CmdError> {
    let note_md = cfg.note_file(ctx);
    if !note_md.exists() {
        return Err(CmdError::NoNote);
    }
    // Guard the read-modify-write against a concurrent writer on the same note.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = store::read(&note_md)?;
    let q = query.trim();

    // Resolve which checkable section + index the query points at. An explicit id picks the
    // section by prefix (`t…` todo, `w…` done-when); a substring search scans the open items of
    // both, so `bn done <text>` checks off a todo or a criterion alike.
    let (section, idx) = if let Some(sec) = section_for_done_id(q) {
        let pos = note
            .front
            .items(sec)
            .iter()
            .position(|t| t.id == q)
            .ok_or_else(|| CmdError::NotFound(q.to_string()))?;
        (sec, pos)
    } else {
        let ql = q.to_lowercase();
        let mut matches: Vec<(Section, usize, String)> = Vec::new();
        for sec in [Section::Todo, Section::DoneWhen] {
            for (i, it) in note.front.items(sec).iter().enumerate() {
                if it.is_open_todo() && it.text.to_lowercase().contains(&ql) {
                    matches.push((sec, i, it.id.clone()));
                }
            }
        }
        match matches.len() {
            0 => return Err(CmdError::NotFound(q.to_string())),
            1 => (matches[0].0, matches[0].1),
            _ => {
                let ids = matches.into_iter().map(|(_, _, id)| id).collect();
                return Err(CmdError::Ambiguous(q.to_string(), ids));
            }
        }
    };

    let item = &mut note.front.items_mut(section)[idx];
    if item.done == Some(true) {
        return Err(CmdError::AlreadyDone(item.id.clone()));
    }
    // A sub-100 pct advances the item instead of closing it; 100 (or more) means done.
    let partial = pct.filter(|&p| p < 100);
    let actual = match partial {
        Some(p) => {
            item.pct = Some(p.max(1));
            None
        }
        None => {
            item.done = Some(true);
            item.done_at = Some(today.to_string());
            // done is 100% by definition — keep the YAML clean
            item.pct = None;
            // Calibration: if the todo carried an estimate, stamp the actual summed from this
            // branch's punch log over its open window. Skips cleanly when the log had no time.
            stamp_todo_actual(cfg, ctx, item, today)
        }
    };
    let id = item.id.clone();
    let text = item.text.clone();
    note.front.record_session();
    store::save(&note, &note_md)?;
    let action = if partial.is_some() {
        "progress"
    } else {
        "done"
    };
    let mut fields = vec![("action", json!(action)), ("item", json!(id))];
    if let Some(p) = partial {
        fields.push(("pct", json!(p)));
    }
    emit_metric(cfg, ctx, "activity", fields);
    Ok(DoneOutcome {
        id,
        text,
        pct: partial,
        actual,
    })
}

/// Stamp `actual:` on a just-closed todo from the punch log, returning the stamped value. A
/// no-op (returns `None`) unless the todo carries an `est` and this branch's intervals over
/// `[created..=today]` summed to more than zero — an empty window records nothing (#245).
fn stamp_todo_actual(cfg: &Config, ctx: &Resolved, item: &mut Item, today: &str) -> Option<String> {
    let _est = item.est.as_deref().and_then(crate::focus::parse_duration)?;
    let now = crate::util::now_epoch_secs();
    let secs =
        crate::calib::todo_actual_secs(cfg, &ctx.repo, &ctx.branch, &item.created, today, now);
    if secs <= 0 {
        return None;
    }
    let actual = crate::track::fmt_dur(secs);
    item.actual = Some(actual.clone());
    Some(actual)
}

/// The checkable section an explicit id targets: `t<digits>` → todos, `w<digits>` → done-when.
/// `None` for anything else (a substring query), so the caller falls back to text search.
fn section_for_done_id(q: &str) -> Option<Section> {
    let (prefix, rest) = q.split_at(q.chars().next().map(char::len_utf8).unwrap_or(0));
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match prefix {
        "t" => Some(Section::Todo),
        "w" => Some(Section::DoneWhen),
        _ => None,
    }
}
