//! The Mentor (#258) — post-merge review-to-teach on your *own* merged PRs.
//!
//! The difference between ten years of experience and one year ten times is feedback on real
//! work. The Mentor is a staff engineer whose only job is to teach: *after* a PR merges (so it
//! never gates, never nags, and can be fully honest) it reviews the diff you already shipped.
//!
//! Its material is discovered **offline** from the `pr_merged` events the `pr-observe` job already
//! folds into the metrics log — only the diff fetch touches the network. Per PR the harness must
//! satisfy a four-section **review-to-teach contract** (enforced at [`validate_output`]):
//!
//! 1. **The stronger design** — the approach a staff engineer might have taken (or "this was the
//!    right shape", a valid and expected answer — but never *empty*).
//! 2. **One idiom** — a concrete, smaller-scale technique worth stealing, with a code sketch.
//! 3. **One genuine strength** — what to keep doing, specific to the diff.
//! 4. **A recurring-theme tag** — a stable slug ([`theme_slug`]) so patterns track across weeks.
//!
//! A validated review lands three private places, none of them public: a **craft note** appended
//! to the merged branch's note (idempotent per PR — [`craft_marker`]), a **dojo deck card** per
//! theme (behind the `[dojo]` feature check — feed-only until it is on), and one **info item per
//! PR** on a staff briefing card in the feed.
//!
//! This module is the pure `Packet → Review → outputs` core (selection/cap/overflow, contract
//! validation, theme slug, and the craft-note / dojo-card / feed-card builders): no IO, fully
//! unit-testable. The CLI (`bn mentor`, the `staff-mentor` cron) does the IO around it — reading
//! events, fetching diffs via `gh`, appending to notes, writing the deck + feed.

use serde::{Deserialize, Serialize};

/// The Mentor's agent name — its feed card is filed under this and the craft-note marker carries
/// it. Kept in one place so the CLI, the skill, and the cron entry agree.
pub const AGENT: &str = "mentor";

/// PRs reviewed per run: "3 PRs/day, largest-diff first; overflow noted" (#258). A day rarely
/// merges more of *your own* work than this, and each review is a full dispatch — the cap keeps a
/// burst day from ballooning the run.
pub const DEFAULT_PR_CAP: usize = 3;

/// Longest a theme slug may be — a stable, filename/id-safe handle, not prose.
pub const MAX_SLUG_CHARS: usize = 48;

// --- offline discovery ---------------------------------------------------------------------------

/// A candidate merged PR discovered offline from the metrics log — the material before any network
/// fetch. `repo` is the gh `owner/repo` slug the `pr_merged` event carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedRef {
    pub repo: String,
    pub number: i64,
    /// GitHub merge time (epoch secs), as stamped on the `pr_merged` event.
    pub merged_ts: i64,
}

/// Discover *your* PRs merged on `reviewed_date` (local `YYYY-MM-DD`) from the metrics log's
/// `pr_merged` events. Offline and deterministic: the `pr-observe` job only ever logs your own
/// PRs, so no author filter is needed here; a PR is kept when the local date of its merge matches
/// `reviewed_date`. De-duplicated by `(repo, number)` (a re-observed merge is the same PR) and
/// returned newest-merge-first, then by repo/number, for a stable order independent of log order.
pub fn discover_merged(events: &[crate::events::Event], reviewed_date: &str) -> Vec<MergedRef> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<MergedRef> = Vec::new();
    for e in events {
        if e.kind != "pr_merged" {
            continue;
        }
        let (Some(repo), Some(number)) = (
            e.fields.get("repo").and_then(|v| v.as_str()),
            e.fields.get("number").and_then(|v| v.as_i64()),
        ) else {
            continue;
        };
        if crate::util::local_date_of(e.ts) != reviewed_date {
            continue;
        }
        if !seen.insert((repo.to_string(), number)) {
            continue;
        }
        out.push(MergedRef {
            repo: repo.to_string(),
            number,
            merged_ts: e.ts,
        });
    }
    out.sort_by(|a, b| {
        b.merged_ts
            .cmp(&a.merged_ts)
            .then(a.repo.cmp(&b.repo))
            .then(a.number.cmp(&b.number))
    });
    out
}

// --- the gathered packet -------------------------------------------------------------------------

/// One PR's material — the harness's entire world for that review. The `description` and `diff`
/// are untrusted external content (framed as data at dispatch, #272); `goal`/`decisions` are the
/// branch-note "why" a post-hoc reviewer never has.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrPacket {
    /// gh `owner/repo` slug (the trusted routing key — the harness echo is never used for it).
    pub repo: String,
    pub number: i64,
    /// The PR's head branch (`headRefName`) — where the craft note lands.
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub diff: String,
    /// Diff size in bytes — the largest-first ordering key (see [`select_and_cap`]). Not presented
    /// to the model as guidance; just the deterministic cap order.
    #[serde(default)]
    pub diff_bytes: usize,
    /// The branch-note one-line goal, when the note is on this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// The branch-note decisions (the "why"), when present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    /// Verify-gate history for the branch (did it flake, was it re-run) — best-effort; empty when
    /// no such record exists on this machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verify_history: Vec<String>,
}

/// The deterministic packet a run gathers — the only input the review dispatch sees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentorPacket {
    pub agent: String,
    /// The run date (`today`, `YYYY-MM-DD`).
    pub date: String,
    /// The merge day being reviewed (`yesterday`).
    pub reviewed_date: String,
    /// The PRs to review, capped and largest-diff-first.
    pub prs: Vec<PrPacket>,
    /// PRs merged that day beyond the cap (dropped, but *noted* — never silently truncated).
    pub overflow: usize,
    /// Total PRs gathered before the cap.
    pub total_merged: usize,
}

impl MentorPacket {
    /// The `(repo, number)` of every PR in the packet — the set [`validate_output`] must cover.
    pub fn expected(&self) -> Vec<(String, i64)> {
        self.prs
            .iter()
            .map(|p| (p.repo.clone(), p.number))
            .collect()
    }

    /// Serialize to the JSON the harness receives.
    pub fn to_json(&self) -> std::io::Result<String> {
        crate::store::to_json_pretty(self)
    }
}

/// Order PRs largest-diff-first and cap the count, reporting the overflow. Deterministic: ties
/// break by `(repo, number)` so the same day always yields the same three. Returns `(kept,
/// overflow)` where `overflow = len - cap` when the day merged more than the cap.
pub fn select_and_cap(mut prs: Vec<PrPacket>, cap: usize) -> (Vec<PrPacket>, usize) {
    prs.sort_by(|a, b| {
        b.diff_bytes
            .cmp(&a.diff_bytes)
            .then(a.repo.cmp(&b.repo))
            .then(a.number.cmp(&b.number))
    });
    let overflow = prs.len().saturating_sub(cap);
    prs.truncate(cap);
    (prs, overflow)
}

/// Assemble the packet from the fetched PRs: capture the pre-cap total, then order + cap.
pub fn build_packet(
    date: &str,
    reviewed_date: &str,
    prs: Vec<PrPacket>,
    cap: usize,
) -> MentorPacket {
    let total_merged = prs.len();
    let (prs, overflow) = select_and_cap(prs, cap);
    MentorPacket {
        agent: AGENT.to_string(),
        date: date.to_string(),
        reviewed_date: reviewed_date.to_string(),
        prs,
        overflow,
        total_merged,
    }
}

// --- theme slug ----------------------------------------------------------------------------------

/// A stable, filename/id-safe slug for a recurring theme: lowercase ASCII-alphanumeric runs joined
/// by single hyphens, every other character a separator (`"Error Paths!"`, `"error paths"`, and
/// `"error-paths"` all collapse to `error-paths`). Deterministic across runs and bn versions, so
/// the same theme reinforces one deck card week over week. Capped at [`MAX_SLUG_CHARS`]; an
/// empty/symbol-only input becomes `general` (a theme is required, but never let it be blank).
pub fn theme_slug(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let capped: String = trimmed.chars().take(MAX_SLUG_CHARS).collect();
    let capped = capped.trim_matches('-');
    if capped.is_empty() {
        "general".to_string()
    } else {
        capped.to_string()
    }
}

// --- the review-to-teach contract ----------------------------------------------------------------

/// One raw per-PR review as parsed from the harness JSON — untrusted, unvalidated. Only the four
/// teaching sections are read from it; the routing keys (`repo`/`number`/`branch`) always come
/// from the trusted packet, never the harness echo.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawReview {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub number: i64,
    #[serde(default)]
    pub stronger_design: String,
    #[serde(default)]
    pub idiom: String,
    #[serde(default)]
    pub strength: String,
    #[serde(default)]
    pub theme: String,
}

/// The harness's whole output envelope: an optional headline and one review per PR.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MentorOutput {
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub reviews: Vec<RawReview>,
}

/// A validated, sanitized per-PR review — the contract-satisfying artifact routed to the note,
/// deck, and feed. Its `repo`/`number`/`branch` are the trusted packet's, its four sections the
/// harness's (escape-stripped and length-capped).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrReview {
    pub repo: String,
    pub number: i64,
    pub branch: String,
    pub stronger_design: String,
    pub idiom: String,
    pub strength: String,
    /// The recurring-theme slug (stable across runs).
    pub theme: String,
}

/// Harness output usually wraps the JSON in prose / code fences — take the outermost `{...}` span.
fn extract_json(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end > start).then(|| &raw[start..=end])
}

/// Validate + sanitize one raw review against the four-section contract, keyed to its trusted
/// packet. Each section is escape-stripped *before* the emptiness test (so an all-escapes section
/// reads as missing, not as smuggled control bytes) and must be non-empty; the theme is required
/// and reduced to a stable [`theme_slug`]. The error names the first missing section (#258:
/// "Validate rejects reviews missing any of the four contract sections").
pub fn validate_review(expected: &PrPacket, raw: &RawReview) -> Result<PrReview, String> {
    let ctx = |m: &str| format!("PR {}#{}: {m}", expected.repo, expected.number);
    let section = |label: &str, value: &str| -> Result<String, String> {
        let clean = crate::staff::sanitize_body(value);
        let trimmed = clean.trim();
        if trimmed.is_empty() {
            Err(ctx(&format!("missing the '{label}' section")))
        } else {
            Ok(trimmed.to_string())
        }
    };
    let stronger_design = section("stronger design", &raw.stronger_design)?;
    let idiom = section("idiom", &raw.idiom)?;
    let strength = section("genuine strength", &raw.strength)?;
    let theme_clean = crate::staff::sanitize_line(&raw.theme);
    if theme_clean.trim().is_empty() {
        return Err(ctx("missing the 'recurring-theme' tag"));
    }
    Ok(PrReview {
        repo: expected.repo.clone(),
        number: expected.number,
        branch: expected.branch.clone(),
        stronger_design,
        idiom,
        strength,
        theme: theme_slug(&theme_clean),
    })
}

/// Match a raw review to a packet PR: prefer an exact `(repo, number)` (case-insensitive, bare or
/// slug), else fall back to the number alone (the harness sometimes normalizes the repo). `None`
/// when neither matches.
fn match_review<'a>(reviews: &'a [RawReview], pr: &PrPacket) -> Option<&'a RawReview> {
    let bare = |s: &str| s.rsplit('/').next().unwrap_or(s).to_ascii_lowercase();
    reviews
        .iter()
        .find(|r| {
            r.number == pr.number
                && (r.repo.eq_ignore_ascii_case(&pr.repo) || bare(&r.repo) == bare(&pr.repo))
        })
        .or_else(|| reviews.iter().find(|r| r.number == pr.number))
}

/// Parse + validate the whole harness output against the contract for `packet`. Every PR in the
/// packet must have a review that satisfies all four sections; a missing review or a missing
/// section fails the run (nothing lands). Reviews are returned in packet order. Extra reviews for
/// PRs not in the packet are ignored (the harness cannot invent routing targets).
pub fn validate_output(raw: &str, packet: &MentorPacket) -> Result<Vec<PrReview>, String> {
    let json = extract_json(raw).ok_or("output contains no JSON object")?;
    let out: MentorOutput =
        serde_json::from_str(json).map_err(|e| format!("not a mentor review: {e}"))?;
    if out.reviews.is_empty() {
        return Err("output contained no reviews".to_string());
    }
    let mut reviews = Vec::with_capacity(packet.prs.len());
    for pr in &packet.prs {
        let raw_review = match_review(&out.reviews, pr)
            .ok_or_else(|| format!("PR {}#{} has no review in the output", pr.repo, pr.number))?;
        reviews.push(validate_review(pr, raw_review)?);
    }
    Ok(reviews)
}

// --- craft note (→ the merged branch's note) -----------------------------------------------------

/// The bare repo name (path segment) for a `owner/repo` slug, lowercased — the key notes and the
/// dojo deck are stored under.
pub fn bare_repo(slug: &str) -> String {
    slug.rsplit('/').next().unwrap_or(slug).to_ascii_lowercase()
}

/// A stable idempotency marker (an HTML comment) embedded on a craft note's first line, so a
/// re-run detects an existing note for this PR and never appends a duplicate.
pub fn craft_marker(repo: &str, number: i64) -> String {
    format!("<!-- mentor:{}#{number} -->", bare_repo(repo))
}

/// Whether a note section already carries this PR's craft note (idempotency check over the section
/// lines a caller read back from the note).
pub fn note_has_craft(section_lines: &[String], marker: &str) -> bool {
    section_lines.iter().any(|l| l.contains(marker))
}

/// Indent every line after the first by `pad`, so a multi-line section nests cleanly under the
/// craft-note bullet.
fn indent_continuation(s: &str, pad: &str) -> String {
    s.replace('\n', &format!("\n{pad}"))
}

/// The craft-note body appended (as a single markdown bullet — the caller's `append_body` adds the
/// leading `- `) to the merged branch's note. First line carries the [`craft_marker`]; the four
/// sections nest beneath it. Deterministic; the sections are already sanitized by
/// [`validate_review`].
pub fn craft_note(r: &PrReview) -> String {
    let pad = "  ";
    let sec = |label: &str, body: &str| format!("{pad}{label}: {}", indent_continuation(body, pad));
    format!(
        "Mentor craft note — #{} · theme `{}`  {}\n{}\n{}\n{}",
        r.number,
        r.theme,
        craft_marker(&r.repo, r.number),
        sec("Stronger design", &r.stronger_design),
        sec("Idiom", &r.idiom),
        sec("Strength", &r.strength),
    )
}

// --- dojo deck card (→ the #253 learning deck) ---------------------------------------------------

/// Build the dojo deck card for a review's recurring theme — a `Quiz` whose stable id (via
/// [`crate::dojo::Card::new`], keyed on `repo + kind + source`) makes re-runs idempotent and makes
/// the *same* theme in the *same* repo **reinforce one card** week over week rather than pile up.
/// So your actual weaknesses (the themes that keep recurring) drive your drills.
pub fn dojo_card(r: &PrReview, created: i64) -> crate::dojo::Card {
    let repo = bare_repo(&r.repo);
    let prompt = format!(
        "🧭 Recurring theme — {repo}: `{}`\n\nSeen again in #{} ({}). Q: what is the pattern behind this theme, and how will you avoid it next time?",
        r.theme, r.number, r.branch
    );
    let answer = format!(
        "The stronger design: {}\n\nAn idiom to steal: {}",
        r.stronger_design, r.idiom
    );
    crate::dojo::Card::new(
        repo,
        crate::dojo::CardKind::Quiz,
        Some(r.theme.clone()),
        prompt,
        answer,
        format!("mentor:theme:{}", r.theme),
        created,
    )
}

// --- feed briefing card (→ bn staff feed) --------------------------------------------------------

/// The staff briefing card mirrored to the feed: a headline, a human `body_md` carrying the full
/// reviews, and one info item per reviewed PR (with the overflow noted in the headline when the
/// cap dropped PRs). All human-facing strings are already sanitized (packet fields via the CLI,
/// review sections via [`validate_review`]).
pub fn feed_card(packet: &MentorPacket, reviews: &[PrReview]) -> crate::staff::BriefingCard {
    let n = reviews.len();
    let mut headline = format!(
        "Mentor: {n} PR{} reviewed from {}",
        if n == 1 { "" } else { "s" },
        packet.reviewed_date
    );
    if packet.overflow > 0 {
        headline.push_str(&format!(
            " ({} more merged that day, over the {}-PR cap)",
            packet.overflow, DEFAULT_PR_CAP
        ));
    }

    let mut body = String::new();
    for r in reviews {
        body.push_str(&format!(
            "## {}#{} — theme `{}`\n\n- **Stronger design:** {}\n- **Idiom:** {}\n- **Strength:** {}\n\n",
            r.repo, r.number, r.theme, r.stronger_design, r.idiom, r.strength
        ));
    }
    if packet.overflow > 0 {
        body.push_str(&format!(
            "_{} further PR(s) merged {} were not reviewed this run (largest-diff-first cap)._\n",
            packet.overflow, packet.reviewed_date
        ));
    }

    let items = reviews
        .iter()
        .map(|r| {
            let pr = packet
                .prs
                .iter()
                .find(|p| p.number == r.number && p.repo == r.repo);
            crate::staff::CardItem {
                kind: "review".to_string(),
                severity: "info".to_string(),
                title: format!("#{} — {} (theme `{}`)", r.number, r.repo, r.theme),
                link: pr.map(|p| p.url.clone()).unwrap_or_default(),
                repo: r.repo.clone(),
                branch: r.branch.clone(),
            }
        })
        .collect();

    crate::staff::BriefingCard {
        agent: AGENT.to_string(),
        date: packet.date.clone(),
        headline: crate::staff::sanitize_line(&headline),
        body_md: crate::staff::sanitize_body(&body),
        items,
    }
}


