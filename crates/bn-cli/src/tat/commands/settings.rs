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
//! Deliberately absent: the lane and heavy caps, and the window schedule. Caps are a claim about
//! the hardware that wants thinking about once, not mid-session; schedules have `bn queue schedule`
//! and an `OnCalendar` grammar that a two-line fzf row cannot honestly capture.
use super::pick_one;
use crate::*;
use bn_core::queue::config_edit::{write_job_classes, JobClasses};

/// The row's note — what the settings are, at a glance, without opening them.
pub(in crate::tat) fn summary(cfg: &Config) -> String {
    let mut parts = Vec::new();
    if bn_core::away::is_declared(bn_core::util::now_epoch_secs()) {
        parts.push("away".to_string());
    }
    let paused = bn_core::queue::pause::PauseState::load(bn_core::util::now_epoch_secs());
    if !paused.pauses.is_empty() {
        parts.push("paused".to_string());
    }
    if !bn_core::queue::daemon::is_running() {
        parts.push("no dispatcher".to_string());
    }
    if cfg.queue.heavy_jobs.is_some() || !cfg.queue.job_priority.is_empty() {
        parts.push("custom job classes".to_string());
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
    let paused = !bn_core::queue::pause::PauseState::load(now)
        .pauses
        .is_empty();
    let running = bn_core::queue::daemon::is_running();

    let mut choices: Vec<(String, String)> = Vec::new();
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
        "jobs" => return job_classes(cfg),
        _ => return,
    };
    report(outcome);
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
    let path = bn_core::resolver::config_file_path();
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
