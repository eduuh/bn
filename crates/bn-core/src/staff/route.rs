//! Where a validated card's items actually land.
//!
//! One function, five sinks, and the rules that keep a re-derived card from piling up: a keyed
//! attention item refreshes rather than duplicates, a `replace` card closes what it stopped
//! reporting, a goal already on today's list is turned away, and a note append happens once.
//!
//! Its own module beside [`super::card`] because the two answer opposite halves of the same
//! question — that one decides whether a card may be believed, this one decides what believing it
//! does — and neither should have to be read to change the other.
use super::*;

// --- routing -------------------------------------------------------------------------------------

/// How many items each sink received when a card was routed.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct RouteSummary {
    pub attn: usize,
    pub daily: usize,
    pub note: usize,
    pub research: usize,
    pub dojo: usize,
    /// Documents written into the agent's `doc_dir`.
    pub doc: usize,
    /// Entries appended to the learnings log.
    pub learn: usize,
    /// Goal items the daily sink refused: a duplicate of one already on today's list, or a goal
    /// hanging off a merged/closed PR (#966). Counted rather than silently dropped so a card that
    /// keeps re-raising the same ping is visible in the run report.
    pub skipped: usize,
    /// Previously-keyed attention items this card acked by no longer reporting them — the
    /// `replace` half. Zero on a card that does not set `replace`.
    pub resolved: usize,
}

/// Whether a card's goal item should be turned away by the daily sink, and why. The staff agents
/// re-derive their cards from scratch every run, so without this an unactioned "ping reviewers on
/// wira360#923" is re-added every morning and every evening, and keeps being re-added after the PR
/// is closed — three copies of a dead goal carried forever.
///
/// `existing` is today's goal list; `dead` the merged/closed PRs ([`crate::daily::dead_prs`]).
/// Pure so the policy is testable without a store.
pub fn goal_skip_reason(
    title: &str,
    link: Option<&str>,
    existing: &[crate::model::Item],
    dead: &std::collections::HashSet<(String, i64)>,
) -> Option<&'static str> {
    if link.is_some_and(|l| crate::pr_observe::link_is_terminal(dead, l)) {
        return Some("PR already merged/closed");
    }
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let title_norm = norm(title);
    // A PR link is the strong identity (two agents word the same ping differently); the
    // normalized text catches unlinked repeats.
    let dup = existing
        .iter()
        .any(|g| (link.is_some() && g.link.as_deref() == link) || norm(&g.text) == title_norm);
    dup.then_some("already on today's list")
}

/// The bullet a `note-append` writes: its title, its key as an inline marker, then any `body`
/// indented beneath.
///
/// The marker is what makes the append idempotent. `append_body_once` decides whether to write by
/// searching the section for the key — so the key has to *be there*, or the guard can never fire
/// and a note re-derived every morning grows a duplicate a day. An HTML comment because a note is
/// read by people, and machinery a person has to read around is machinery in the wrong place.
fn note_bullet(item: &CardItem) -> String {
    let mut out = item.title.clone();
    if !item.key.is_empty() {
        out.push_str(&format!("  <!-- {} -->", item.key));
    }
    for line in item.body.lines() {
        out.push_str("\n  ");
        out.push_str(line);
    }
    out
}

/// Route a validated card's items through the existing channels — attention (keyed, so re-routes
/// are idempotent and `bn next` / the ATTENTION panel pick them up with zero new plumbing), daily
/// goals, note todo/research sections (`--if-absent` semantics), note Progress appends, and the
/// #253 dojo deck (a `Quiz` [`crate::dojo::Card`] appended to `<machine>/dojo/cards.jsonl`, deduped
/// by stable id so re-routes don't pile up). The card must have passed [`validate_card`], so every
/// kind/sink here is known and allowlisted.
pub fn route_card(
    cfg: &Config,
    card: &BriefingCard,
    doc_dir: Option<&str>,
    today: &str,
) -> Result<RouteSummary, String> {
    let source = job_name(&card.agent);
    let mut sum = RouteSummary::default();
    let mut raised_keys: std::collections::HashSet<String> = Default::default();
    // One read of the merged/closed PR set for the whole card (it's a scan of the metrics logs),
    // and only when a goal item is actually present.
    let dead_prs = if card.items.iter().any(|it| it.kind == "goal") {
        crate::daily::dead_prs(cfg)
    } else {
        Default::default()
    };
    for (i, item) in card.items.iter().enumerate() {
        let link = (!item.link.is_empty()).then_some(item.link.as_str());
        let note_ctx = || Resolved {
            repo: item.repo.clone(),
            branch: item.branch.clone(),
        };
        let fail = |e: &dyn std::fmt::Display| format!("item {i} ({}): {e}", item.kind);
        let fail_cmd =
            |e: crate::commands::CmdError| format!("item {i} ({}): {}", item.kind, e.message());
        match item.kind.as_str() {
            "attn" => {
                // An agent-chosen `key` names the *subject* and so survives rewording, which is
                // what makes both refresh-in-place and `replace` work. Without one the item falls
                // back to a positional key that is unique to this run — fine for a card that
                // reports events, useless for one that re-derives a current set.
                let key = if item.key.is_empty() {
                    format!("staff:{}:{}:{i}", card.agent, card.date)
                } else {
                    crate::attention::ingest_key(&source, &item.key)
                };
                // Backpressure (#269): shed stale info items to keep this source under its cap,
                // then admit the item at a severity that honors the agent's daily urgent budget.
                crate::staff_backpressure::enforce_source_cap(&source, today);
                let severity = crate::staff_backpressure::urgent_budget_severity(
                    &card.agent,
                    &item.severity,
                    today,
                );
                crate::attention::add_new(
                    &crate::attention::NewAttn {
                        source: &source,
                        title: &item.title,
                        severity: &severity,
                        repo: (!item.repo.is_empty()).then_some(item.repo.as_str()),
                        branch: (!item.branch.is_empty()).then_some(item.branch.as_str()),
                        link,
                        key: Some(&key),
                        detail: (!item.detail.is_empty()).then_some(item.detail.as_str()),
                        mode: (!item.mode.is_empty()).then_some(item.mode.as_str()),
                        prompt: (!item.prompt.is_empty()).then_some(item.prompt.as_str()),
                        ..Default::default()
                    },
                    today,
                )
                .map_err(|e| fail(&e))?;
                raised_keys.insert(key);
                sum.attn += 1;
            }
            "goal" => {
                // Read today's list fresh for every goal item so a duplicate *within* one card is
                // caught too (the AM card has raised the same PR ping twice in a single run).
                let existing = crate::daily::ensure_today(cfg, today)
                    .map(|d| d.front.goals)
                    .unwrap_or_default();
                // A goal for a future day is checked against what is already queued for that day,
                // not against today's list — an evening card re-run must not add tomorrow's first
                // task twice, and it is not a duplicate of anything on today's.
                let scheduled = (!item.on.is_empty()).then_some(item.on.as_str());
                let dup = match scheduled {
                    Some(date) => crate::daily::scheduled_pending(cfg, today)
                        .into_iter()
                        .any(|s| {
                            s.scheduled == date
                                && (s.text == item.title
                                    || (link.is_some() && s.link.as_deref() == link))
                        })
                        .then_some("already scheduled for that day"),
                    None => goal_skip_reason(&item.title, link, &existing, &dead_prs),
                };
                if let Some(why) = dup {
                    eprintln!("staff route: skipped goal \"{}\" — {why}", item.title);
                    sum.skipped += 1;
                    continue;
                }
                crate::daily::add(cfg, today, &item.title, link.map(str::to_string), scheduled)
                    .map_err(fail_cmd)?;
                sum.daily += 1;
            }
            "todo" => {
                crate::commands::add(
                    cfg,
                    &note_ctx(),
                    "todo",
                    &item.title,
                    Some(&item.title),
                    today,
                )
                .map_err(fail_cmd)?;
                sum.note += 1;
            }
            "research" => {
                crate::commands::add(
                    cfg,
                    &note_ctx(),
                    "research",
                    &item.title,
                    Some(&item.title),
                    today,
                )
                .map_err(fail_cmd)?;
                sum.research += 1;
            }
            "note-append" => {
                // Keyed appends are idempotent: an agent that re-derives the same craft note every
                // morning writes it once, not once a day. Unkeyed appends stay plain appends.
                //
                // The note must already exist. An agent appends *about* a branch it did not
                // choose — the packet named it — and every prompt that emits this kind already
                // says "only when there is an obvious active branch note". Creating one would
                // mint an empty note for a branch that was never worked on this machine.
                let section = if item.section.is_empty() {
                    "progress"
                } else {
                    item.section.as_str()
                };
                let (_, written) = crate::commands::append_body_once(
                    cfg,
                    &note_ctx(),
                    section,
                    &note_bullet(item),
                    (!item.key.is_empty()).then_some(item.key.as_str()),
                    true,
                    today,
                )
                .map_err(fail_cmd)?;
                if written {
                    sum.note += 1;
                } else {
                    sum.skipped += 1;
                }
            }
            "dojo-card" => {
                // Land it in the #253 deck as a Quiz card. `source` is stable so append_cards'
                // id-dedup makes re-routing the same item idempotent, matching the other sinks.
                // A staff card carries no answer key, so the link (or a self-study note) stands in.
                let repo = if item.repo.is_empty() {
                    card.agent.clone()
                } else {
                    item.repo.clone()
                };
                let answer = if item.link.is_empty() {
                    "(self-study — staff-routed card, no answer key)".to_string()
                } else {
                    item.link.clone()
                };
                // The deck's identity is its `source`. Keyed on the *title*, a card whose text
                // cites this week's PR mints a brand-new card every week instead of reinforcing
                // the one card for that theme — which is the entire point of a spaced deck. An
                // agent-supplied `key` names the subject, so prefer it.
                let source = if item.key.is_empty() {
                    format!("staff:{}:{}", card.agent, item.title)
                } else {
                    format!("staff:{}:{}", card.agent, item.key)
                };
                let dojo_card = crate::dojo::Card::new(
                    repo,
                    crate::dojo::CardKind::Quiz,
                    (!item.area.is_empty()).then(|| item.area.clone()),
                    item.title.clone(),
                    answer,
                    source,
                    crate::util::now_epoch_secs(),
                );
                crate::dojo::DojoStore::from_config(cfg)
                    .append_cards(&[dojo_card])
                    .map_err(|e| fail(&e))?;
                sum.dojo += 1;
            }
            "learning" => {
                // The learnings log, not a note: it is queried across repos and time
                // (`bn learned`, `bn recall`), which a file in a directory would not be.
                // Idempotent by key — a weekly plan re-derived on Sunday must not log a second
                // copy of the same topic.
                let prefix = (!item.key.is_empty()).then_some(item.key.as_str());
                if prefix.is_some_and(|k| {
                    crate::learnings::all_entries(cfg, today)
                        .iter()
                        .any(|e| e.text.starts_with(k))
                }) {
                    sum.skipped += 1;
                    continue;
                }
                let text = match prefix {
                    Some(k) => format!("{k} {}", item.title),
                    None => item.title.clone(),
                };
                crate::daily::learned(
                    cfg,
                    today,
                    &text,
                    &crate::daily::LogProvenance {
                        repo: (!item.repo.is_empty()).then(|| item.repo.clone()),
                        ..Default::default()
                    },
                )
                .map_err(fail_cmd)?;
                sum.learn += 1;
            }
            "doc" => {
                // The manifest's directory, the card's filename. `validate_card` has already
                // refused a key that could climb out of it, so the join below cannot escape.
                let Some(dir) = doc_dir else {
                    return Err(format!(
                        "item {i} (doc): agent '{}' declares the doc sink but no doc_dir",
                        card.agent
                    ));
                };
                let dir = cfg.personal_root.join(cfg.machine()).join(dir);
                std::fs::create_dir_all(&dir).map_err(|e| fail(&e))?;
                let name = if item.key.ends_with(".md") {
                    item.key.clone()
                } else {
                    format!("{}.md", item.key)
                };
                std::fs::write(dir.join(name), &item.body).map_err(|e| fail(&e))?;
                sum.doc += 1;
            }
            // validate_card admits only the kinds above.
            other => return Err(format!("item {i}: unknown kind '{other}'")),
        }
    }
    // The `replace` contract: this card stated the agent's whole current set, so a keyed item it
    // used to raise and no longer does has been fixed.
    //
    // An empty card is the strongest form of that statement, not an exception to it: the night an
    // estate patrol finds nothing wrong is exactly the night every standing finding should close.
    // Guarding this on a non-empty key set meant a clean sweep resolved nothing and the headline's
    // "N resolved" was a claim the store never honoured. A run that *fails* never reaches here —
    // it returns before routing — so the empty card that arrives is a real answer.
    if card.replace {
        sum.resolved = crate::attention::resolve_absent(&source, &raised_keys, today)
            .map_err(|e| format!("replace: {e}"))?;
    }
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::{note_bullet, CardItem};

    /// The regression the marker exists for: `append_body_once` decides whether to append by
    /// searching the section for the key, so a bullet that does not *contain* the key can never be
    /// recognised as already written — and a note re-derived every morning grows a duplicate a day.
    #[test]
    fn a_keyed_bullet_carries_its_own_key() {
        let bullet = note_bullet(&CardItem {
            title: "Mentor craft note — #12".into(),
            key: "mentor:bn#12".into(),
            ..Default::default()
        });

        assert!(
            bullet.contains("mentor:bn#12"),
            "the key must survive into the written text, got: {bullet}"
        );
    }

    /// `title` is single-line sanitized, so a note with four things to say has to say the other
    /// three in `body` — indented under the title, not flattened into it.
    #[test]
    fn the_body_is_indented_under_the_title() {
        let bullet = note_bullet(&CardItem {
            title: "Craft note".into(),
            body: "Stronger design: x\nIdiom: y".into(),
            ..Default::default()
        });

        let lines: Vec<&str> = bullet.lines().collect();
        assert_eq!(
            lines.len(),
            3,
            "one title line and two body lines: {bullet}"
        );
        assert_eq!(lines[1], "  Stronger design: x");
        assert_eq!(lines[2], "  Idiom: y");
    }

    #[test]
    fn an_unkeyed_bullet_is_left_alone() {
        assert_eq!(
            note_bullet(&CardItem {
                title: "plain".into(),
                ..Default::default()
            }),
            "plain"
        );
    }
}
