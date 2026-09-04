//! The notification policy gate — one chokepoint every desktop toast passes through.
//!
//! `bn` used to call [`super::emit`] from fifteen independent sites, each deciding on its own
//! whether to interrupt. There was no severity bar, no rate limit, no cross-source dedup and no
//! quiet window, so an urgent attention item was toasted twice (once by `bn watch`, once by the
//! `notify` job, each with its own dedup ledger), a blocked orchestrate lane re-toasted every
//! fifteen seconds, and neither focus mode nor away mode silenced anything. The predictable
//! result is that the toasts stopped being read.
//!
//! This module applies the rule the staff framework already states for attention cards (#269,
//! [`crate::staff_backpressure`]) to the OS toast layer: **degrade toward silence, not noise.**
//! A toast means *work has stopped and needs your decision*. Everything else is pull-only and
//! stays readable in `bn next` / `bn attn` / `bn agent live` and the tmux badges.
//!
//! Call sites declare **intent** — a [`Class`] and a stable dedup [`Notification::key`] — and
//! [`decide`] alone decides delivery, in this order:
//!
//! 1. **Bar** — is this class loud enough to interrupt for? (default: [`Class::Blocking`] only)
//! 2. **Quiet window** — away mode silences everything (it batches into the `bn afk end` digest);
//!    focus mode and protected routine blocks silence everything *except* blocking, matching what
//!    [`crate::next::next_actions`] already does for the pull surface.
//! 3. **Duplicate** — this exact key has never been delivered before.
//! 4. **Cooldown** — a still-true condition re-pings at most every [`COOLDOWN_SECS`].
//! 5. **Budget** — at most [`DAILY_BUDGET`] toasts a day; past it the day degrades to silence.
//!
//! Suppression never loses anything: the underlying signal is already durable in the attention
//! store, the wake queue and the orchestrate state, and `bn notify status` reports exactly what
//! was held back and why.
//!
//! **Concurrency disclosure** (the same explicitness [`crate::routine::DUPLICATE_RISK`] uses):
//! the ledger is a plain load-modify-save with no lock, matching [`crate::staff_backpressure`].
//! Two processes delivering the same key in the same instant can therefore both see an empty
//! ledger and both toast. The window is milliseconds and the worst case is one duplicate — the
//! failure mode this module exists to bound, not to make impossible.

use crate::resolver::{state_dir, Config};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How long a given key stays quiet after a successful delivery. A condition that is *still* true
/// — a lane still parked on a question, an item still unacknowledged — re-pings at most
/// half-hourly rather than every supervise pass. Half an hour is long enough that a re-ping reads
/// as "this is still waiting" rather than as a repeat of the same alert.
pub const COOLDOWN_SECS: i64 = 1800;

/// The most toasts one day may deliver before the gate falls silent. Past it only the *first*
/// delivery of a never-seen key still lands, so a genuinely new blocker is never buried, while a
/// pathological day (a flapping fleet, a sweep dumping fifty urgent rows) degrades to silence
/// instead of a stream. Roughly one interruption per waking hour. `bn notify status` reports the
/// overflow.
pub const DAILY_BUDGET: usize = 12;

/// The most of the daily budget any one source may claim. `attention-sweep` folds email, Teams,
/// meetings, ADO and ICM through a single source at a caller-supplied severity
/// (`feed::mirror_to_attention`), so without this one misconfigured gatherer could starve every
/// other source of the day's budget. The per-source containment mirrors
/// [`crate::staff_backpressure::MAX_OPEN_ATTN_PER_SOURCE`].
pub const SOURCE_BUDGET: usize = 4;

// --- the model ---------------------------------------------------------------------------------

/// What kind of interruption a call site is asking for. The ordering is the severity ordering —
/// [`Bar`] admits everything at or above its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    /// Nothing decides itself from here: an agent is parked on a question, a command needs
    /// approval, a relaunch gave up, an urgent item is unacknowledged. Work has *stopped*.
    Blocking,
    /// Something you launched finished. Worth a look, never worth an interruption by default —
    /// completions are explicitly "not attention" (see [`crate::attention::completed_items`]).
    Completion,
    /// The system narrating that it handled itself: reconciled a merged PR, drained a queue,
    /// reclaimed a worktree, auto-switched a usage-limited harness.
    Lifecycle,
    /// A scheduled summary (standup line, wrap digest, routine block reminder) — a digest is by
    /// definition something you read on your own schedule.
    Digest,
}

impl Class {
    /// The stable wire/report label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::Completion => "completion",
            Self::Lifecycle => "lifecycle",
            Self::Digest => "digest",
        }
    }
}

/// How loud a notification must be to interrupt. Parsed from `[notify].bar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bar {
    /// Only [`Class::Blocking`] — the default, and the only setting that keeps toasts meaningful.
    Blocking,
    /// Blocking plus completions — "tell me when the thing I launched finished".
    Completion,
    /// Everything, including lifecycle narration and digests. The pre-gate behaviour.
    All,
}

impl Bar {
    /// Whether `class` clears this bar.
    pub fn admits(self, class: Class) -> bool {
        match self {
            Self::Blocking => class == Class::Blocking,
            Self::Completion => matches!(class, Class::Blocking | Class::Completion),
            Self::All => true,
        }
    }

    /// Parse a `[notify].bar` value; `None` when unrecognized so the caller can fall back loudly.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "blocking" => Some(Self::Blocking),
            "completion" | "completions" => Some(Self::Completion),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::Completion => "completion",
            Self::All => "all",
        }
    }
}

/// One notification a call site is asking to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub class: Class,
    /// Stable identity for dedup and cooldown — the *condition*, not the moment. `attn:<id>`,
    /// `orchestrate:<repo>/<branch>:needs-decision`, `fleet:<target>:blocked`. Two call sites
    /// describing the same real-world condition MUST use the same key; that is what collapses
    /// the historical double-delivery.
    pub key: String,
    /// Where it came from, for `bn notify status` attribution and the per-source share
    /// (`watch`, `orchestrate`, `routine`).
    pub source: String,
    pub title: String,
    pub body: String,
    /// A human explicitly asked for *this* toast (`bn agent done --notify`). Clears the class bar
    /// — you asked — but nothing else: a flag typed an hour ago should not punch through a focus
    /// block started ten minutes ago, nor spend more than its share of the day's budget.
    pub user_requested: bool,
}

impl Notification {
    pub fn new(
        class: Class,
        key: impl Into<String>,
        source: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            class,
            key: key.into(),
            source: source.into(),
            title: title.into(),
            body: body.into(),
            user_requested: false,
        }
    }

    /// Mark this as explicitly asked for by a human — see [`Notification::user_requested`].
    pub fn requested(mut self) -> Self {
        self.user_requested = true;
        self
    }
}

/// Why a notification was held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Suppressed {
    /// The class does not clear the configured [`Bar`].
    BelowBar,
    /// A quiet window is open — away mode, focus mode, or a protected routine block.
    Quiet(Quieted),
    /// This exact key is inside its [`COOLDOWN_SECS`] window.
    Cooldown,
    /// Today's [`DAILY_BUDGET`] is spent and this key has been delivered before.
    Budget,
    /// This source has claimed its [`SOURCE_BUDGET`] share of the day.
    SourceBudget,
    /// The gate is switched off entirely (`[notify].enabled = false`).
    Disabled,
    /// The policy said yes but no toast backend exists on this host (a headless box, a container).
    /// Deliberately *not* a delivery: it spends no budget and writes no cooldown stamp, so a
    /// machine that never had a backend does not silently burn its day's budget on toasts nobody
    /// saw — and the same condition can fire for real once a backend appears.
    NoBackend,
}

impl Suppressed {
    /// The stable tally key `bn notify status` groups by.
    pub fn label(self) -> &'static str {
        match self {
            Self::BelowBar => "below-bar",
            Self::Quiet(q) => q.label(),
            Self::Cooldown => "cooldown",
            Self::Budget => "budget",
            Self::SourceBudget => "source-budget",
            Self::Disabled => "disabled",
            Self::NoBackend => "no-backend",
        }
    }
}

/// Which quiet window silenced a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Quieted {
    Afk,
    Focus,
    ProtectedBlock,
}

impl Quieted {
    pub fn label(self) -> &'static str {
        match self {
            Self::Afk => "afk",
            Self::Focus => "focus",
            Self::ProtectedBlock => "protected-block",
        }
    }
}

/// The gate's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Cleared the gate; [`deliver`] then calls the backend (which may itself find no backend).
    Sent,
    Held(Suppressed),
}

impl Delivery {
    pub fn sent(self) -> bool {
        matches!(self, Self::Sent)
    }

    /// The suppression reason, if held.
    pub fn held(self) -> Option<Suppressed> {
        match self {
            Self::Sent => None,
            Self::Held(reason) => Some(reason),
        }
    }
}

// --- config ------------------------------------------------------------------------------------

/// The resolved `[notify]` policy. Fail-soft like [`crate::routine::RoutineConfig`]: a malformed
/// section never breaks an unrelated command, it just leaves the defaults in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// `false` silences every toast (the gate still tallies, so `bn notify status` explains it).
    pub enabled: bool,
    pub bar: Bar,
    pub cooldown_secs: i64,
    pub daily_budget: usize,
    pub source_budget: usize,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bar: Bar::Blocking,
            cooldown_secs: COOLDOWN_SECS,
            daily_budget: DAILY_BUDGET,
            source_budget: SOURCE_BUDGET,
        }
    }
}

/// The raw `[notify]` table as written in `config.toml`. Deserialized straight into the domain's
/// own shape (mirroring `[routine]`) rather than through a scalar-per-key section, so the
/// clamping and the vocabulary live next to the policy that uses them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NotifyFileConfig {
    pub enabled: Option<bool>,
    pub bar: Option<String>,
    pub cooldown_mins: Option<i64>,
    pub daily_budget: Option<usize>,
    pub source_budget: Option<usize>,
}

impl NotifyConfig {
    /// Resolve the `[notify]` table over the defaults. Deliberately infallible: a nonsense value
    /// falls back to its default rather than failing the whole `Config::resolve`, because a typo
    /// in a notification preference must never break unrelated commands. Out-of-range numbers are
    /// clamped rather than rejected.
    ///
    /// What is **not** configurable, on purpose: the class of each call site, the quiet-window
    /// semantics, and which classes each bar admits. The point of the gate is fewer knobs, not
    /// more — every added knob is a route back to the noise.
    pub fn resolve(file: Option<NotifyFileConfig>) -> Self {
        let d = Self::default();
        let Some(file) = file else { return d };
        let daily_budget = file
            .daily_budget
            .map(|n| n.clamp(1, 100))
            .unwrap_or(d.daily_budget);
        Self {
            enabled: file.enabled.unwrap_or(d.enabled),
            bar: file.bar.as_deref().and_then(Bar::parse).unwrap_or(d.bar),
            cooldown_secs: file
                .cooldown_mins
                .filter(|m| *m > 0)
                .map(|m| m.saturating_mul(60))
                .unwrap_or(d.cooldown_secs),
            daily_budget,
            // A per-source share above the daily budget is meaningless — clamp rather than reject.
            source_budget: file
                .source_budget
                .map(|n| n.clamp(1, daily_budget))
                .unwrap_or_else(|| d.source_budget.min(daily_budget)),
        }
    }
}

// --- quiet windows -----------------------------------------------------------------------------

/// The open quiet windows at a moment — probed once per delivery so [`decide`] stays pure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Quiet {
    /// Away mode ([`crate::afk`]): everything is batched into the `bn afk end` digest.
    pub afk: bool,
    /// A declared deep-work block ([`crate::focus`]).
    pub focus: bool,
    /// A protected recurring block — family/recovery/exercise/sleep ([`crate::routine`]).
    pub protected_block: bool,
}

impl Quiet {
    /// Read the three quiet-window predicates. The routine config is fail-soft: an `Err` is a
    /// no-op, exactly as [`crate::next::next_actions`] treats it.
    pub fn probe(cfg: &Config, now: i64) -> Self {
        Self {
            afk: crate::afk::is_active(now),
            focus: crate::focus::is_active(now),
            protected_block: cfg
                .routine
                .as_ref()
                .is_ok_and(|routine| crate::routine::suppresses_guidance(routine, now)),
        }
    }

    /// Which window (if any) silences `class`.
    ///
    /// Away mode silences everything — that is its documented contract ("routine fleet activity is
    /// batched"), and `bn afk end` replays the lot. Focus and protected blocks let blocking
    /// through, matching [`crate::next::next_actions`], which keeps urgent actions visible inside
    /// a focus block. Consistency with the pull surface matters more here than absolute silence.
    pub fn silences(self, class: Class) -> Option<Quieted> {
        if self.afk {
            return Some(Quieted::Afk);
        }
        if class == Class::Blocking {
            return None;
        }
        if self.focus {
            return Some(Quieted::Focus);
        }
        if self.protected_block {
            return Some(Quieted::ProtectedBlock);
        }
        None
    }
}

// --- the ledger --------------------------------------------------------------------------------

/// Delivery bookkeeping: when each key last landed, and today's tallies. Machine-local under the
/// state dir beside the other small side-stores ([`crate::staff_backpressure`], `doctor.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// Notification key → epoch seconds of its last successful delivery.
    #[serde(default)]
    pub last_sent: BTreeMap<String, i64>,
    /// The date the tallies below belong to; a different `today` resets them.
    #[serde(default)]
    pub day: String,
    /// Toasts actually delivered today.
    #[serde(default)]
    pub sent_today: usize,

    /// How many of `sent_today` were carried by the tmux fallback rather than an OS toast — so
    /// `bn notify status` can say the desktop backend is down without claiming nothing arrived.
    #[serde(default)]
    pub sent_via_tmux: usize,
    /// Source → toasts it delivered today, for the per-source share.
    #[serde(default)]
    pub sent_by_source: BTreeMap<String, usize>,
    /// Suppression reason label → count, today.
    #[serde(default)]
    pub held_today: BTreeMap<String, usize>,
}

impl Ledger {
    /// Roll the tallies over when the day changes, so counters describe *today*.
    pub fn roll(&mut self, today: &str) {
        if self.day != today {
            self.day = today.to_string();
            self.sent_today = 0;
            self.sent_via_tmux = 0;
            self.sent_by_source.clear();
            self.held_today.clear();
        }
    }

    /// Whether `key` has ever been delivered.
    pub fn seen(&self, key: &str) -> bool {
        self.last_sent.contains_key(key)
    }

    /// Toasts `source` has delivered today.
    pub fn source_spend(&self, source: &str) -> usize {
        self.sent_by_source.get(source).copied().unwrap_or(0)
    }

    /// Record an outcome. `roll` must have run first.
    ///
    /// [`Suppressed::NoBackend`] is tallied like any other hold but writes no cooldown stamp and
    /// spends no budget — nothing reached a screen, so nothing was spent.
    pub fn record(&mut self, n: &Notification, outcome: Delivery, now: i64) {
        match outcome {
            Delivery::Sent => {
                self.last_sent.insert(n.key.clone(), now);
                self.sent_today += 1;
                *self.sent_by_source.entry(n.source.clone()).or_insert(0) += 1;
            }
            Delivery::Held(reason) => {
                *self
                    .held_today
                    .entry(reason.label().to_string())
                    .or_insert(0) += 1;
            }
        }
    }

    /// Toasts held back today for a reason that means *something wanted you and did not reach
    /// you*: a quiet window, a cooldown, a spent budget.
    ///
    /// Deliberately excludes `below-bar`, which is by-design silence rather than a withheld
    /// interrupt — a lifecycle toast that never fires is the gate working, not a thing to report.
    /// Counting it here would put a permanent warning on `bn next` from the two-minute cron.
    pub fn withheld_today(&self) -> usize {
        self.held_today
            .iter()
            .filter(|(reason, _)| reason.as_str() != Suppressed::BelowBar.label())
            .map(|(_, count)| *count)
            .sum()
    }

    /// Toasts that failed to reach a backend at all today — the channel is broken, not busy.
    ///
    /// Split out of [`Self::withheld_today`] because the two mean opposite things to the reader:
    /// a cooldown or a spent budget is the policy gate working as designed and needs no action,
    /// while `no-backend` means every interrupt bn raised today was lost and someone has to fix
    /// the delivery path. Summed together they read as one undifferentiated "held" count.
    pub fn undeliverable(&self) -> usize {
        self.held_today
            .get(Suppressed::NoBackend.label())
            .copied()
            .unwrap_or(0)
    }

    /// Toasts held back today by a *policy* decision — a quiet window, a cooldown, a spent
    /// budget — excluding both by-design silence (`below-bar`) and outright delivery failure
    /// ([`Self::undeliverable`]).
    pub fn withheld_by_policy(&self) -> usize {
        self.withheld_today() - self.undeliverable()
    }

    /// Keys still inside their cooldown at `now`, newest delivery first — the `bn notify status`
    /// "currently quiet" list.
    pub fn in_cooldown(&self, cooldown_secs: i64, now: i64) -> Vec<(String, i64)> {
        let mut out: Vec<(String, i64)> = self
            .last_sent
            .iter()
            .filter(|(_, &sent)| now.saturating_sub(sent) < cooldown_secs)
            .map(|(key, &sent)| (key.clone(), sent))
            .collect();
        out.sort_by_key(|(_, sent)| std::cmp::Reverse(*sent));
        out
    }

    /// Drop delivery stamps far older than any cooldown so the file cannot grow without bound.
    /// A key not seen for a day is functionally new anyway.
    fn prune(&mut self, now: i64) {
        self.last_sent
            .retain(|_, &mut sent| now.saturating_sub(sent) < 86_400);
    }
}

fn ledger_path() -> PathBuf {
    state_dir().join("notify.json")
}

/// Read the ledger; a missing or corrupt file reads as empty (the same degrade-to-default the
/// other state-dir side-stores use — bookkeeping must never block a notification).
pub fn load_ledger() -> Ledger {
    std::fs::read_to_string(ledger_path())
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}

fn save_ledger(ledger: &Ledger) -> std::io::Result<()> {
    crate::store::save_json_atomic(&ledger_path(), ledger)
}

// --- the decision ------------------------------------------------------------------------------

/// The gate, as a pure function of the notification and the world around it — so every rule below
/// is table-testable without an OS backend, a state dir or a clock. [`deliver`] is the thin IO
/// wrapper. Same split as [`crate::staff_backpressure::decide`] / `throttle_gate`.
pub fn decide(
    n: &Notification,
    ledger: &Ledger,
    cfg: &NotifyConfig,
    quiet: Quiet,
    now: i64,
) -> Delivery {
    if !cfg.enabled {
        return Delivery::Held(Suppressed::Disabled);
    }
    // `user_requested` clears the bar and nothing else — see `Notification::user_requested`.
    if !n.user_requested && !cfg.bar.admits(n.class) {
        return Delivery::Held(Suppressed::BelowBar);
    }
    if let Some(window) = quiet.silences(n.class) {
        return Delivery::Held(Suppressed::Quiet(window));
    }
    // A key inside its cooldown stays quiet — this is what collapses a lane that re-reports the
    // same blocking condition on every supervise pass.
    if let Some(&sent) = ledger.last_sent.get(&n.key) {
        if now.saturating_sub(sent) < cfg.cooldown_secs {
            return Delivery::Held(Suppressed::Cooldown);
        }
    }
    // Budget shedding deliberately spares first-ever keys: a brand-new blocker still gets through
    // on a day that has otherwise gone loud.
    if ledger.seen(&n.key) {
        if ledger.source_spend(&n.source) >= cfg.source_budget {
            return Delivery::Held(Suppressed::SourceBudget);
        }
        if ledger.sent_today >= cfg.daily_budget {
            return Delivery::Held(Suppressed::Budget);
        }
    }
    Delivery::Sent
}

/// Deliver `n` subject to policy: probe the quiet windows, apply [`decide`], toast on a pass, and
/// record the outcome either way. Best-effort throughout — a ledger that cannot be written still
/// lets the toast through, because a missed dedup is a smaller failure than a missed blocker.
pub fn deliver(n: &Notification, now: i64) -> Delivery {
    let cfg = Config::load();
    deliver_with(&cfg, n, now)
}

/// [`deliver`] for callers that already hold a [`Config`], so a hot path need not re-read it.
pub fn deliver_with(cfg: &Config, n: &Notification, now: i64) -> Delivery {
    deliver_using(
        cfg,
        n,
        now,
        &|title, body| super::emit(title, body),
        &|title, body| super::emit_tmux(title, body),
    )
}

/// [`deliver_with`] with the backend injected, so tests exercise the whole ledger path without
/// firing a real desktop toast on the developer's machine. `emit` reports whether a toast landed.
pub fn deliver_using(
    cfg: &Config,
    n: &Notification,
    now: i64,
    emit: &dyn Fn(&str, &str) -> bool,
    fallback: &dyn Fn(&str, &str) -> bool,
) -> Delivery {
    let quiet = Quiet::probe(cfg, now);
    let mut ledger = load_ledger();
    ledger.roll(&crate::util::today());
    let mut outcome = decide(n, &ledger, &cfg.notify, quiet, now);
    let mut via_tmux = false;
    if outcome.sent() && !emit(&n.title, &n.body) {
        // The OS toast backend is absent or broken. Before writing this off, try the one screen we
        // know the user is looking at: the policy already decided this interrupt was worth making,
        // and a decision that reaches nobody is the same as no decision at all.
        if fallback(&n.title, &n.body) {
            via_tmux = true;
        } else {
            // Nothing reached a screen, so this must not count as a delivery.
            outcome = Delivery::Held(Suppressed::NoBackend);
        }
    }
    ledger.record(n, outcome, now);
    if via_tmux {
        ledger.sent_via_tmux += 1;
    }
    ledger.prune(now);
    crate::util::best_effort(save_ledger(&ledger), "notify: persist delivery ledger");
    outcome
}

/// The `bn watch` shape: decide every candidate independently, then fold the survivors into **one**
/// interrupt rather than firing a toast per item. Five blocked agents are one toast, not five.
///
/// `title` receives the number of survivors so the caller can phrase its own headline, and the
/// bodies are joined with `separator`. Every survivor's key is recorded, so the next pass sees
/// them all in cooldown. Returns each candidate's verdict, positionally.
pub fn deliver_batch(
    cfg: &Config,
    candidates: &[Notification],
    title: &dyn Fn(usize) -> String,
    separator: &str,
    now: i64,
) -> Vec<Delivery> {
    deliver_batch_using(
        cfg,
        candidates,
        title,
        separator,
        now,
        &|t, b| super::emit(t, b),
        &|t, b| super::emit_tmux(t, b),
    )
}

/// [`deliver_batch`] with the backend injected — see [`deliver_using`].
pub fn deliver_batch_using(
    cfg: &Config,
    candidates: &[Notification],
    title: &dyn Fn(usize) -> String,
    separator: &str,
    now: i64,
    emit: &dyn Fn(&str, &str) -> bool,
    fallback: &dyn Fn(&str, &str) -> bool,
) -> Vec<Delivery> {
    let quiet = Quiet::probe(cfg, now);
    let mut ledger = load_ledger();
    ledger.roll(&crate::util::today());

    // Decide against a *provisional* copy that grows as we go, so one burst cannot spend the day's
    // budget several times over. The real ledger is only written once the verdicts are final.
    let mut provisional = ledger.clone();
    let mut verdicts = Vec::with_capacity(candidates.len());
    let mut winners = Vec::new();
    for n in candidates {
        let outcome = decide(n, &provisional, &cfg.notify, quiet, now);
        if outcome.sent() {
            provisional.record(n, outcome, now);
            winners.push(n);
        }
        verdicts.push(outcome);
    }

    if !winners.is_empty() {
        let body = winners
            .iter()
            .map(|n| n.body.as_str())
            .collect::<Vec<_>>()
            .join(separator);
        let headline = title(winners.len());
        if !emit(&headline, &body) {
            if fallback(&headline, &body) {
                // The burst is already folded into one interrupt, so the fallback inherits that
                // discipline: one tmux message, not one per item.
                ledger.sent_via_tmux += 1;
            } else {
                // Nothing reached a screen, so no key earned a cooldown: demote the whole burst.
                for verdict in verdicts.iter_mut().filter(|v| v.sent()) {
                    *verdict = Delivery::Held(Suppressed::NoBackend);
                }
            }
        }
    }

    for (n, verdict) in candidates.iter().zip(verdicts.iter()) {
        ledger.record(n, *verdict, now);
    }
    ledger.prune(now);
    crate::util::best_effort(save_ledger(&ledger), "notify: persist delivery ledger");
    verdicts
}

// --- the injectable seam -----------------------------------------------------------------------

/// Delivers a desktop notification — the seam callers route through, so tests can inject a
/// deterministic fake instead of exercising real OS toast backends (#801 amendment invariant 8).
/// Lives here rather than in [`crate::routine`] because the policy gate, not the routine tick, is
/// now the thing worth faking; `routine` re-exports it for its existing callers.
pub trait Notifier {
    fn notify(&self, title: &str, body: &str) -> bool;
}

/// The production notifier: a [`Class::Digest`] delivery through the gate. Routine reminders are
/// scheduled summaries, so under the default bar they are pull-only — `bn routine status` and
/// `bn next` still show the block, and raising `[notify].bar` opts back into the toast.
pub struct RealNotifier;

impl Notifier for RealNotifier {
    /// Returns whether the gate **consumed** the notification, which is not the same as whether a
    /// toast appeared. [`crate::routine::tick_with`] reads `false` as "the backend is down": it
    /// then skips its delivery record and retries the same reminder every tick until the catch-up
    /// horizon expires. A deliberate suppression must therefore report `true` — the reminder was
    /// handled, and re-offering it every five minutes would be exactly the noise this gate exists
    /// to remove. Only [`Suppressed::NoBackend`], where nothing could have been shown, is `false`.
    ///
    /// The consequence worth knowing: `TickOutcome.sent` means "handed to the gate", not "reached
    /// your screen". `bn notify status` is where a held reminder shows up.
    fn notify(&self, title: &str, body: &str) -> bool {
        // The routine tick already dedups on its own delivery key (stable block id + resolved
        // instant + event kind), so the gate keys on the message it produced rather than
        // re-deriving one; a resent reminder is caught by the tick's ledger before it reaches here.
        let n = Notification::new(
            Class::Digest,
            format!("routine:{body}"),
            "routine",
            title,
            body,
        );
        !matches!(
            deliver(&n, crate::util::now_epoch_secs()),
            Delivery::Held(Suppressed::NoBackend)
        )
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    fn blocking(key: &str) -> Notification {
        Notification {
            class: Class::Blocking,
            key: key.to_string(),
            source: "watch".to_string(),
            title: "an agent is blocked".to_string(),
            body: "bn/main needs a decision".to_string(),
            user_requested: false,
        }
    }

    /// The bug this closes. On a host where every OS toast backend is absent or broken, a
    /// notification that passed the whole policy gate used to be written off as `no-backend` — the
    /// ledger on the machine that motivated this read `sent_today: 0` against nineteen holds in a
    /// day. It must now reach the screen the user is actually looking at, and count as delivered.
    #[test]
    fn a_dead_desktop_backend_falls_back_to_tmux_and_counts_as_sent() {
        crate::test_support::with_state_dir(|| {
            let cfg = Config::default();
            let out = deliver_using(
                &cfg,
                &blocking("attn:1"),
                1_000,
                &|_, _| false, // every OS backend broken, as on WSL with a failing PS toast
                &|_, _| true,  // a tmux client is attached
            );
            assert!(out.sent(), "the fallback delivered it: {out:?}");
            let mut ledger = load_ledger();
            ledger.roll(&crate::util::today());
            assert_eq!(ledger.sent_today, 1);
            assert_eq!(ledger.sent_via_tmux, 1, "and we recorded which channel");
        });
    }

    /// With no screen at all — a headless cron run, no tmux client attached — the honest answer is
    /// still `no-backend`. The fallback must not let us claim delivery to nobody.
    #[test]
    fn with_no_screen_at_all_it_is_still_recorded_as_undeliverable() {
        crate::test_support::with_state_dir(|| {
            let cfg = Config::default();
            let out = deliver_using(&cfg, &blocking("attn:2"), 1_000, &|_, _| false, &|_, _| {
                false
            });
            assert_eq!(out, Delivery::Held(Suppressed::NoBackend));
            let mut ledger = load_ledger();
            ledger.roll(&crate::util::today());
            assert_eq!(ledger.sent_today, 0);
            assert_eq!(ledger.undeliverable(), 1);
        });
    }

    /// A working desktop backend must not touch the fallback, nor be miscounted as a tmux
    /// delivery — otherwise `bn notify status` would start claiming the backend is down.
    #[test]
    fn a_working_desktop_backend_never_reaches_the_fallback() {
        crate::test_support::with_state_dir(|| {
            let cfg = Config::default();
            let out = deliver_using(&cfg, &blocking("attn:3"), 1_000, &|_, _| true, &|_, _| {
                panic!("the fallback must not run when the toast landed")
            });
            assert!(out.sent());
            let mut ledger = load_ledger();
            ledger.roll(&crate::util::today());
            assert_eq!((ledger.sent_today, ledger.sent_via_tmux), (1, 0));
        });
    }

    /// `bn watch` folds a burst into ONE interrupt; the fallback inherits that discipline rather
    /// than firing a tmux message per item.
    #[test]
    fn a_burst_falls_back_as_a_single_message() {
        crate::test_support::with_state_dir(|| {
            let cfg = Config::default();
            let items = [blocking("attn:a"), blocking("attn:b"), blocking("attn:c")];
            let calls = std::cell::Cell::new(0);
            let verdicts = deliver_batch_using(
                &cfg,
                &items,
                &|n| format!("{n} need you"),
                " · ",
                1_000,
                &|_, _| false,
                &|_, _| {
                    calls.set(calls.get() + 1);
                    true
                },
            );
            assert_eq!(calls.get(), 1, "one message for the whole burst");
            assert!(verdicts.iter().all(|v| v.sent()), "{verdicts:?}");
            let mut ledger = load_ledger();
            ledger.roll(&crate::util::today());
            assert_eq!(ledger.sent_via_tmux, 1);
        });
    }
}
