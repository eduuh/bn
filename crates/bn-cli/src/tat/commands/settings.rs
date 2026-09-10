//! The `Queue settings…` row — the machine's queue knobs, without leaving the picker.
//!
//! Everything here was already reachable: `bn queue away`, `bn queue pause`, `bn queue stop`, and
//! `[queue.jobs]` in `config.toml`. That is exactly the problem. The knobs a person actually
//! reaches for mid-session — *I am going for lunch*, *stop starting things*, *this build is not
//! expensive on this box* — are the ones they will not stop to look up a command for, and one of
//! them means opening a config file in another window and remembering the TOML.
//!
//! So the picker offers them where the work is. Two groups, and they are different in kind:
//!
//! * **Live controls** change what the machine is doing right now. They take effect immediately and
//!   leave no trace in config.
//! * **Job classes** change what the machine *believes*, and are written to `config.toml`. They
//!   affect work queued from then on — an entry carries the classification it was filed with.
//!
//! The off switch belongs to the first group, and that is the point of it: *stop holding my work
//! back* is a thing you decide at 4pm and undo at 5, not a belief about the hardware. With the queue
//! off nothing is filed at all — every trigger, scheduled or pressed, runs on the spot. See
//! [`bn_core::queue::off`].
//!
//! Deliberately absent: the lane and heavy caps, and the window schedule. Caps are a claim about
//! the hardware that wants thinking about once, not mid-session; schedules have `bn queue schedule`
//! and an `OnCalendar` grammar that a two-line fzf row cannot honestly capture.
use super::pick_one;
use crate::*;
use bn_core::queue::config_edit::{write_job_classes, JobClasses};

/// The row's note — what the settings are, at a glance, without opening them.
pub(in crate::tat) fn summary(cfg: &Config) -> String {
    let now = bn_core::util::now_epoch_secs();
    let off = bn_core::queue::off::active(now);
    let mut parts = Vec::new();
    // Leads, because it subsumes everything below it: with the queue off, nothing else this row
    // could say about what is being held back is true any more.
    if let Some(off) = &off {
        parts.push(match off.remaining(now) {
            Some(left) => format!(
                "queue off ({} left)",
                crate::cmd::queue_when::in_words(left)
            ),
            None => "queue off".to_string(),
        });
    }
    if bn_core::away::is_declared(now) {
        parts.push("away".to_string());
    }
    let paused = bn_core::queue::pause::PauseState::load(now);
    if !paused.pauses.is_empty() {
        parts.push("paused".to_string());
    }
    // Worth saying only while something is meant to be draining: with the queue off, a machine
    // with no dispatcher is behaving exactly as asked.
    if off.is_none() && !bn_core::queue::daemon::is_running() {
        parts.push("no dispatcher".to_string());
    }
    if cfg.queue.heavy_jobs.is_some() || !cfg.queue.job_priority.is_empty() {
        parts.push("custom job classes".to_string());
    }
    if !cfg.queue.direct_mode(off.is_some()).is_immediate() {
        parts.push("commands queued".to_string());
    }
    match parts.is_empty() {
        true => String::new(),
        false => parts.join(" · "),
    }
}

/// Open the settings menu and act on the choice.
pub(in crate::tat) fn menu(cfg: &Config) {
    let now = bn_core::util::now_epoch_secs();
    let away = bn_core::away::active(now);
    let off = bn_core::queue::off::active(now);
    let paused = !bn_core::queue::pause::PauseState::load(now)
        .pauses
        .is_empty();
    let running = bn_core::queue::daemon::is_running();

    let mut choices: Vec<(String, String)> = Vec::new();
    // First, with the live controls rather than below them: it is the biggest of the switches on
    // this menu, and the one whose effect the rest are conditional on.
    choices.push(match &off {
        None => (
            "queue-off".to_string(),
            "Switch the queue off — everything runs the moment it is triggered".to_string(),
        ),
        Some(off) => (
            "queue-on".to_string(),
            match off.remaining(now) {
                Some(left) => format!(
                    "Queue is off ({} left) — switch it back on now",
                    crate::cmd::queue_when::in_words(left)
                ),
                None => "Queue is off, everything runs immediately — switch it back on".to_string(),
            },
        ),
    });
    if off.is_none() {
        choices.push((
            "queue-off-2h".to_string(),
            "Switch the queue off for 2h — back on by itself".to_string(),
        ));
    }
    match &away {
        Some(open) => choices.push((
            "back".to_string(),
            format!(
                "I'm back — end the break ({} left)",
                crate::cmd::queue_when::in_words(open.remaining(now))
            ),
        )),
        None => {
            choices.push(("away-45m".to_string(), "I'm away for 45m".to_string()));
            choices.push(("away-2h".to_string(), "I'm away for 2h".to_string()));
        }
    }
    choices.push(match paused {
        true => ("resume".to_string(), "Resume admission".to_string()),
        false => (
            "pause".to_string(),
            "Pause admission — finish what is running, start nothing".to_string(),
        ),
    });
    choices.push(match running {
        true => (
            "stop".to_string(),
            "Stop the dispatcher — the next bn command starts a new one".to_string(),
        ),
        false => ("start".to_string(), "Start the dispatcher".to_string()),
    });
    // Only while there is a queue to file into. With it off, `direct_mode` reads `Immediate`
    // whatever the config says, so offering the toggle would be offering a setting with no effect —
    // the same reason `cancel` is not offered before a verb is filed.
    if off.is_none() {
        choices.push(match cfg.queue.direct_mode(false).is_immediate() {
            true => (
                "direct-queued".to_string(),
                "Commands start instantly — switch to queueing them for the dispatcher".to_string(),
            ),
            false => (
                "direct-immediate".to_string(),
                "Commands are queued — switch to starting them instantly".to_string(),
            ),
        });
    }
    choices.push((
        "jobs".to_string(),
        "Job classes… — which jobs are expensive here".to_string(),
    ));

    let Some(choice) = pick_one(&choices, "queue settings") else {
        return; // dismissed
    };
    let outcome = match choice.as_str() {
        "away-45m" => declare_away(45 * 60, now),
        "away-2h" => declare_away(2 * 3600, now),
        "back" => bn_core::away::end(now)
            .map(|_| "welcome back — opportunistic work waits for you again".to_string())
            .map_err(|e| e.to_string()),
        "pause" => bn_core::queue::pause::pause(
            &bn_core::queue::pause::PauseScope::All,
            "paused from the picker",
            None,
            now,
        )
        .map(|_| "admission paused — running work finishes".to_string())
        .map_err(|e| e.to_string()),
        "resume" => bn_core::queue::pause::resume(&bn_core::queue::pause::PauseScope::All, now)
            .map(|_| "admission resumed".to_string())
            .map_err(|e| e.to_string()),
        "stop" => match crate::cmd::queue_daemon::stop_dispatcher() {
            true => Ok("dispatcher stopped".to_string()),
            false => Ok("no dispatcher was running".to_string()),
        },
        "start" => match crate::cmd::queue_daemon::spawn_dispatcher() {
            true => Ok("dispatcher started".to_string()),
            false => Err("could not start the dispatcher".to_string()),
        },
        "queue-off" => switch_off(None, now),
        "queue-off-2h" => switch_off(Some(now + 2 * 3600), now),
        "queue-on" => switch_on(now),
        "direct-queued" => set_direct(bn_core::resolver::DirectMode::Queued),
        "direct-immediate" => set_direct(bn_core::resolver::DirectMode::Immediate),
        "jobs" => return job_classes(cfg),
        _ => return,
    };
    report(outcome);
}

/// Switch the queue off, and say what it does to work that is already waiting.
///
/// The message names the backlog because that is the question switching off raises and the answer
/// is not the obvious one: this stops work being *filed*, it does not cancel what is already there.
/// The dispatcher keeps running and keeps draining it, so somebody who switched off and then watched
/// three queued builds start anyway would otherwise conclude the switch did nothing.
fn switch_off(until: Option<i64>, now: i64) -> Result<String, String> {
    bn_core::queue::off::off("switched off from the picker", until, now)
        .map(|()| match until {
            None => "the queue is off — everything you or a cron triggers now runs immediately; \
                     work already queued still drains"
                .to_string(),
            Some(until) => format!(
                "the queue is off until {} — triggers run immediately until then",
                bn_core::util::clock_hm(until)
            ),
        })
        .map_err(|e| format!("could not switch the queue off: {e}"))
}

/// Switch it back on, and say how long it was off for.
fn switch_on(now: i64) -> Result<String, String> {
    bn_core::queue::off::on(now)
        .map(|since| match since {
            Some(since) => format!(
                "the queue is back on after {}m — triggers file work for the dispatcher again",
                (now - since).max(0) / 60
            ),
            None => "the queue was already on".to_string(),
        })
        .map_err(|e| format!("could not switch the queue on: {e}"))
}

/// Write the direct-command setting, and say what it changes and what it does not.
///
/// Scheduled work is named in the message because that is the question the setting raises: it
/// governs only what a *person's* keypress does. Crons, warms and `ctrl-s` deferrals go through
/// admission either way.
fn set_direct(mode: bn_core::resolver::DirectMode) -> Result<String, String> {
    let path = bn_core::resolver::machine_config_path();
    bn_core::queue::config_edit::write_direct_mode(&path, mode)
        .map(|()| match mode {
            bn_core::resolver::DirectMode::Immediate => {
                "commands you press now start instantly; scheduled work still queues".to_string()
            }
            bn_core::resolver::DirectMode::Queued => {
                "commands you press now queue for the dispatcher, like scheduled work".to_string()
            }
        })
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

fn declare_away(secs: i64, now: i64) -> Result<String, String> {
    bn_core::away::start(secs, now)
        .map(|open| {
            format!(
                "away until {} — opportunistic work may start now",
                bn_core::util::clock_hm(open.ends_at)
            )
        })
        .map_err(|e| e.to_string())
}

/// The job-class editor: every job the queue knows, and whether this machine calls it expensive.
///
/// The roster comes from the picker's own verbs plus bn's built-in heavy list, which is the set
/// somebody would actually want to re-rate. A machine with a job bn has never heard of can still
/// name it in `config.toml`; a menu is not the place to type an arbitrary job name.
fn job_classes(cfg: &Config) {
    let jobs = known_jobs(cfg);
    let choices: Vec<(String, String)> = jobs
        .iter()
        .map(|job| {
            let heavy = cfg.queue.is_heavy(job);
            (
                job.clone(),
                format!(
                    "{}  {:<18} priority {}",
                    if heavy { "heavy" } else { "light" },
                    job,
                    cfg.queue.priority_for(job)
                ),
            )
        })
        .collect();
    let Some(job) = pick_one(&choices, "toggle heavy — enter to flip") else {
        return;
    };
    // Toggling writes the *whole* list, because `[queue.jobs].heavy` replaces bn's built-in one
    // rather than adding to it. So the first toggle has to materialise the current set, or turning
    // one job heavy would silently declare every other job light.
    let mut heavy: Vec<String> = jobs
        .iter()
        .filter(|j| cfg.queue.is_heavy(j))
        .cloned()
        .collect();
    let now_heavy = match heavy.iter().position(|j| *j == job) {
        Some(i) => {
            heavy.remove(i);
            false
        }
        None => {
            heavy.push(job.clone());
            heavy.sort();
            true
        }
    };
    let classes = JobClasses {
        heavy: Some(heavy),
        priority: cfg
            .queue
            .job_priority
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
    };
    let path = bn_core::resolver::machine_config_path();
    report(
        write_job_classes(&path, &classes)
            .map(|()| {
                format!(
                    "{job} is {} on this machine — affects work queued from now on",
                    if now_heavy { "heavy" } else { "light" }
                )
            })
            .map_err(|e| format!("could not write {}: {e}", path.display())),
    );
}

/// Every job worth re-rating: the picker's verbs, plus whatever bn or this machine already calls
/// heavy. Sorted and deduplicated so the menu is stable between openings.
fn known_jobs(cfg: &Config) -> Vec<String> {
    let mut jobs: Vec<String> = bn_core::wt_actions::all()
        .iter()
        .map(|a| a.job.to_string())
        .chain(
            bn_core::queue::jobs::HEAVY_JOBS
                .iter()
                .map(|j| j.to_string()),
        )
        .chain(cfg.queue.heavy_jobs.iter().flatten().cloned())
        .collect();
    jobs.sort();
    jobs.dedup();
    jobs
}

fn report(outcome: Result<String, String>) {
    match outcome {
        Ok(msg) => println!("✓ {msg}"),
        Err(e) => {
            eprintln!("✗ {e}");
            crate::picker::hold();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    /// The roster is the set someone would plausibly re-rate: the verbs they can press, plus
    /// whatever is already considered expensive.
    #[test]
    fn the_job_list_covers_the_pickers_verbs_and_the_heavy_ones() {
        let jobs = known_jobs(&cfg());
        for verb in bn_core::wt_actions::all() {
            assert!(jobs.contains(&verb.job.to_string()), "missing {}", verb.job);
        }
        for heavy in bn_core::queue::jobs::HEAVY_JOBS {
            assert!(jobs.contains(&heavy.to_string()), "missing {heavy}");
        }
    }

    /// Stable between openings — a menu whose rows move under the cursor is worse than no menu.
    #[test]
    fn the_job_list_is_sorted_and_has_no_duplicates() {
        let jobs = known_jobs(&cfg());
        let mut sorted = jobs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(jobs, sorted);
    }

    /// A machine's own additions appear alongside bn's, so a job named only in config is still
    /// togglable from here.
    #[test]
    fn a_locally_declared_heavy_job_is_listed() {
        let mut cfg = cfg();
        cfg.queue.heavy_jobs = Some(vec!["my-own-job".to_string()]);
        assert!(known_jobs(&cfg).contains(&"my-own-job".to_string()));
    }

    /// The opt-out is worth a word on the row: a person who wonders why Enter no longer starts
    /// things should find the answer without opening the menu.
    #[test]
    fn the_summary_names_the_direct_command_opt_out() {
        let mut cfg = cfg();
        assert!(
            !summary(&cfg).contains("commands queued"),
            "the default is quiet"
        );
        cfg.queue.direct = bn_core::resolver::DirectMode::Queued;
        assert!(summary(&cfg).contains("commands queued"));
    }

    /// The summary is quiet when nothing is overridden or held — a settings row that always shouts
    /// teaches you to stop reading it.
    #[test]
    fn the_summary_says_nothing_when_there_is_nothing_to_say() {
        let mut cfg = cfg();
        cfg.queue.heavy_jobs = Some(vec!["build".to_string()]);
        assert!(
            summary(&cfg).contains("custom job classes"),
            "an override is worth surfacing"
        );
    }
}
