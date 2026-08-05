//! The Curriculum Director (#260) — the Sunday learning plan.
//!
//! Most studying is disconnected from actual gaps: people learn what's trendy, not what they got
//! wrong on Tuesday. This agent closes the loop. The Mentor's recurring-theme tags and the #253
//! dojo mastery map say *where you're weak*; the Curriculum Director turns that into **one topic a
//! week with the single best resource**, drills scheduled, a learning note seeded.
//!
//! Split of concerns (mirroring [`crate::chief`]): this module is the pure
//! `Packet → Plan → Card` core — no IO, fully unit-testable. The CLI (`bn curriculum`, the
//! `staff-curriculum` native job) gathers the packet from the live stores, applies the plan's
//! idempotent side-effects (the Monday daily goal, the seeded learn note, the dojo drills), and
//! mirrors the card to the staff feed.
//!
//! Degraded mode. The Mentor staff agent that would emit recurring-theme feed cards is not yet
//! shipped (epic #266), so the theme signal is best-effort: absent themes simply fall through to
//! the dojo mastery signal, and — when even that is thin — to a deterministic tech-inventory
//! fallback ("you work in Rust daily; this week: lifetimes in async contexts"). The habit matters
//! more than perfect targeting, so a card always ships.

use serde::{Deserialize, Serialize};

use crate::dojo::WeakArea;
use crate::staff::{BriefingCard, CardItem};

/// The stable prefix of the Monday learning goal — parsed back out of prior cards to recover the
/// topic they proposed (see [`prior_topic`]).
pub const GOAL_PREFIX: &str = "curriculum: ";
/// The stable suffix of the Monday learning goal (a fixed 30-minute daily box).
pub const GOAL_SUFFIX: &str = " — 30m";
/// A theme flagged at least this many times in the week counts as a real (rich) signal.
pub const THEME_SIGNAL_MIN: u32 = 2;
/// Once a topic has ridden this many consecutive weekly cards unfinished it is a zombie the
/// two-carry rule drops (3 cards = proposed once, carried twice — a month must never pass).
pub const CARRY_DROP_AT: u32 = 3;
/// Total time-box for the week's resource, in minutes (≤ 2h).
pub const RESOURCE_MINUTES: u32 = 120;

// --- the gathered packet ------------------------------------------------------------------------

/// How a theme's count moved against the prior week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trend {
    /// Not seen in the prior week.
    New,
    /// Up on the prior week.
    Rising,
    /// Unchanged.
    Steady,
    /// Down on the prior week.
    Falling,
}

/// The trend of `count` (this week) against `prior` (last week).
pub fn trend_of(count: u32, prior: u32) -> Trend {
    match (count, prior) {
        (_, 0) => Trend::New,
        (c, p) if c > p => Trend::Rising,
        (c, p) if c < p => Trend::Falling,
        _ => Trend::Steady,
    }
}

/// One recurring-theme tag from the Mentor's week (degraded to learnings themes until the Mentor
/// ships), with this-week and prior-week counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeTag {
    /// The tag / theme label (e.g. `error-paths`).
    pub tag: String,
    /// Times flagged this week.
    pub count: u32,
    /// Times flagged the prior week.
    pub prior: u32,
}

impl ThemeTag {
    /// The trend of this tag against the prior week.
    pub fn trend(&self) -> Trend {
        trend_of(self.count, self.prior)
    }
}

/// One line of the deterministic tech inventory — a language and the repos it dominates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TechFact {
    /// Language label (e.g. `Rust`).
    pub language: String,
    /// Repos where this language leads, most-representative first.
    pub repos: Vec<String>,
    /// Source files counted for this language across the scan (the weight).
    pub files: u32,
}

/// A prior Curriculum card, flattened to what continuity needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorCard {
    /// The card's date (`YYYY-MM-DD`).
    pub date: String,
    /// The topic it proposed (parsed from its Monday goal).
    pub topic: String,
    /// Whether that topic's Monday goal was later marked done — the completion signal.
    pub completed: bool,
}

/// Last week's honest verdict on the prior topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The prior topic's Monday goal was marked done.
    Completed,
    /// Unfinished and still within the carry budget — carried into this week.
    Carried,
    /// Unfinished and over the carry budget — the two-carry rule drops it (a zombie).
    Dropped,
}

impl Verdict {
    /// The lowercase label used in the card body.
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Completed => "completed",
            Verdict::Carried => "carried",
            Verdict::Dropped => "dropped",
        }
    }
}

/// The continuity read over the last few Curriculum cards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Continuity {
    /// The most recent prior topic, if any.
    pub prior_topic: Option<String>,
    /// Last week's verdict on it, if any.
    pub verdict: Option<Verdict>,
    /// How many consecutive most-recent cards share the prior topic (`0` when there are none).
    pub carry_count: u32,
    /// This week must finish-with-smaller-scope-or-drop the prior topic (unfinished, carried at
    /// least once).
    pub must_resolve: bool,
}

/// Whether the week's signals are strong enough to target, or thin enough to fall back on habit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Signal {
    /// A real theme or dojo weak-spot to target.
    Rich,
    /// Nothing strong — fall back on the tech inventory to keep the habit.
    Thin,
}

/// The deterministic packet gathered for a run. Every source past `date`/`machine` is optional (an
/// empty vec) so a missing source degrades the plan rather than failing it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Packet {
    /// Run date (`YYYY-MM-DD`, the Sunday).
    pub date: String,
    /// The machine the plan is for.
    pub machine: String,
    /// Mentor recurring-theme tags with counts + prior-week counts (degraded to learnings themes).
    pub themes: Vec<ThemeTag>,
    /// The weak / never-drilled area selected by dojo's canonical mastery policy.
    pub weak_area: Option<WeakArea>,
    /// The tech inventory, most-used language first.
    pub tech: Vec<TechFact>,
    /// Topics already covered by existing learn notes — so the plan builds on, not duplicates them.
    pub existing_notes: Vec<String>,
    /// The last few Curriculum cards, newest first.
    pub prior_cards: Vec<PriorCard>,
    /// The continuity read (derived from `prior_cards`).
    pub continuity: Continuity,
    /// Whether the signal is rich or thin (derived).
    pub signal: Signal,
}

impl Packet {
    /// Assemble a packet from its gathered sources, deriving [`continuity`](Packet::continuity) and
    /// [`signal`](Packet::signal). Themes are sorted by descending count; dojo supplies its
    /// precomputed weak-area selection.
    pub fn assemble(
        date: impl Into<String>,
        machine: impl Into<String>,
        mut themes: Vec<ThemeTag>,
        weak_area: Option<WeakArea>,
        tech: Vec<TechFact>,
        existing_notes: Vec<String>,
        prior_cards: Vec<PriorCard>,
    ) -> Packet {
        themes.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
        let continuity = classify_continuity(&prior_cards);
        let signal = signal_of(&themes, weak_area.as_ref());
        Packet {
            date: date.into(),
            machine: machine.into(),
            themes,
            weak_area,
            tech,
            existing_notes,
            prior_cards,
            continuity,
            signal,
        }
    }

    /// The packet as pretty JSON — the agent-facing gather output (`bn curriculum gather`).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// --- continuity + signal (pure) -----------------------------------------------------------------

/// The continuity read over `prior_cards` (newest first). The `carry_count` is how many
/// consecutive most-recent cards share the newest card's topic; the verdict on that topic is
/// [`Completed`](Verdict::Completed) when its goal was done, [`Dropped`](Verdict::Dropped) once it
/// has ridden [`CARRY_DROP_AT`] cards unfinished (the two-carry rule), else
/// [`Carried`](Verdict::Carried). `must_resolve` flags an unfinished topic carried at least once —
/// this week must finish it with a smaller scope or drop it.
pub fn classify_continuity(prior_cards: &[PriorCard]) -> Continuity {
    let Some(last) = prior_cards.first() else {
        return Continuity {
            prior_topic: None,
            verdict: None,
            carry_count: 0,
            must_resolve: false,
        };
    };
    let carry_count = prior_cards
        .iter()
        .take_while(|c| c.topic == last.topic)
        .count() as u32;
    let verdict = if last.completed {
        Verdict::Completed
    } else if carry_count >= CARRY_DROP_AT {
        Verdict::Dropped
    } else {
        Verdict::Carried
    };
    let must_resolve = !last.completed && carry_count >= 2;
    Continuity {
        prior_topic: Some(last.topic.clone()),
        verdict: Some(verdict),
        carry_count,
        must_resolve,
    }
}

/// Whether the week's signals are rich (a real theme or a concrete dojo weak-spot) or thin.
pub fn signal_of(themes: &[ThemeTag], weak_area: Option<&WeakArea>) -> Signal {
    let rich_theme = themes.iter().any(|t| t.count >= THEME_SIGNAL_MIN);
    if rich_theme || weak_area.is_some() {
        Signal::Rich
    } else {
        Signal::Thin
    }
}

/// The deterministic fallback topic from the tech inventory — the dominant language mapped to a
/// concrete, slightly-stretching subtopic. `None` when the inventory is empty.
pub fn fallback_topic(tech: &[TechFact]) -> Option<(String, String)> {
    let lead = tech.first()?;
    let sub = fallback_subtopic(&lead.language);
    let repo = lead.repos.first().cloned().unwrap_or_default();
    Some((format!("{}: {sub}", lead.language), repo))
}

/// A canned, slightly-advanced subtopic for a language — deterministic so the fallback is stable.
fn fallback_subtopic(language: &str) -> &'static str {
    match language.to_ascii_lowercase().as_str() {
        "rust" => "lifetimes in async contexts",
        "typescript" | "javascript" => "structural typing & narrowing edge cases",
        "python" => "async iterators & context managers",
        "go" => "goroutine lifecycle & context cancellation",
        "c" | "c++" | "cpp" => "ownership discipline without a borrow checker",
        "java" | "kotlin" => "the memory model & happens-before",
        "ruby" => "metaprogramming without foot-guns",
        "shell" | "bash" => "safe quoting & failure modes (set -euo pipefail)",
        _ => "one deep fundamentals chapter",
    }
}

// --- topic selection + plan (pure) --------------------------------------------------------------

/// Where the chosen topic's evidence came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TopicSource {
    /// A prior topic being carried / finished this week.
    Continuation,
    /// A Mentor recurring-theme tag.
    Theme,
    /// A dojo weak / never-drilled area.
    Dojo,
    /// The tech-inventory habit fallback.
    Fallback,
}

/// The topic chosen for the week, with its evidence and the repo/area it hangs off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenTopic {
    /// The one topic.
    pub topic: String,
    /// The evidence sentence tying it to the week's signals.
    pub why: String,
    /// What kind of signal chose it.
    pub source: TopicSource,
    /// Home repo for the drills / resource note (may be empty in the pure-habit case).
    pub repo: String,
    /// Dojo area the topic exercises, when it came from one.
    pub area: Option<String>,
}

/// Choose the one topic for the week from the packet, honoring continuity first.
///
/// Order: a [`Carried`](Verdict::Carried) prior topic is continued (finished with a smaller scope
/// when `must_resolve`); otherwise the strongest fresh signal wins — a real theme, then a dojo
/// weak-spot, then the tech fallback, then a bare habit topic. A just-[`Dropped`](Verdict::Dropped)
/// or [`Completed`](Verdict::Completed) prior topic is never re-picked as the fresh choice.
pub fn select_topic(packet: &Packet) -> ChosenTopic {
    // 1) Continuity — carry (and maybe narrow) an unfinished prior topic.
    if packet.continuity.verdict == Some(Verdict::Carried) {
        if let Some(topic) = &packet.continuity.prior_topic {
            let repo = topic_repo(packet, topic);
            let why = if packet.continuity.must_resolve {
                format!(
                    "carried {}× and still open — finish it this week with a smaller scope or drop it",
                    packet.continuity.carry_count
                )
            } else {
                "continuing last week's topic — it isn't done yet".to_string()
            };
            return ChosenTopic {
                topic: topic.clone(),
                why,
                source: TopicSource::Continuation,
                repo,
                area: dojo_area_for(packet, topic),
            };
        }
    }

    let dropped = (packet.continuity.verdict == Some(Verdict::Dropped))
        .then(|| packet.continuity.prior_topic.clone())
        .flatten();

    // 2) A rich theme.
    if let Some(theme) = packet
        .themes
        .iter()
        .find(|t| t.count >= THEME_SIGNAL_MIN && Some(&t.tag) != dropped.as_ref())
    {
        let trend = match theme.trend() {
            Trend::New => "new this week",
            Trend::Rising => "rising",
            Trend::Steady => "steady",
            Trend::Falling => "easing but still recurring",
        };
        return ChosenTopic {
            topic: theme.tag.clone(),
            why: format!(
                "recurring theme `{}` — flagged {}× this week ({trend}, was {}×)",
                theme.tag, theme.count, theme.prior
            ),
            source: TopicSource::Theme,
            repo: topic_repo(packet, &theme.tag),
            area: None,
        };
    }

    // 3) A dojo weak-spot.
    if let Some(w) = packet
        .weak_area
        .as_ref()
        .filter(|w| Some(&w.area) != dropped.as_ref())
    {
        let why = if w.reviewed == 0 {
            format!(
                "never drilled on {}/{} ({} card{} mined, 0 reviewed)",
                w.repo,
                w.area,
                w.cards,
                plural(w.cards)
            )
        } else {
            format!(
                "dojo weak-spot {}/{} — {} lapse{} across {} reviewed card{}",
                w.repo,
                w.area,
                w.lapses,
                plural(w.lapses),
                w.reviewed,
                plural(w.reviewed)
            )
        };
        return ChosenTopic {
            topic: w.area.clone(),
            why,
            source: TopicSource::Dojo,
            repo: w.repo.clone(),
            area: Some(w.area.clone()),
        };
    }

    // 4) Tech-inventory fallback.
    if let Some((topic, repo)) = fallback_topic(&packet.tech) {
        let lang = packet
            .tech
            .first()
            .map(|t| t.language.as_str())
            .unwrap_or("your stack");
        return ChosenTopic {
            topic,
            why: format!(
                "thin signal this week — you work in {lang} daily, so keep the habit sharp"
            ),
            source: TopicSource::Fallback,
            repo,
            area: None,
        };
    }

    // 5) Bare habit — nothing to go on at all.
    ChosenTopic {
        topic: "reading one deep engineering essay".to_string(),
        why: "no themes, dojo or tech signal yet — the habit matters more than the target"
            .to_string(),
        source: TopicSource::Fallback,
        repo: String::new(),
        area: None,
    }
}

/// The repo to hang a topic off: an exact dojo-area match, else the leading tech repo.
fn topic_repo(packet: &Packet, topic: &str) -> String {
    if let Some(w) = packet.weak_area.as_ref().filter(|w| w.area == topic) {
        return w.repo.clone();
    }
    packet
        .tech
        .first()
        .and_then(|t| t.repos.first())
        .cloned()
        .unwrap_or_default()
}

/// The dojo area label for a topic that matches one, else `None`.
fn dojo_area_for(packet: &Packet, topic: &str) -> Option<String> {
    packet
        .weak_area
        .as_ref()
        .filter(|w| w.area == topic)
        .map(|w| w.area.clone())
}

fn plural(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The one best resource for the week — a single article/paper/doc chapter, time-boxed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    /// The resource title.
    pub title: String,
    /// Its URL (empty for a self-study fallback).
    pub url: String,
    /// A one-line summary of what it covers.
    pub summary: String,
    /// A fallback pointer if the primary is paywalled/unavailable.
    pub fallback: Option<String>,
}

/// The deterministic self-study resource for a topic — the reliable floor when no harness-backed
/// finder fills in a real article. Nothing is fetched: it is a prompt to find one yourself.
pub fn default_resource(topic: &str) -> Resource {
    Resource {
        title: format!("Self-study: {topic}"),
        url: String::new(),
        summary: format!(
            "No external resource selected (deterministic fallback). Spend up to {}m finding and \
             reading one strong article/doc chapter on {topic}.",
            RESOURCE_MINUTES
        ),
        fallback: None,
    }
}

/// One scheduled practice drill for the week's topic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drill {
    /// The drill prompt.
    pub prompt: String,
}

/// The 2–3 deterministic drills for a topic — stable text so their dojo-card ids stay stable and
/// re-seeding the same week is idempotent.
pub fn drills_for(topic: &str, repo: &str) -> Vec<Drill> {
    let where_ = if repo.is_empty() {
        "your work".to_string()
    } else {
        repo.to_string()
    };
    vec![
        Drill {
            prompt: format!("Explain the core idea of {topic} in 2–3 sentences."),
        },
        Drill {
            prompt: format!("Where in {where_} does {topic} apply? Name one concrete case."),
        },
        Drill {
            prompt: format!("What's a common mistake with {topic}, and how do you avoid it?"),
        },
    ]
}

/// The fully-resolved weekly plan — everything the card render and the idempotent side-effects
/// need. Pure: [`build_plan`] derives it, [`build_card`] renders it, the CLI applies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Run date (the Sunday).
    pub date: String,
    /// The Monday the goal is scheduled for.
    pub monday: String,
    /// The one chosen topic + its evidence.
    pub chosen: ChosenTopic,
    /// The one resource.
    pub resource: Resource,
    /// The scheduled drills.
    pub drills: Vec<Drill>,
    /// The continuity read carried onto the card.
    pub continuity: Continuity,
    /// Whether the signal was rich or thin.
    pub signal: Signal,
}

impl Plan {
    /// The Monday learning goal text (`curriculum: <topic> — 30m`).
    pub fn goal_title(&self) -> String {
        format!("{GOAL_PREFIX}{}{GOAL_SUFFIX}", self.chosen.topic)
    }

    /// The seed text for the learn note — the topic, why, resource and drill outline in one line
    /// so it lands as a single `bn learned` entry.
    pub fn learn_text(&self) -> String {
        let mut s = format!("curriculum: {} — {}", self.chosen.topic, self.chosen.why);
        if !self.resource.url.is_empty() {
            s.push_str(&format!(
                ". Resource: {} ({})",
                self.resource.title, self.resource.url
            ));
        } else {
            s.push_str(&format!(". Resource: {}", self.resource.title));
        }
        let outline: Vec<String> = self.drills.iter().map(|d| d.prompt.clone()).collect();
        if !outline.is_empty() {
            s.push_str(&format!(". Outline: {}", outline.join(" | ")));
        }
        s
    }

    /// The dojo cards to seed for this week's drills — stable ids (via `curriculum:<date>:<topic>`
    /// sources) so re-seeding the same week de-dups. Answers are self-study (the resource stands
    /// in when it has a URL).
    pub fn dojo_cards(&self, created: i64) -> Vec<crate::dojo::Card> {
        let repo = if self.chosen.repo.is_empty() {
            "curriculum".to_string()
        } else {
            self.chosen.repo.clone()
        };
        let answer = if self.resource.url.is_empty() {
            "(self-study — curriculum drill)".to_string()
        } else {
            self.resource.url.clone()
        };
        self.drills
            .iter()
            .enumerate()
            .map(|(i, d)| {
                crate::dojo::Card::new(
                    repo.clone(),
                    crate::dojo::CardKind::Quiz,
                    self.chosen.area.clone(),
                    d.prompt.clone(),
                    answer.clone(),
                    format!("curriculum:{}:{}:{i}", self.date, self.chosen.topic),
                    created,
                )
            })
            .collect()
    }
}

/// Derive the weekly [`Plan`] from a packet. `monday` is the date the goal is scheduled for; a
/// `resource` override (from a harness-backed finder) is used when present, else a deterministic
/// [`default_resource`].
pub fn build_plan(packet: &Packet, monday: &str, resource: Option<Resource>) -> Plan {
    let chosen = select_topic(packet);
    let resource = resource.unwrap_or_else(|| default_resource(&chosen.topic));
    let drills = drills_for(&chosen.topic, &chosen.repo);
    Plan {
        date: packet.date.clone(),
        monday: monday.to_string(),
        chosen,
        resource,
        drills,
        continuity: packet.continuity.clone(),
        signal: packet.signal,
    }
}

// --- card render (pure) -------------------------------------------------------------------------

/// Render the plan as the one weekly [`BriefingCard`] — headline (exactly one topic), an
/// evidence-linked body, and the routed items: the Monday `goal`, a `research` item for the
/// resource (only when a home repo is known, so the card stays link/route-clean), and the
/// `dojo-card` drills.
pub fn build_card(plan: &Plan) -> BriefingCard {
    let topic = &plan.chosen.topic;
    let headline = format!("Curriculum · {topic}");

    let mut body = String::new();
    body.push_str(&format!("**This week: {topic}**\n\n"));
    body.push_str(&format!("- Why: {}\n", plan.chosen.why));
    let res_line = if plan.resource.url.is_empty() {
        format!("{} — {}", plan.resource.title, plan.resource.summary)
    } else {
        format!(
            "[{}]({}) — {}",
            plan.resource.title, plan.resource.url, plan.resource.summary
        )
    };
    body.push_str(&format!(
        "- Resource (≤{}m): {res_line}\n",
        RESOURCE_MINUTES
    ));
    if let Some(fb) = &plan.resource.fallback {
        body.push_str(&format!("- Fallback: {fb}\n"));
    }
    body.push_str(&format!(
        "- Practice: {} drill{} queued on the dojo; Monday goal `{}`\n",
        plan.drills.len(),
        plural(plan.drills.len() as u32),
        plan.goal_title()
    ));
    if plan.signal == Signal::Thin {
        body.push_str("- Signal: thin this week — targeting the habit, not a specific gap\n");
    }
    body.push_str(&format!("\n{}\n", verdict_line(&plan.continuity)));

    let mut items = Vec::new();
    items.push(CardItem {
        kind: "goal".to_string(),
        severity: "info".to_string(),
        title: plan.goal_title(),
        link: link_if_github(&plan.resource.url),
        repo: String::new(),
        branch: String::new(),
    });
    if !plan.chosen.repo.is_empty() {
        items.push(CardItem {
            kind: "research".to_string(),
            severity: "info".to_string(),
            title: format!(
                "resource: {} — {}",
                plan.resource.title, plan.resource.summary
            ),
            link: link_if_github(&plan.resource.url),
            repo: plan.chosen.repo.clone(),
            branch: "main".to_string(),
        });
    }
    for d in &plan.drills {
        items.push(CardItem {
            kind: "dojo-card".to_string(),
            severity: "info".to_string(),
            title: d.prompt.clone(),
            link: String::new(),
            repo: plan.chosen.repo.clone(),
            branch: String::new(),
        });
    }

    BriefingCard {
        agent: "curriculum".to_string(),
        date: plan.date.clone(),
        headline,
        body_md: body,
        items,
    }
}

/// The "last week's verdict" line for the card body.
fn verdict_line(c: &Continuity) -> String {
    match (&c.prior_topic, c.verdict) {
        (Some(topic), Some(v)) => {
            let extra = if v == Verdict::Dropped {
                " (carried too long — dropped to avoid a zombie topic)"
            } else if c.must_resolve {
                " (must finish or drop this week)"
            } else {
                ""
            };
            format!("_Last week's verdict: **{}** — {topic}{extra}._", v.label())
        }
        _ => "_Last week's verdict: none — this is the first Curriculum card._".to_string(),
    }
}

/// A link only when it points at github (the always-allowed staff card domain); anything else is
/// left off the card item so it can never carry an attacker-chosen destination. The full URL still
/// lives in the body / learn note for a human to open deliberately.
fn link_if_github(url: &str) -> String {
    let host = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    if host == "github.com" || host.ends_with(".github.com") {
        url.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prior(date: &str, topic: &str, completed: bool) -> PriorCard {
        PriorCard {
            date: date.to_string(),
            topic: topic.to_string(),
            completed,
        }
    }

    fn weak(repo: &str, area: &str, lapses: u32, cards: u32, reviewed: u32) -> WeakArea {
        WeakArea {
            repo: repo.to_string(),
            area: area.to_string(),
            lapses,
            cards,
            reviewed,
        }
    }

    fn theme(tag: &str, count: u32, prior: u32) -> ThemeTag {
        ThemeTag {
            tag: tag.to_string(),
            count,
            prior,
        }
    }

    fn tech(language: &str, repos: &[&str], files: u32) -> TechFact {
        TechFact {
            language: language.to_string(),
            repos: repos.iter().map(|s| s.to_string()).collect(),
            files,
        }
    }

    #[test]
    fn trend_reads_prior_week() {
        assert_eq!(trend_of(3, 0), Trend::New);
        assert_eq!(trend_of(3, 1), Trend::Rising);
        assert_eq!(trend_of(2, 2), Trend::Steady);
        assert_eq!(trend_of(1, 4), Trend::Falling);
    }

    #[test]
    fn continuity_none_on_first_card() {
        let c = classify_continuity(&[]);
        assert_eq!(c.prior_topic, None);
        assert_eq!(c.verdict, None);
        assert_eq!(c.carry_count, 0);
        assert!(!c.must_resolve);
    }

    #[test]
    fn continuity_completed_when_prior_done() {
        let c = classify_continuity(&[prior("2026-07-05", "error-paths", true)]);
        assert_eq!(c.verdict, Some(Verdict::Completed));
        assert!(!c.must_resolve);
    }

    #[test]
    fn continuity_carried_first_then_flags_resolve_on_second() {
        // Fresh unfinished topic — carried, no pressure yet.
        let c1 = classify_continuity(&[prior("2026-07-05", "async", false)]);
        assert_eq!(c1.verdict, Some(Verdict::Carried));
        assert_eq!(c1.carry_count, 1);
        assert!(!c1.must_resolve);

        // Same topic two weeks running, unfinished — carried, but must resolve now.
        let c2 = classify_continuity(&[
            prior("2026-07-05", "async", false),
            prior("2026-06-28", "async", false),
        ]);
        assert_eq!(c2.verdict, Some(Verdict::Carried));
        assert_eq!(c2.carry_count, 2);
        assert!(c2.must_resolve);
    }

    #[test]
    fn continuity_two_carry_rule_drops_zombie() {
        // Three weeks unfinished on the same topic → the two-carry rule drops it.
        let c = classify_continuity(&[
            prior("2026-07-05", "async", false),
            prior("2026-06-28", "async", false),
            prior("2026-06-21", "async", false),
        ]);
        assert_eq!(c.carry_count, 3);
        assert_eq!(c.verdict, Some(Verdict::Dropped));
        assert!(c.must_resolve);
    }

    #[test]
    fn continuity_carry_count_resets_when_topic_changes() {
        let c = classify_continuity(&[
            prior("2026-07-05", "lifetimes", false),
            prior("2026-06-28", "async", false),
            prior("2026-06-21", "async", false),
        ]);
        assert_eq!(c.prior_topic, Some("lifetimes".to_string()));
        assert_eq!(c.carry_count, 1);
        assert_eq!(c.verdict, Some(Verdict::Carried));
    }

    #[test]
    fn signal_rich_on_theme_or_selected_dojo_area_else_thin() {
        assert_eq!(signal_of(&[theme("x", 2, 0)], None), Signal::Rich);
        assert_eq!(signal_of(&[theme("x", 1, 0)], None), Signal::Thin);
        let weak = weak("r", "a", 1, 3, 2);
        assert_eq!(signal_of(&[], Some(&weak)), Signal::Rich);
    }

    #[test]
    fn fallback_topic_maps_language() {
        let (topic, repo) = fallback_topic(&[tech("Rust", &["bn"], 40)]).unwrap();
        assert_eq!(topic, "Rust: lifetimes in async contexts");
        assert_eq!(repo, "bn");
        assert!(fallback_topic(&[]).is_none());
    }

    fn rich_packet() -> Packet {
        Packet::assemble(
            "2026-07-05",
            "my-laptop",
            vec![theme("error-paths", 3, 1)],
            Some(weak("bn", "verify", 6, 5, 3)),
            vec![tech("Rust", &["bn"], 40)],
            vec![],
            vec![],
        )
    }

    #[test]
    fn select_prefers_theme_when_rich() {
        let p = rich_packet();
        let c = select_topic(&p);
        assert_eq!(c.source, TopicSource::Theme);
        assert_eq!(c.topic, "error-paths");
        assert!(c.why.contains("3×"));
    }

    #[test]
    fn select_uses_dojo_when_no_theme() {
        let p = Packet::assemble(
            "2026-07-05",
            "my-laptop",
            vec![],
            Some(weak("bn", "verify", 6, 5, 3)),
            vec![tech("Rust", &["bn"], 40)],
            vec![],
            vec![],
        );
        let c = select_topic(&p);
        assert_eq!(c.source, TopicSource::Dojo);
        assert_eq!(c.topic, "verify");
        assert_eq!(c.repo, "bn");
        assert_eq!(c.area, Some("verify".to_string()));
    }

    #[test]
    fn select_thin_signal_falls_back_to_tech() {
        let p = Packet::assemble(
            "2026-07-05",
            "my-laptop",
            vec![],
            None,
            vec![tech("Rust", &["bn"], 40)],
            vec![],
            vec![],
        );
        assert_eq!(p.signal, Signal::Thin);
        let c = select_topic(&p);
        assert_eq!(c.source, TopicSource::Fallback);
        assert_eq!(c.topic, "Rust: lifetimes in async contexts");
        assert_eq!(c.repo, "bn");
    }

    #[test]
    fn select_bare_habit_when_nothing() {
        let p = Packet::assemble(
            "2026-07-05",
            "my-laptop",
            vec![],
            None,
            vec![],
            vec![],
            vec![],
        );
        let c = select_topic(&p);
        assert_eq!(c.source, TopicSource::Fallback);
        assert!(c.repo.is_empty());
        assert!(c.topic.contains("essay"));
    }

    #[test]
    fn select_continues_carried_topic() {
        let p = Packet::assemble(
            "2026-07-05",
            "my-laptop",
            vec![theme("error-paths", 3, 1)],
            Some(weak("bn", "verify", 6, 5, 3)),
            vec![tech("Rust", &["bn"], 40)],
            vec![],
            vec![prior("2026-06-28", "async streams", false)],
        );
        let c = select_topic(&p);
        // The carried topic wins over the fresh theme/dojo signal.
        assert_eq!(c.source, TopicSource::Continuation);
        assert_eq!(c.topic, "async streams");
    }

    #[test]
    fn select_does_not_repick_dropped_topic() {
        // "verify" was dropped as a zombie; the fresh choice must avoid it and take the theme.
        let p = Packet::assemble(
            "2026-07-05",
            "my-laptop",
            vec![theme("error-paths", 3, 1)],
            Some(weak("bn", "verify", 6, 5, 3)),
            vec![tech("Rust", &["bn"], 40)],
            vec![],
            vec![
                prior("2026-07-05", "verify", false),
                prior("2026-06-28", "verify", false),
                prior("2026-06-21", "verify", false),
            ],
        );
        assert_eq!(p.continuity.verdict, Some(Verdict::Dropped));
        let c = select_topic(&p);
        assert_ne!(c.topic, "verify");
        assert_eq!(c.topic, "error-paths");
    }

    #[test]
    fn plan_and_card_shape_is_one_topic_evidence_linked_timeboxed() {
        let p = rich_packet();
        let plan = build_plan(&p, "2026-07-06", None);
        assert_eq!(plan.goal_title(), "curriculum: error-paths — 30m");
        assert_eq!(plan.drills.len(), 3);

        let card = build_card(&plan);
        assert_eq!(card.agent, "curriculum");
        assert_eq!(card.headline, "Curriculum · error-paths");
        // Exactly one topic — the headline names it and there is a single goal item.
        let goals: Vec<_> = card.items.iter().filter(|i| i.kind == "goal").collect();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].title, "curriculum: error-paths — 30m");
        // Evidence-linked + time-boxed.
        assert!(card.body_md.contains("flagged 3×"));
        assert!(card.body_md.contains("≤120m"));
        // Drills routed as dojo cards.
        assert_eq!(
            card.items.iter().filter(|i| i.kind == "dojo-card").count(),
            3
        );
        // A research item for the resource (repo known).
        assert_eq!(
            card.items.iter().filter(|i| i.kind == "research").count(),
            1
        );
    }

    #[test]
    fn thin_signal_still_produces_a_valid_card() {
        let p = Packet::assemble(
            "2026-07-05",
            "my-laptop",
            vec![],
            None,
            vec![tech("Rust", &["bn"], 40)],
            vec![],
            vec![],
        );
        let plan = build_plan(&p, "2026-07-06", None);
        let card = build_card(&plan);
        assert!(!card.headline.is_empty());
        assert!(card.body_md.contains("thin"));
        assert_eq!(card.items.iter().filter(|i| i.kind == "goal").count(), 1);
        assert!(card.items.iter().any(|i| i.kind == "dojo-card"));
    }

    #[test]
    fn dojo_cards_have_stable_ids_for_idempotent_reseed() {
        let p = rich_packet();
        let plan = build_plan(&p, "2026-07-06", None);
        let a = plan.dojo_cards(1000);
        let b = plan.dojo_cards(2000);
        let ids_a: Vec<_> = a.iter().map(|c| c.id.clone()).collect();
        let ids_b: Vec<_> = b.iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids_a, ids_b, "ids must not depend on created time");
        assert_eq!(ids_a.len(), 3);
        // ids are distinct across the three drills
        let mut uniq = ids_a;
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), 3);
    }

    #[test]
    fn only_github_links_survive_onto_items() {
        assert_eq!(
            link_if_github("https://github.com/x/y"),
            "https://github.com/x/y"
        );
        assert_eq!(
            link_if_github("https://raw.github.com/x"),
            "https://raw.github.com/x"
        );
        assert_eq!(link_if_github("https://evil.com/x"), "");
        assert_eq!(link_if_github("https://github.com.evil.com/x"), "");
        assert_eq!(link_if_github(""), "");
    }

    #[test]
    fn packet_json_round_trips() {
        let p = rich_packet();
        let json = p.to_json();
        let back: Packet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn learn_text_carries_topic_resource_and_outline() {
        let p = rich_packet();
        let plan = build_plan(&p, "2026-07-06", None);
        let t = plan.learn_text();
        assert!(t.starts_with("curriculum: error-paths"));
        assert!(t.contains("Resource:"));
        assert!(t.contains("Outline:"));
    }
}
