//! The two task agents: **enhance**, which makes one task executable, and **triage**, which decides
//! what is doable today.
//!
//! Split out of `cmd/task.rs` (which is at the 1000-line limit) because it is one coherent thing:
//! the pair of headless agent runs that move a task through the SMART gate. `cmd/task.rs` owns the
//! store's CRUD; this owns the two places bn spends a model on a task.
//!
//! The load-bearing rule here is that **a verdict is recorded, never merely reported.** Every
//! producer downstream — the queue's admission test, the Today board, the pipeline sweep — reads
//! `smart:`, not this module's stdout. An enhance run that finds a task already passing and only
//! prints so leaves it unjudged, which means unstartable, which means the next run picks the same
//! task again. See [`emit_task_enhance`].
use crate::*;

// --- The two task agents (#1002, EPIC #993) -----------------------------------------------

/// Whether this machine is the one allowed to rewrite the shared task store wholesale.
///
/// Triage restamps `day:` across every open task, so two laptops running it on the same synced
/// file would fight — the union-merge repair in #995 collapses duplicates, but the *decisions*
/// would still flip-flop. `[staff].home` is the shipped "only one machine does this" knob, so
/// reuse it rather than inventing a second one.
fn triage_allowed_here(cfg: &Config) -> bool {
    match cfg.staff_home.as_deref() {
        Some(home) => home == cfg.machine(),
        None => true,
    }
}

struct TaskAgentRun {
    prompt: String,
    launched: bool,
    output: String,
}

/// Strictly render a task agent and run its configured headless harness unless this is a dry run.
fn run_task_agent(cfg: &Config, label: &str, packet: &str, dry_run: bool) -> TaskAgentRun {
    let ctx = target_ctx(cfg, None, None);
    let catalog = bn_core::agents::discover(cfg, ctx.as_ref())
        .unwrap_or_else(|error| fail(&format!("bn task {label}: {error}")));
    let agent = catalog
        .require(label)
        .unwrap_or_else(|error| fail(&format!("bn task {label}: {error}")));
    let mut args = serde_json::Map::new();
    args.insert(
        "packet".into(),
        serde_json::Value::String(packet.to_string()),
    );
    let prompt = agent
        .render(Some(&args))
        .unwrap_or_else(|error| fail(&format!("bn task {label}: {error}")));
    if dry_run {
        return TaskAgentRun {
            prompt,
            launched: false,
            output: String::new(),
        };
    }

    let harness = agent.harness.as_deref().unwrap_or("copilot");
    let Some(cmd) =
        crate::harness_dispatch::harness_command(harness, &prompt, agent.model.as_deref(), None)
    else {
        fail(&format!(
            "bn task {label}: unknown harness '{harness}' (use copilot | claude)"
        ));
    };
    let (success, output, combined) = crate::harness_dispatch::dispatch_capture(cmd, None);
    if !success {
        fail(&format!(
            "bn task {label}: harness failed: {}",
            bn_core::run_status::failure_detail(&combined)
        ));
    }
    TaskAgentRun {
        prompt,
        launched: true,
        output,
    }
}

/// How many already-ready tasks this run scheduled, as a line worth printing.
///
/// Worth saying out loud: these are tasks that became startable without an agent running at all,
/// which is the difference between "the hourly job did nothing" and "the hourly job cleared the
/// backlog it could clear for free".
fn scheduled_line(stamped: usize) -> String {
    match stamped {
        0 => "No new tasks passed the gate.".to_string(),
        1 => "Scheduled 1 task that already passed the gate.".to_string(),
        n => format!("Scheduled {n} tasks that already passed the gate."),
    }
}

/// `bn task enhance <id>` — make one task executable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_task_enhance(
    cfg: &Config,
    id: Option<&str>,
    steer: Option<&str>,
    due: bool,
    agent: &str,
    dry_run: bool,
    as_json: bool,
) {
    let today = today();
    // Stamp everything that already passes before picking anything to work on. Judging is free
    // (pure check + one write, no model), and without it an already-SMART task is never stamped, so
    // it never becomes startable, never leaves the enhance queue, and is re-picked as the
    // highest-ranked candidate on every run — the hourly job spends its whole life re-judging one
    // task and reporting "nothing to improve". Only the `--due` sweep does this: an operator asking
    // for one task by id should not have the rest of the store rewritten underneath them.
    let stamped = if due && !dry_run {
        bn_core::pipeline::gate::stamp_passing(cfg, &today)
    } else {
        0
    };

    let path = task_path(cfg);
    let file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));

    let task = match (id, due) {
        (Some(id), _) => file
            .tasks()
            .find(|t| t.id() == Some(id))
            .cloned()
            .unwrap_or_else(|| fail(&format!("bn task enhance: no task {id}"))),
        // `--due` picks the work the gate actually rejected, highest-ranked first — the queue the
        // idle loop drains. A `stuck` task is excluded: three agents already failed at it, so it
        // is waiting on a person, not on another attempt.
        (None, true) => {
            let open: Vec<&bn_core::todo::Task> = file
                .tasks()
                .filter(|t| t.is_open(&today))
                .filter(|t| !matches!(t.meta("smart"), Some("ok") | Some("stuck")))
                // The gate does not judge machine-minted work, so there is nothing here to
                // improve: enhancing a generated prompt means rewriting a chore nobody chose,
                // three times, and then asking a person to scope it (#1151). Left in, these
                // outnumber real tasks 99 to 20 and the hourly run never reaches yours.
                .filter(|t| !bn_core::task_owner::is_machine(t))
                .collect();
            match bn_core::task_priority::rank(&open, &today).first() {
                Some(t) => (*t).clone(),
                None => {
                    if as_json {
                        let data = serde_json::json!({ "stamped": stamped, "improved": null });
                        println!("{}", Envelope::success("task_enhance", data).to_json());
                    } else {
                        println!("{}", scheduled_line(stamped));
                        println!("Nothing needs improving — every open task has a verdict.");
                    }
                    return;
                }
            }
        }
        (None, false) => fail("bn task enhance: pass a task id, or --due for the next one"),
    };

    let task_id = task.id().unwrap_or("?").to_string();
    // Named explicitly, a machine task still gets the gate's answer rather than an agent: it is
    // exempt from judgement, so there is no verdict to improve and no author to hand it back to.
    if bn_core::task_owner::is_machine(&task) {
        if as_json {
            let data = serde_json::json!({
                "id": task_id, "stamped": stamped, "improved": null, "machine": true,
            });
            println!("{}", Envelope::success("task_enhance", data).to_json());
        } else {
            println!("{task_id} is machine-minted — the SMART gate does not judge it.");
        }
        return;
    }
    // Re-judge now so the packet names the *current* gaps rather than a stale stamp.
    let known = bn_core::issues::known_repos(cfg).into_iter().collect();
    let doc = bn_core::task_doc::read(cfg, &task);
    let verdict = bn_core::smart::check(&task, doc.as_ref(), &known, &today);
    if verdict.ok {
        // Record the pass rather than only reporting it. An unstamped task is indistinguishable
        // from an unjudged one to every producer downstream — the queue, the Today board and the
        // gate all read `smart:`, not this line of output — so leaving it unwritten is what kept
        // ready work sitting in the enhance queue.
        if !dry_run {
            bn_core::pipeline::gate::mark_passing(cfg, &task_id);
        }
        if as_json {
            let data = serde_json::json!({
                "id": task_id, "stamped": stamped.max(1), "improved": null, "smart": "ok",
            });
            println!("{}", Envelope::success("task_enhance", data).to_json());
        } else if dry_run {
            println!("{task_id} passes the SMART gate — would stamp it ready.");
        } else {
            println!("{task_id} passes the SMART gate — stamped ready, nothing to improve.");
        }
        return;
    }

    let captured = bn_core::task_priority::CapturedContext::read(cfg, &task);
    let placements = bn_core::issues::known_repos(cfg);
    let packet =
        bn_core::task_priority::improve_packet(&task, Some(&verdict), steer, &captured, &placements);
    if !dry_run && !as_json {
        println!("{}", scheduled_line(stamped));
        println!("Improving {task_id} — fails on: {}", verdict.codes().join(", "));
    }
    let run = run_task_agent(cfg, agent, &packet, dry_run);

    // Judge the task the agent just rewrote, and record the answer. This is what makes the loop
    // terminate: a pass is stamped `smart:ok` so the task becomes startable in this run rather than
    // an hour later, and a failure counts the attempt — after `MAX_IMPROVE_TRIES` the gate stamps
    // `smart:stuck` and raises the attention item that hands it to a person. Without it the attempt
    // leaves no trace, so an unfixable task is re-picked as the top candidate forever and is never
    // escalated to anyone.
    let outcome = (!dry_run && run.launched)
        .then(|| {
            let fresh = bn_core::todo::load(&path)
                .ok()
                .and_then(|f| f.tasks().find(|t| t.id() == Some(task_id.as_str())).cloned())?;
            Some(bn_core::pipeline::gate::judge(cfg, &fresh, &today))
        })
        .flatten();

    if as_json {
        let data = serde_json::json!({
            "id": task_id,
            "stamped": stamped,
            "gaps": verdict.codes(),
            "prompt": run.prompt,
            "launched": run.launched,
            "output": run.output,
            "verdict": match &outcome {
                Some(bn_core::pipeline::gate::GateDecision::Admitted) => "ok",
                Some(bn_core::pipeline::gate::GateDecision::Refused { stuck: true, .. }) => "stuck",
                Some(bn_core::pipeline::gate::GateDecision::Refused { .. }) => "fail",
                _ => "unjudged",
            },
        });
        println!("{}", Envelope::success("task_enhance", data).to_json());
        return;
    }
    if dry_run {
        println!("{}", run.prompt);
        return;
    }
    if !run.output.trim().is_empty() {
        println!("{}", run.output.trim());
    }
    // Say where the task landed, so an unattended run leaves a log line worth reading.
    match &outcome {
        Some(bn_core::pipeline::gate::GateDecision::Admitted) => {
            println!("{task_id} now passes the gate — scheduled and ready to start.")
        }
        Some(bn_core::pipeline::gate::GateDecision::Refused { stuck: true, tries, .. }) => println!(
            "{task_id} still fails after {tries} attempts — handed to you (smart:stuck)."
        ),
        Some(bn_core::pipeline::gate::GateDecision::Refused { tries, verdict, .. }) => println!(
            "{task_id} still fails on: {} (attempt {tries}/{}).",
            verdict.codes().join(", "),
            bn_core::pipeline::gate::MAX_IMPROVE_TRIES
        ),
        _ => {}
    }
}

/// `bn task triage` — decide what is doable today.
pub(crate) fn emit_task_triage(
    cfg: &Config,
    day: Option<&str>,
    agent: &str,
    dry_run: bool,
    as_json: bool,
) {
    if !dry_run && !triage_allowed_here(cfg) {
        fail(&format!(
            "bn task triage: this machine is not the staff home ({}); triage rewrites the shared store, so it runs in one place",
            cfg.staff_home.as_deref().unwrap_or("?")
        ));
    }
    let day = day.map(str::to_string).unwrap_or_else(today);
    if bn_core::util::epoch_day(&day).is_none() {
        fail("bn task triage: --day must be YYYY-MM-DD");
    }

    let path = task_path(cfg);
    let file = bn_core::todo::load(&path).unwrap_or_else(|e| fail(&format!("bn task: {e}")));
    // Machine bookkeeping is never triaged onto a person's day. Without this the ranking agent
    // stamps `day:` on cron chores — `pre_score` even boosts them, since the gate auto-admits
    // machine tasks as `smart:ok` — and the store accretes dated `auto:1` rows forever after.
    let open: Vec<&bn_core::todo::Task> = file
        .tasks()
        .filter(|t| t.is_open(&day) && !bn_core::task_owner::is_machine(t))
        .collect();
    if open.is_empty() {
        println!("No open tasks to triage — `bn task pull` to refill.");
        return;
    }
    let ranked = bn_core::task_priority::rank(&open, &day);
    let packet = bn_core::task_priority::triage_packet(&ranked, &day, None);
    let shortlist: Vec<serde_json::Value> = ranked
        .iter()
        .take(bn_core::task_priority::TRIAGE_WINDOW)
        .map(|t| {
            serde_json::json!({
                "id": t.id(),
                "summary": t.summary(),
                "score": bn_core::task_priority::pre_score(t, &day),
            })
        })
        .collect();

    if !as_json && !dry_run {
        println!("Ranked shortlist for {day} ({} open):", open.len());
        for t in ranked.iter().take(bn_core::task_priority::TRIAGE_WINDOW) {
            println!(
                "  {:>5}  {}  {}",
                bn_core::task_priority::pre_score(t, &day),
                t.id().unwrap_or("?"),
                t.summary()
            );
        }
    }
    let run = run_task_agent(cfg, agent, &packet, dry_run);
    if as_json {
        let data = serde_json::json!({
            "day": day,
            "shortlist": shortlist,
            "prompt": run.prompt,
            "launched": run.launched,
            "output": run.output,
        });
        println!("{}", Envelope::success("task_triage", data).to_json());
    } else if dry_run {
        println!("{}", run.prompt);
    } else if !run.output.trim().is_empty() {
        println!("{}", run.output.trim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    #[test]
    fn task_enhance_launches_the_copilot_harness_from_the_shared_job_path() {
        let _lock = bn_core::test_support::env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let overlay = temp.path().join("overlay");
        let home = temp.path().join("home");
        let agents = overlay.join("agents");
        let bin = home.join(".local/bin");
        fs::create_dir_all(&agents).expect("agents dir");
        fs::create_dir_all(&bin).expect("copilot dir");

        let copilot = bin.join("copilot");
        fs::write(&copilot, "#!/bin/sh\nprintf 'copilot-stub\\n'\n").expect("copilot stub");
        let mut perms = fs::metadata(&copilot).expect("copilot metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&copilot, perms).expect("copilot executable");

        fs::write(
            agents.join("task-enhance.md"),
            concat!(
                "---\n",
                "name: task-enhance\n",
                "kind: command\n",
                "harness: copilot\n",
                "arguments:\n",
                "  - name: packet\n",
                "    required: true\n",
                "---\n",
                "{{packet}}\n",
            ),
        )
        .expect("task-enhance manifest");

        let _home = bn_core::test_support::EnvGuard::set("HOME", &home);
        let _agents_dir = bn_core::test_support::EnvGuard::set("BN_AGENTS_DIR", &agents);
        let path = temp.path().join("path");
        fs::create_dir_all(&path).expect("path dir");
        let _path = bn_core::test_support::EnvGuard::set("PATH", &path);
        let cfg = Config {
            personal_root: temp.path().join("personal"),
            work_root: temp.path().join("work"),
            overlay_root: Some(overlay),
            hostname: "test-host".to_string(),
            ..Default::default()
        };

        let run = run_task_agent(&cfg, "task-enhance", "hello", false);

        assert!(run.launched, "the harness should launch");
        assert_eq!(run.prompt.trim(), "hello");
        assert_eq!(run.output.trim(), "copilot-stub");
    }
}
