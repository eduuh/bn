//! Producers: the sites that used to launch an agent, now enqueuing one instead.
//!
//! Each is behind `[queue].capture`, a list rather than a bool, so a producer can be switched over
//! and rolled back on its own. Flipping every dispatch path at once is not a change anyone can
//! evaluate — and these are the paths that spend money.
//!
//! What "capture" buys, concretely: PR review today launches an agent with **no RAM, load or cost
//! guard whatsoever** — just a 60-minute lock that stops two reviews of the *same PR*. Routed
//! through the queue it inherits every guard the machine already had and simply never noticed it
//! was missing.
//!
//! The domain logic stays where it is. `autoreview::Ledger::pick_next` still decides *which* PR is
//! next (longest wait, force-push re-trigger, dead-PR filter — orderings the backlog's single
//! priority integer cannot express); it just hands the answer to the queue instead of to a
//! launcher.
use crate::*;
use bn_core::backlog::{self, AddSpec, BacklogKind};
use bn_core::queue::enqueue::{enqueue, Enqueued};
use bn_core::queue::kind::{ExecSpec, Requirements};

/// While this exists, nothing enqueues.
///
/// **The structural fix for the trap that bit twice.** Several entry points — `run_job`,
/// `bn dispatch` — each decide *whether to queue* before doing their work. The dispatcher calls
/// them to execute what it has already admitted, and each time it did, the entry point re-made a
/// decision that was already made: the entry was enqueued again, forever, and never ran, with
/// nothing in the logs to say so.
///
/// The previous fixes were per-call-site — "call the implementation, not the entry point" — which
/// is a rule someone has to know. It was violated twice, the second time by the same person who
/// had just written it down.
///
/// So the guard moved to the *decision* rather than the call sites. The dispatcher declares its
/// dynamic extent; inside it, [`active`] is unconditionally false. Any entry point may now be
/// called from the dispatcher — existing or not yet written — and will simply do its work. There is
/// no rule left to remember, because there is no longer a wrong way to call it.
///
/// Two scopes, because the dispatcher's work spans processes: an atomic for in-process calls, and
/// an environment variable that child `bn` invocations inherit.
///
/// The environment half must be read as *"which dispatch?"* rather than *"was there one?"*, because
/// inheritance has no end: the agent a dispatch launches keeps its copy for hours, and a `tmux`
/// server started during a dispatch hands its copy to every pane it ever creates. A bare flag
/// therefore decays from "we are inside a dispatch" into "this machine has run one", after which
/// every producer silently stops enqueueing — the exact failure this type exists to prevent. So the
/// marker carries the dispatcher's pid and is honoured only while that process lives; see
/// [`bn_core::queue::extent`].
pub(crate) struct Dispatching;

/// Set for the dynamic extent of a dispatch. Not a lock — a single-threaded truth about *what this
/// process is currently doing*.
static IN_DISPATCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The env twin of [`IN_DISPATCH`], for the `bn` processes a dispatch spawns.
const IN_DISPATCH_ENV: &str = bn_core::queue::extent::DISPATCH_EXTENT_ENV;

impl Dispatching {
    /// Enter the dispatcher's extent.
    pub(crate) fn begin() -> Dispatching {
        IN_DISPATCH.store(true, std::sync::atomic::Ordering::Relaxed);
        #[allow(clippy::disallowed_methods)] // production child-process propagation, not test env
        std::env::set_var(IN_DISPATCH_ENV, bn_core::queue::extent::extent_marker());
        Dispatching
    }

    /// Whether we are inside one — in this process, or in an ancestor that is *still running*.
    fn active() -> bool {
        IN_DISPATCH.load(std::sync::atomic::Ordering::Relaxed)
            || bn_core::queue::extent::in_dispatch_extent()
    }
}

impl Drop for Dispatching {
    fn drop(&mut self) {
        IN_DISPATCH.store(false, std::sync::atomic::Ordering::Relaxed);
        #[allow(clippy::disallowed_methods)] // paired with the set in `begin`
        std::env::remove_var(IN_DISPATCH_ENV);
    }
}

/// Whether `producer` should enqueue right now.
///
/// Config *and* a live dispatcher. Enqueueing is only correct when something will later dequeue;
/// on a machine where the tick was never registered, capturing would quietly halt every scheduled
/// agent — everything looking fine while nothing runs. The fallback is the pre-queue behaviour,
/// announced once so a silently-degraded machine is still a legible one.
pub(crate) fn active(cfg: &Config, producer: &str) -> bool {
    active_with_fallback(cfg, producer, "running it directly")
}

/// Whether the work being enqueued is the machine's own bookkeeping rather than something a person
/// asked for.
///
/// `$BN_MACHINE_DISPATCH` rather than a flag, because the only caller that can honestly set it is
/// another bn process: the dashboard's repo-setup button and its enhance/triage agents, each of
/// which renders a prompt from a template and shells out — the same hop that already passes
/// `$BN_WORKER_REPO`. A flag would put the declaration in a person's hands, and a person asserting
/// "this is not my work" about work they just asked for is the one case it must not describe.
///
/// Read by **both** enqueue doors, which is the whole point of it living here. `bn dispatch` had it
/// first; `bn queue add` did not, and that gap is what put `enhance t17: Run the today-agent daily
/// coordination cron` on a human's list. Grooming a machine-minted cron task is machine work twice
/// over — the task being groomed was never a person's, and neither is the grooming.
///
/// What it buys: the minted task (#1001) is stamped `auto:1`, so it stays off the day's list and
/// out of the SMART gate, and the entry classifies `autonomous` rather than competing for the
/// watched coding lane. See [`bn_core::queue::classify::is_machine`].
pub(crate) fn machine_owned() -> bool {
    std::env::var("BN_MACHINE_DISPATCH").is_ok_and(|v| {
        let v = v.trim();
        !v.is_empty() && v != "0"
    })
}

/// [`active`], naming what happens when the queue is configured but not draining.
///
/// Most producers fall back to launching the work themselves, which is what the default phrase says.
/// A producer whose fallback is something else has to say so: telling an operator their accepted
/// suggestion is "running directly" when it is in fact sitting unqueued describes the opposite of
/// what happened, and the whole point of the warning is that a degraded machine stays legible.
pub(crate) fn active_with_fallback(cfg: &Config, producer: &str, fallback: &str) -> bool {
    if !cfg.queue_enabled_here() {
        return false;
    }
    // Inside a dispatch, never. The decision to queue this work has already been made — by the
    // dispatcher, which is why it is executing it.
    if Dispatching::active() {
        return false;
    }
    let now = bn_core::util::now_epoch_secs();
    if cfg.queue.capture_active(producer, now) {
        return true;
    }
    if cfg.queue.captures(producer) {
        warn_dispatcher_down(producer, fallback);
    }
    false
}

/// Say once per process that the queue is configured but not draining. Silence here would be the
/// whole failure mode: work quietly launching direct while the operator believes it is queued.
fn warn_dispatcher_down(producer: &str, fallback: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SAID: AtomicBool = AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        eprintln!(
            "bn: '{producer}' would queue, but no tick has run in {}m — {fallback}. \
             Register the dispatcher with `bn cron register` (see `bn queue status`).",
            bn_core::resolver::DISPATCHER_TTL_SECS / 60
        );
    }
}

/// Producer names understood by `[queue].capture`.
pub(crate) const REVIEW: &str = "review";
pub(crate) const ATTN: &str = "attn";
pub(crate) const STAFF: &str = "staff";
pub(crate) const CRON: &str = "cron";
pub(crate) const DISPATCH: &str = "dispatch";
pub(crate) const TODAY: &str = "today";

/// Where an accepted suggestion's work sits against everything else in the queue.
///
/// Above the cron floor (negative) and above an attention `warn`, because a person reviewed this and
/// said yes. Below a typed `bn dispatch` (100), because Today's agent proposing work and a person
/// asking for it *right now* are not the same claim on the machine.
pub(crate) const TODAY_PRIORITY: i64 = 80;

/// What accepting a suggestion should put into the queue.
///
/// Two outcomes, never a refusal. A suggestion whose task the SMART gate admits queues the work; one
/// it rejects queues the pass that will make it admissible. That is what makes "Today feeds the
/// queue" unconditional — the alternative, demanding a `--repo` from whoever accepted it, hands the
/// work back to a person for exactly the suggestions Today exists to handle.
#[derive(Debug, Clone)]
pub(crate) enum TodayPlan<'a> {
    /// The task passed the gate: run the work itself, in `repo`.
    Work {
        repo: &'a str,
        task_id: &'a str,
        kind: BacklogKind,
    },
    /// The task failed an agent-closeable gap: resolve **where the work happens** first.
    ///
    /// Booked against the home repo rather than a real one, because the repo is precisely what this
    /// pass is for. It is headless (`needs_worktree()` is false), so it costs no lane slot, no pool
    /// slot and no placement — a placement-resolution pass that itself needed a placement would be
    /// a circular requirement.
    Enhance { task_id: &'a str },
}

/// Enqueue an accepted suggestion's work — or the enhance pass that will make it placeable.
///
/// Returns the `(repo, entry_id)` the suggestion should cache, or `None` when the write failed. The
/// caller must not treat that cache as authoritative: the entry is findable by its
/// `source: today:<id>` regardless, which is what keeps a crash between enqueue and link from
/// queueing the same work twice.
pub(crate) fn capture_today(
    cfg: &Config,
    suggestion: &bn_core::suggestions::Suggestion,
    plan: &TodayPlan<'_>,
    quiet: bool,
) -> Option<(String, String)> {
    let now = bn_core::util::now_epoch_secs();
    let source = suggestion.queue_source();
    let (repo, subject, spec) = match plan {
        TodayPlan::Work {
            repo,
            task_id,
            kind,
        } => (
            repo.to_string(),
            suggestion.title.clone(),
            AddSpec {
                kind: *kind,
                priority: TODAY_PRIORITY,
                source,
                task: task_id.to_string(),
                exec: ExecSpec::Agent,
                ..Default::default()
            },
        ),
        TodayPlan::Enhance { task_id } => (
            cfg.queue_home_repo(),
            format!("resolve where {task_id} happens — {}", suggestion.title),
            AddSpec {
                kind: BacklogKind::Enhance,
                // One notch above the work it unblocks: nothing downstream can be placed until this
                // has run, so letting the work it gates overtake it would only waste a lane.
                priority: TODAY_PRIORITY + 1,
                source,
                task: task_id.to_string(),
                // `Shell` rather than `Job`, because this pass must land on *this* task and the
                // `task-enhance` job takes `--due` (whichever task ranks highest). Catalog
                // validation is not lost: `bn task enhance <id>` renders the agent through
                // `agents::discover` itself.
                exec: ExecSpec::Shell {
                    cmd: format!("bn task enhance {task_id}"),
                },
                ..Default::default()
            },
        ),
    };
    match enqueue(&backlog::backlog_path(), &repo, &subject, &spec, now) {
        Ok(e) => {
            let id = e.entry().id.clone();
            if quiet {
                // Nothing: the tick sweeps this every pass, and a line per pass is log noise.
            } else if e.is_new() {
                println!("  queued {repo}/{id} · {} · priority {}", spec.kind.label(), spec.priority);
            } else {
                println!("  already queued {repo}/{id} ({})", e.label());
            }
            Some((repo, id))
        }
        Err(err) => {
            eprintln!("bn today accept: could not queue {repo} — {err}");
            None
        }
    }
}

/// Enqueue a PR review instead of launching one.
///
/// Priority is the hours it has been waiting, so a review that has been ignored all day outranks
/// one raised a minute ago without needing a second ordering mechanism. The per-PR lock stays where
/// it is — that is deduplication, not capacity, and the two should not be confused.
pub(crate) fn capture_review(cfg: &Config, repo: &str, number: i64, url: &str, waited_hours: i64) -> bool {
    let spec = AddSpec {
        kind: BacklogKind::ReviewPr,
        priority: waited_hours.max(0),
        source: format!("review:{repo}#{number}"),
        // The auto-review poll raised this, not a person deciding to review something.
        task: crate::cmd::task_mint::task_for_machine_enqueue(cfg, repo, url, "review"),
        exec: ExecSpec::Skill {
            skill: "auto-review-pr".to_string(),
            arg: Some(url.to_string()),
        },
        requires: Requirements {
            skills: vec!["auto-review-pr".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    report("auto-review-trigger", repo, url, enqueue(&backlog::backlog_path(), repo, url, &spec, bn_core::util::now_epoch_secs()))
}

/// Enqueue an attention item's agent instead of starting it.
///
/// `attention::dispatch::promote_to_backlog` already existed and is exactly this operation — the
/// autonomous path simply never used it, which is how the attention sweep ended up with its own
/// concurrency knob (`BN_ATTN_AUTO_MAX`) parallel to the machine's real capacity.
pub(crate) fn capture_attn(cfg: &Config, item: &bn_core::attention::AttnItem) -> bool {
    let Some(repo) = item.repo.clone() else {
        return false;
    };
    let now = bn_core::util::now_epoch_secs();
    let path = backlog::backlog_path();
    let bl = backlog::load(&path);
    let out = bn_core::attention::promote_to_backlog(item, &bl, &repo, now);
    let spec = AddSpec::from_entry(&out.entry);
    let _ = cfg;
    report(
        "attention-auto-trigger",
        &repo,
        &out.subject,
        enqueue(&path, &repo, &out.subject, &spec, now),
    )
}

/// Enqueue a scheduled agent by identity.
///
/// `needs_worktree()` is false for both, so the tick runs them on its express path: no lane slot,
/// no pool slot, no placement. That is what makes queuing a two-second housekeeping cron harmless
/// rather than a way to make the machine slower.
pub(crate) fn capture_job(
    cfg: &Config,
    name: &str,
    exec: ExecSpec,
    requires: Requirements,
    not_after: Option<i64>,
) -> bool {
    capture_job_with_subject(
        cfg,
        name,
        &format!("run {name}"),
        exec,
        requires,
        not_after,
    )
}

pub(crate) fn capture_job_with_subject(
    cfg: &Config,
    name: &str,
    subject: &str,
    exec: ExecSpec,
    requires: Requirements,
    not_after: Option<i64>,
) -> bool {
    let recovery = bn_core::queue::jobs::is_recovery(name);
    let heavy = bn_core::queue::jobs::is_heavy(name);
    let repo = cfg.queue_home_repo();
    let now = bn_core::util::now_epoch_secs();
    let spec = AddSpec {
        kind: BacklogKind::Cron,
        // Scheduled work is the floor: it should run, but never ahead of work a person is waiting
        // on. A negative priority says that once, here, instead of at every call site — except for
        // recovery, which has to outrank a backlog of routine crons to be worth exempting at all.
        priority: bn_core::queue::jobs::priority_for(name),
        source: format!("cron:{name}"),
        task: crate::cmd::task_mint::task_for_machine_enqueue(cfg, &repo, subject, "cron"),
        exec,
        requires,
        // A scheduled job keeps its time inside the queue: past this it is dropped rather than run
        // late, because a digest at the wrong hour is worse than no digest.
        not_after,
        recovery,
        heavy,
        ..Default::default()
    };
    report(
        name,
        &repo,
        subject,
        enqueue(&backlog::backlog_path(), &repo, subject, &spec, now),
    )
}

/// The requirements a roster agent's frontmatter implies.
///
/// A direct projection — `skills`, `mcps`, `harness` are already declared there, so a captured
/// agent needs no new declaration surface. The `home:` pin becomes an ordinary requirement, which
/// means the tick honours it uniformly instead of every job re-checking it for itself.
pub(crate) fn agent_requirements(agent: &bn_core::agents::AgentDef, home: Option<&str>) -> Requirements {
    Requirements {
        skills: agent.skills.clone(),
        harness: agent.harness.iter().cloned().collect(),
        mcps: agent.mcps.clone(),
        capabilities: home.map(|h| vec![format!("home:{h}")]).unwrap_or_default(),
        ..Default::default()
    }
}

/// The exec for a named job, carrying the repository the job belongs to.
///
/// `ctx` answers when a person ran the job inside a worktree. A systemd timer has no ctx — and the
/// repo was simply dropped there, which is how a scheduled job ended up describing itself as
/// belonging to nothing. But the cron manifest declares one (`config/crons.toml` sets `repo = "bn"`
/// at the top and every entry inherits it), `bn run` already reads that declaration as an
/// availability gate, and it was being discarded one function later.
///
/// `declared_repo` is that manifest answer, passed in rather than looked up so this stays pure.
fn named_job_exec(name: &str, ctx: Option<&Resolved>, declared_repo: Option<String>) -> ExecSpec {
    ExecSpec::Job {
        job: name.to_string(),
        repo: ctx.map(|ctx| ctx.repo.clone()).or(declared_repo),
        // Deliberately left to the ctx: the manifest names a repo, never a branch, and inventing
        // one here would make a job claim a checkout it was never pointed at.
        branch: ctx.map(|ctx| ctx.branch.clone()),
    }
}

/// Print the enqueue outcome and report success.
///
/// A repeating producer saying "already queued" rather than printing a fresh id every ten minutes
/// is the difference between a log you read and one you learn to ignore.
fn report(job: &str, repo: &str, subject: &str, outcome: std::io::Result<Enqueued>) -> bool {
    match outcome {
        Ok(e) if e.is_new() => {
            println!("{job}: queued {repo}/{} — {subject}", e.entry().id);
            true
        }
        Ok(e) => {
            println!("{job}: already queued {repo}/{} ({})", e.entry().id, e.label());
            true
        }
        Err(err) => {
            eprintln!("{job}: could not queue {repo} — {err}");
            false
        }
    }
}

/// Capture a roster agent if the queue is configured to take it.
///
/// `Some(ok)` when the agent was enqueued (the caller returns it); `None` when it should run
/// inline exactly as before. Returning an Option rather than a bool keeps the decision — captured
/// or not — legible at the call site instead of hiding it behind a flag.
pub(crate) fn maybe_capture_agent(
    cfg: &Config,
    ctx: Option<&Resolved>,
    agent: &bn_core::agents::AgentDef,
) -> Option<bool> {
    if !agent.queue || !cfg.job_allowed_here(&agent.name) || !active(cfg, CRON) {
        return None;
    }
    let home = cfg.staff_home_pin();
    Some(capture_job(
        cfg,
        &agent.name,
        named_job_exec(&agent.name, ctx, bn_core::crons::job_repo(cfg, ctx, &agent.name)),
        agent_requirements(agent, home.as_deref()),
        schedule_deadline(agent.on_calendar.as_deref()),
    ))
}

// Job classification (recovery / heavy / never-capture) lives in
// `bn_core::queue::jobs` so every enqueue path agrees on what a job is, not just this one.


/// Capture a native LLM job if the queue is configured to take it. See [`maybe_capture_agent`].
pub(crate) fn maybe_capture_job(
    cfg: &Config,
    ctx: Option<&Resolved>,
    job: &str,
) -> Option<bool> {
    if bn_core::queue::jobs::never_capture(job) {
        return None;
    }
    // Honour the host pin here rather than at the call site: a job `[jobs].only_on` pins to another
    // machine must not be queued on this one, where nothing would ever be allowed to run it. The
    // producer is the right place to know it may not take the work.
    if !cfg.job_allowed_here(job) {
        return None;
    }
    // `staff` and `cron` are separate names so an operator can queue the expensive LLM jobs while
    // leaving cheap housekeeping inline — but by default both are on, and a headless job takes the
    // express path anyway (no worktree, no lane slot), so queueing it costs a tick of latency and
    // buys uniform visibility.
    if !active(cfg, STAFF) && !active(cfg, CRON) {
        return None;
    }
    let home = cfg.staff_home_pin();
    Some(capture_job(
        cfg,
        job,
        named_job_exec(job, ctx, bn_core::crons::job_repo(cfg, ctx, job)),
        Requirements {
            capabilities: home.map(|h| vec![format!("home:{h}")]).unwrap_or_default(),
            ..Default::default()
        },
        schedule_deadline(job_schedule(cfg, job).as_deref()),
    ))
}

/// What a `bn dispatch` asked for, in one value.
///
/// Positional arguments are how the identity was lost: `capture_dispatch` took the six the queue
/// happened to support, and `--branch`/`--name`/`--yolo` were simply never passed to it. Nothing
/// failed — the call compiled, the work enqueued, and it came back out as a different piece of
/// work. A struct makes the whole request one thing that is either carried or refused, and adding
/// a dispatch flag now means adding a field here, where [`DispatchRequest::unsupported`] is
/// looking.
///
/// **Deliberately not [`Default`].** A default would let the construction site write
/// `..Default::default()`, and then a newly added flag would take a silent `false` instead of
/// failing to compile — reintroducing the exact loss this type exists to prevent, in the exact
/// place it happened before. Every field must be stated, so adding one is a compile error at every
/// caller until each has decided what it means.
#[derive(Debug, Clone)]
pub(crate) struct DispatchRequest<'a> {
    pub repo: Option<&'a str>,
    pub item: &'a str,
    pub scout: bool,
    pub skill: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub name: Option<&'a str>,
    pub yolo: bool,
    /// Declining the plan-review gate — durable on the entry, so a captured dispatch is launched
    /// with the same stance the caller asked for.
    pub no_plan_review: bool,
    /// Whether dispatch must reserve a detached primary-branch comparison slot too.
    pub with_main: bool,
    /// The pipeline stage this dispatch is, when the caller knows it. Durable for the same reason
    /// the rest of the identity is: the stage picks the model, effort, harness and workflow, and an
    /// entry that loses it is re-inferred into whichever of three stages the guess allows.
    pub stage: Option<bn_core::pipeline::stage::Stage>,
    /// Whether the machine generated this dispatch for itself rather than a person asking for it.
    ///
    /// A repo setup agent is the case that forced it: the *click* is a human's, but the work — read
    /// the build system, write `jobs/build.sh`, run `bn verify` — is bookkeeping, and its prompt is
    /// generated. Booked as a person's task it landed on the day's list as a wall of flattened
    /// prompt, and classified `foreground` it competed for the watched lane against work you were
    /// actually sitting in front of.
    ///
    /// Declared here rather than inferred from the subject, because a subject is only ever a guess:
    /// the last attempt at inference read the word "warm" out of a setup prompt's own instructions
    /// and froze priority-100 work for an away window (see `queue::classify::is_warming`).
    pub machine: bool,
    /// Flags whose meaning is "do this to the worktree as you cut it". The queue cuts the worktree
    /// later, from the entry alone, so these have nowhere durable to live yet.
    pub fresh: bool,
    pub verify: bool,
    pub plan: bool,
    pub orchestrator: bool,
}

impl DispatchRequest<'_> {
    /// What this request asks for that a queue entry cannot express — empty when capture is
    /// lossless.
    ///
    /// **The invariant: a capture is lossless or it does not happen.** Silently dropping an
    /// unrepresentable flag turns enqueueing into a quiet mutation of what the caller asked for,
    /// and the failure surfaces hours later as an agent doing not-quite-the-declared-job. Naming
    /// the gap sends the work down the direct path instead, which is always correct — worse for
    /// throughput, never wrong.
    fn unsupported(&self) -> Vec<&'static str> {
        [
            (self.orchestrator, "--orchestrator"),
            (self.fresh, "--fresh"),
            (self.verify, "--verify"),
            (self.plan, "--plan"),
        ]
        .into_iter()
        .filter_map(|(set, flag)| set.then_some(flag))
        .collect()
    }
}

/// Enqueue a typed `bn dispatch` instead of launching it.
///
/// The most user-visible capture, so it prints where the work went and how to get it back — a
/// command that used to open a window and now prints one line has to say why, or it reads as a
/// failure.
///
/// Returns `false` when the work was **not** taken, which the caller reads as "launch it directly":
/// no repo to book it against, an unrepresentable flag, or a failed write.
pub(crate) fn capture_dispatch(cfg: &Config, req: &DispatchRequest<'_>, json: bool) -> bool {
    let DispatchRequest {
        item, scout, skill, harness, ..
    } = *req;
    let unsupported = req.unsupported();
    if !unsupported.is_empty() {
        // Loud, because the alternative is the bug this whole type exists to prevent. It runs now
        // instead — the right outcome — but the operator should know the queue was bypassed.
        eprintln!(
            "bn dispatch: running directly — the queue cannot yet carry {}",
            unsupported.join(", ")
        );
        return false;
    }
    let repo = match req.repo {
        Some(r) => r.to_string(),
        None => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match bn_core::resolver::resolve_hint(cfg, &cwd) {
                Some(ctx) => ctx.repo,
                // No repo to book it against, so there is nothing to enqueue. Returning false hands
                // it back to the direct path rather than guessing a repo.
                None => return false,
            }
        }
    };
    let kind = match (scout, skill) {
        (_, Some("investigate")) => BacklogKind::Investigate,
        (true, _) => BacklogKind::Scout,
        _ => BacklogKind::Ship,
    };
    let exec = match skill {
        Some(s) => ExecSpec::Skill {
            skill: s.to_string(),
            arg: None,
        },
        None => ExecSpec::Agent,
    };
    let now = bn_core::util::now_epoch_secs();
    let spec = AddSpec {
        kind,
        // A person typed this, so it outranks anything scheduled — but not an urgent attention
        // interrupt, which is the one thing that should still cut ahead of a human's own request.
        priority: 100,
        source: "dispatch".to_string(),
        task: match req.machine {
            true => crate::cmd::task_mint::task_for_machine_enqueue(cfg, &repo, item, "dispatch"),
            false => crate::cmd::task_mint::task_for_enqueue(cfg, &repo, item, "dispatch"),
        },
        // Durable, so the tick classifies the entry as what it was declared to be rather than
        // re-deriving provenance from a subject line it cannot read.
        machine: req.machine,
        exec,
        requires: Requirements {
            harness: harness.map(|h| vec![h.to_string()]).unwrap_or_default(),
            ..Default::default()
        },
        // The identity travels with the entry, so the tick launches the work that was declared
        // rather than one rebuilt from its subject line.
        agent: bn_core::backlog::AgentSpec {
            branch: req.branch.map(str::to_string),
            name: req.name.map(str::to_string),
            yolo: req.yolo,
            no_plan_review: req.no_plan_review,
            with_main: req.with_main,
            stage: req.stage,
        },
        ..Default::default()
    };
    match enqueue(&backlog::backlog_path(), &repo, item, &spec, now) {
        Ok(e) => {
            // Under `--json` this must emit an envelope, not prose. Every API caller — the
            // dashboard's setup and dispatch buttons among them — reads the last JSON line of
            // stdout; printing only human text made a *queued* dispatch indistinguishable from a
            // *launched* one, so the UI kept reporting "starting an agent…" for work that had gone
            // into the backlog and would not run for hours.
            if json {
                let data = serde_json::json!({
                    "repo": repo,
                    "id": e.entry().id,
                    "item": item,
                    "kind": kind.label(),
                    "dispatched": false,
                    "queued": e.is_new(),
                    "priority": e.entry().priority,
                    "status": e.entry().status.label(),
                });
                println!("{}", Envelope::success("dispatch", data).to_json());
            } else {
                let verb = if e.is_new() { "queued" } else { "already queued" };
                println!("{verb} {repo}/{} · {} · priority 100", e.entry().id, kind.label());
                println!("  the tick starts it when there is capacity — `bn queue status`, or");
                println!("  `bn dispatch --on-pool-full fail …` to run one directly.");
            }
            true
        }
        Err(err) => {
            eprintln!("bn dispatch: could not queue — {err}");
            false
        }
    }
}

/// When a scheduled job stops being worth running.
///
/// Its own next firing. Once tomorrow's 09:00 digest is due, today's has been superseded — running
/// it then is worse than not running it. That makes the schedule, rather than an arbitrary grace
/// period, the thing that decides how late is too late.
///
/// `None` for a job with no enumerable schedule: with nothing to supersede it, there is no moment
/// to miss, so it waits for capacity like any other work.
fn schedule_deadline(on_calendar: Option<&str>) -> Option<i64> {
    let spec = on_calendar?;
    bn_core::staff_schedule::next_occurrence(spec, bn_core::util::now_epoch_secs())
}

/// The `on_calendar` a declared cron carries for `job`, if any.
fn job_schedule(cfg: &Config, job: &str) -> Option<String> {
    bn_core::crons::discover(cfg, None)
        .into_iter()
        .find(|c| c.entry.job == job)
        .map(|c| c.entry.on_calendar)
        .filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_agents_reenter_by_name_and_context() {
        let ctx = Resolved {
            repo: "demo".to_string(),
            branch: "feature".to_string(),
        };

        assert_eq!(
            named_job_exec("custom-agent", Some(&ctx), None),
            ExecSpec::Job {
                job: "custom-agent".to_string(),
                repo: Some("demo".to_string()),
                branch: Some("feature".to_string()),
            }
        );
    }

    /// The worktree a person ran it from beats what the manifest declares — they pointed at it.
    #[test]
    fn the_invocation_context_outranks_the_manifest() {
        let ctx = Resolved {
            repo: "demo".to_string(),
            branch: "feature".to_string(),
        };
        let exec = named_job_exec("custom-agent", Some(&ctx), Some("bn".to_string()));
        assert!(matches!(exec, ExecSpec::Job { repo, .. } if repo.as_deref() == Some("demo")));
    }

    /// A headless firing has no context, and used to therefore claim no repo at all — which is
    /// what put its window in a session named after the queue's sentinel.
    #[test]
    fn a_headless_firing_falls_back_to_the_repo_the_manifest_declares() {
        let exec = named_job_exec("pool-warm", None, Some("bn".to_string()));
        assert_eq!(
            exec,
            ExecSpec::Job {
                job: "pool-warm".to_string(),
                repo: Some("bn".to_string()),
                // No ctx, no branch: the manifest names a repo and nothing more.
                branch: None,
            }
        );
    }
}

#[cfg(test)]
mod capture_tests {
    //! The lossless-capture invariant, tested directly. It is the rule that turns "we remembered to
    //! carry the branch" into "we cannot fail to notice that we didn't".
    use super::*;

    /// A plain dispatch, with every field stated.
    ///
    /// Spelled out rather than defaulted for the same reason the type has no [`Default`]: adding a
    /// dispatch flag should fail to compile here too, so the test that guards the invariant is
    /// forced to say whether the new flag is carried or refused.
    fn req<'a>() -> DispatchRequest<'a> {
        DispatchRequest {
            repo: Some("nvim"),
            item: "set up nvim",
            scout: false,
            skill: None,
            harness: None,
            branch: None,
            name: None,
            yolo: false,
            no_plan_review: false,
            with_main: false,
            machine: false,
            fresh: false,
            verify: false,
            plan: false,
            orchestrator: false,
            stage: None,
        }
    }

    #[test]
    fn identity_the_queue_carries_is_not_a_reason_to_bypass_it() {
        let r = DispatchRequest {
            branch: Some("repo-setup"),
            name: Some("repo-setup"),
            yolo: true,
            // The repo-setup lane's stance: autonomous, and explicitly not behind the plan-review
            // gate. Both are durable on the entry, so neither is a reason to skip the queue.
            no_plan_review: true,
            with_main: true,
            harness: Some("copilot"),
            ..req()
        };
        assert!(
            r.unsupported().is_empty(),
            "branch, name, yolo, no-plan-review and with-main are durable — capture must still happen"
        );
    }

    #[test]
    fn a_flag_the_queue_cannot_express_sends_the_work_down_the_direct_path() {
        // The failure this prevents is silent, so the test is about *naming* the gap rather than
        // about any particular flag: whatever cannot be carried must be reported, never dropped.
        for r in [
            DispatchRequest { fresh: true, ..req() },
            DispatchRequest { verify: true, ..req() },
            DispatchRequest { plan: true, ..req() },
            DispatchRequest { orchestrator: true, ..req() },
        ] {
            assert_eq!(
                r.unsupported().len(),
                1,
                "an unrepresentable flag must be named, not silently dropped"
            );
        }
    }

    #[test]
    fn a_plain_dispatch_is_captured() {
        assert!(req().unsupported().is_empty());
    }

    /// The env marker is read the same way on both enqueue doors, and only an explicit truthy value
    /// counts. `BN_MACHINE_DISPATCH=0` is a caller saying *no* — it must not read as "set, therefore
    /// machine", which is the shape of bug that turns an opt-in into "this machine has run one".
    #[test]
    fn only_an_explicit_marker_declares_machine_work() {
        let _lock = bn_core::test_support::env_lock();
        let guard = bn_core::test_support::EnvGuard::remove("BN_MACHINE_DISPATCH");
        assert!(!machine_owned(), "unset is not a declaration");

        for (value, want) in [("", false), ("   ", false), ("0", false), ("1", true), (" 1 ", true)]
        {
            guard.set_value(value);
            assert_eq!(machine_owned(), want, "BN_MACHINE_DISPATCH={value:?}");
        }
    }
}
