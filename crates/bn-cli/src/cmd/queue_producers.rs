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
        warn_dispatcher_down(producer);
    }
    false
}

/// Say once per process that the queue is configured but not draining. Silence here would be the
/// whole failure mode: work quietly launching direct while the operator believes it is queued.
fn warn_dispatcher_down(producer: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SAID: AtomicBool = AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        eprintln!(
            "bn: '{producer}' would queue, but no tick has run in {}m — running it directly. \
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
        task: crate::cmd::task::task_for_enqueue(cfg, repo, url, "review"),
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
    let recovery = bn_core::queue::jobs::is_recovery(name);
    let heavy = bn_core::queue::jobs::is_heavy(name);
    let repo = cfg.queue_home_repo();
    let now = bn_core::util::now_epoch_secs();
    let subject = format!("run {name}");
    let spec = AddSpec {
        kind: BacklogKind::Cron,
        // Scheduled work is the floor: it should run, but never ahead of work a person is waiting
        // on. A negative priority says that once, here, instead of at every call site — except for
        // recovery, which has to outrank a backlog of routine crons to be worth exempting at all.
        priority: bn_core::queue::jobs::priority_for(name),
        source: format!("cron:{name}"),
        task: crate::cmd::task::task_for_enqueue(cfg, &repo, &subject, "cron"),
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
        &subject,
        enqueue(&backlog::backlog_path(), &repo, &subject, &spec, now),
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

fn named_job_exec(name: &str, ctx: Option<&Resolved>) -> ExecSpec {
    ExecSpec::Job {
        job: name.to_string(),
        repo: ctx.map(|ctx| ctx.repo.clone()),
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
        named_job_exec(&agent.name, ctx),
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
        named_job_exec(job, ctx),
        Requirements {
            capabilities: home.map(|h| vec![format!("home:{h}")]).unwrap_or_default(),
            ..Default::default()
        },
        schedule_deadline(job_schedule(cfg, job).as_deref()),
    ))
}

/// Enqueue a typed `bn dispatch` instead of launching it.
///
/// The most user-visible capture, so it prints where the work went and how to get it back — a
/// command that used to open a window and now prints one line has to say why, or it reads as a
/// failure.
pub(crate) fn capture_dispatch(
    cfg: &Config,
    repo: Option<&str>,
    item: &str,
    scout: bool,
    skill: Option<&str>,
    harness: Option<&str>,
) -> bool {
    let repo = match repo {
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
        task: crate::cmd::task::task_for_enqueue(cfg, &repo, item, "dispatch"),
        exec,
        requires: Requirements {
            harness: harness.map(|h| vec![h.to_string()]).unwrap_or_default(),
            ..Default::default()
        },
        ..Default::default()
    };
    match enqueue(&backlog::backlog_path(), &repo, item, &spec, now) {
        Ok(e) => {
            let verb = if e.is_new() { "queued" } else { "already queued" };
            println!("{verb} {repo}/{} · {} · priority 100", e.entry().id, kind.label());
            println!("  the tick starts it when there is capacity — `bn queue status`, or");
            println!("  `bn dispatch --on-pool-full fail …` to run one directly.");
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
            named_job_exec("custom-agent", Some(&ctx)),
            ExecSpec::Job {
                job: "custom-agent".to_string(),
                repo: Some("demo".to_string()),
                branch: Some("feature".to_string()),
            }
        );
    }
}
