//! `ctrl-o` on a command row — everything you can do to a verb other than press Enter.
//!
//! Enter is deliberately one thing: queue it now, or take it back off if it is already filed. That
//! covers the common case and nothing else, so the moment the queue grew times and windows there
//! was nowhere to say "at lunch" or "actually, start that now".
//!
//! This is that menu, and it is built from the machine's **live** window list rather than a fixed
//! set of labels — a person who runs `bn queue schedule add nightly …` should find `run at nightly`
//! here without anything being taught about it.
//!
//! The options offered depend on whether the verb is already filed, because a menu that lists
//! actions the CLI would refuse is a menu that lies: `bn queue now` does nothing to an entry that
//! does not exist, and "cancel" is meaningless before it is queued.
use super::pick_one;
use crate::*;
use bn_core::queue::window::WindowMode;
use bn_core::resolver::DirectMode;
use bn_core::wt_actions::{When, WtAction};

/// Offer the queue actions for `action` on `repo`, and carry out the chosen one.
pub(super) fn menu(cfg: &Config, action: &WtAction, repo: &str) {
    // The entry lives under the repo it targets — the `machine` sentinel was retired in #1317.
    let id = crate::cmd::wt_action_jobs::action_entry_id(action, repo, None);
    let filed = bn_core::backlog::latest(&bn_core::backlog::backlog_path(), repo, &id)
        .filter(|e| e.status.is_active());
    let opened = bn_core::util::now_epoch_secs();
    let choices = options(cfg, filed.is_some(), direct(cfg, opened));
    let header = match &filed {
        Some(e) => format!("{} — {} ({})", action.label, repo, e.status.label()),
        None => format!("{} — {repo}", action.label),
    };
    let Some(choice) = pick_one(&choices, &header) else {
        return; // dismissed
    };
    let now = bn_core::util::now_epoch_secs();
    let outcome = match choice.as_str() {
        // The opt-out machine: "now" on a filed verb can only move it to the front of the line.
        "now" if filed.is_some() && !direct(cfg, now).is_immediate() => {
            bn_core::backlog::start_next(&bn_core::backlog::backlog_path(), repo, &id, now)
                .map(|done| match done {
                    true => format!("{} is next", action.label),
                    false => format!("{} could not be moved", action.label),
                })
                .map_err(|e| e.to_string())
        }
        // Otherwise "now" means now: file it if it is not filed, and start it from here whether it
        // was waiting for the away window or was just filed — `start_action` handles both.
        "now" => crate::cmd::wt_action_jobs::start_action(cfg, action, repo, None)
            .map(|started| format!("{} — {}", action.label, started.describe())),
        "away" => queue(cfg, action, repo, When::Away),
        "cancel" => bn_core::backlog::cancel(&bn_core::backlog::backlog_path(), repo, &id, now)
            .map(|_| format!("un-queued {}", action.label))
            .map_err(|e| e.to_string()),
        // Everything else is a window name: file it (if it is not filed) and hold it until the
        // window next opens. Both halves, because "run at lunch" on an unqueued verb has to queue
        // it first — a schedule with nothing attached to it is not what anyone meant.
        window => at_window(
            cfg,
            &Target {
                action,
                repo,
                id: &id,
            },
            window,
            filed.is_some(),
            now,
        ),
    };
    match outcome {
        Ok(msg) => println!("✓ {msg}"),
        Err(e) => {
            eprintln!("✗ {e}");
            crate::picker::hold();
        }
    }
}

/// The menu, as `(id, label)` pairs.
///
/// Windows come from config, so this list grows with the machine's schedule. Only `away` and
/// `deadline` windows are offered: those are the two modes that decide *whether* opportunistic work
/// may run, and holding a verb until a merely-preferred window would delay it for no benefit.
fn options(cfg: &Config, filed: bool, direct: DirectMode) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let now_label = match (direct.is_immediate(), filed) {
        (true, true) => "Start it now — skip the queue",
        (true, false) => "Run it now",
        (false, true) => "Start it now — jump the queue",
        (false, false) => "Queue it to run now",
    };
    out.push(("now".to_string(), now_label.to_string()));
    out.push(("away".to_string(), "Run it when I'm away".to_string()));
    for w in cfg
        .queue
        .windows
        .iter()
        .filter(|w| matches!(w.mode, WindowMode::Away | WindowMode::Deadline))
    {
        out.push((w.name.clone(), format!("Run at {}", pretty(&w.name))));
    }
    if filed {
        out.push(("cancel".to_string(), "Take it off the queue".to_string()));
    }
    out
}

/// What Enter and "now" do here, given the machine's setting and the queue's off switch.
///
/// One read of the switch per menu, passed down: [`options`] stays pure so its labels are testable
/// without an event log, which is the same reason [`bn_core::resolver::QueueConfig::direct_mode`]
/// takes the fact rather than reading it.
fn direct(cfg: &Config, now: i64) -> DirectMode {
    cfg.queue.direct_mode(bn_core::queue::off::is_off(now))
}

/// `pool-warm-lunch` → `lunch`. The names are namespaced for the config file's sake; a menu is not
/// the config file.
fn pretty(name: &str) -> &str {
    name.strip_prefix("pool-warm-").unwrap_or(name)
}

fn queue(cfg: &Config, action: &WtAction, repo: &str, when: When) -> Result<String, String> {
    crate::cmd::wt_action_jobs::enqueue_action(cfg, action, repo, None, when)
        .map(|o| format!("{} — {} ({})", action.label, when.label(), o.label()))
}

/// The verb and where its queue entry lives — the four things every action here needs.
///
/// A struct rather than more parameters: `at_window` needs all of them plus a window, a time and
/// whether the entry exists, and that many positional arguments is the point at which a caller
/// swapping two `&str`s compiles and does the wrong thing.
struct Target<'a> {
    action: &'a WtAction,
    repo: &'a str,
    id: &'a str,
}

/// File the verb if needed, then hold it until `window` next opens.
fn at_window(
    cfg: &Config,
    target: &Target<'_>,
    window: &str,
    filed: bool,
    now: i64,
) -> Result<String, String> {
    let (action, repo, id) = (target.action, target.repo, target.id);
    if !filed {
        // Filed as away, not now: a verb you are scheduling for lunch is by definition one you are
        // happy to have run while you are gone, and stamping it foreground would make it start
        // immediately — the opposite of what was asked.
        queue(cfg, action, repo, When::Away)?;
    }
    let at = bn_core::when::parse(window, &cfg.queue.windows, now)
        .map_err(|e| format!("could not schedule {}: {e}", action.label))?;
    bn_core::backlog::snooze(&bn_core::backlog::backlog_path(), repo, id, at, now)
        .map_err(|e| format!("could not schedule {}: {e}", action.label))?;
    Ok(format!(
        "{} runs at {} ({})",
        action.label,
        bn_core::util::clock_hm(at),
        pretty(window)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::queue::window::PriorityWindow;

    fn cfg_with(windows: Vec<PriorityWindow>) -> Config {
        let mut cfg = Config::default();
        cfg.queue.windows = windows;
        cfg
    }

    fn window(name: &str, mode: WindowMode) -> PriorityWindow {
        PriorityWindow {
            name: name.to_string(),
            at: "*-*-* 12:30:00".to_string(),
            secs: 3600,
            boost: 100,
            matches: vec!["*".to_string()],
            mode,
        }
    }

    fn ids(choices: &[(String, String)]) -> Vec<&str> {
        choices.iter().map(|(id, _)| id.as_str()).collect()
    }

    /// The default machine, with the queue on: commands start on the spot.
    fn immediate() -> DirectMode {
        DirectMode::Immediate
    }

    /// The menu grows with the machine's schedule — a window someone adds shows up without this
    /// module being taught about it, which is the whole reason it reads config rather than a list.
    #[test]
    fn configured_windows_become_menu_entries() {
        let cfg = cfg_with(vec![
            window("pool-warm-lunch", WindowMode::Away),
            window("nightly", WindowMode::Deadline),
        ]);
        assert_eq!(
            ids(&options(&cfg, false, immediate())),
            vec!["now", "away", "pool-warm-lunch", "nightly"]
        );
    }

    /// A `prefer` window says which work to rank higher, not whether it may run at all, so holding
    /// a verb until one opens would delay it and change nothing.
    #[test]
    fn preference_only_windows_are_not_offered_as_a_schedule() {
        let cfg = cfg_with(vec![window("busy-hours", WindowMode::Prefer)]);
        assert_eq!(ids(&options(&cfg, false, immediate())), vec!["now", "away"]);
    }

    /// A menu that offers what the CLI would refuse is a menu that lies: there is nothing to cancel
    /// before the verb is filed.
    #[test]
    fn cancel_is_offered_only_once_something_is_filed() {
        let cfg = cfg_with(Vec::new());
        assert!(!ids(&options(&cfg, false, immediate())).contains(&"cancel"));
        assert!(ids(&options(&cfg, true, immediate())).contains(&"cancel"));
    }

    /// And "now" reads differently either side of that line — and either side of the machine's
    /// direct-command setting. By default it starts the verb from here; on a machine that opted
    /// out it can only file it, or move a filed one to the front.
    #[test]
    fn the_now_label_says_which_of_the_things_it_will_do() {
        let cfg = cfg_with(Vec::new());
        assert!(
            cfg.queue.direct_mode(false).is_immediate(),
            "immediate is the default"
        );
        let unfiled = options(&cfg, false, immediate());
        let filed = options(&cfg, true, immediate());
        assert!(unfiled[0].1.contains("Run it now"), "{:?}", unfiled[0]);
        assert!(filed[0].1.contains("skip the queue"), "{:?}", filed[0]);

        let unfiled = options(&cfg, false, DirectMode::Queued);
        let filed = options(&cfg, true, DirectMode::Queued);
        assert!(unfiled[0].1.contains("Queue it"), "{:?}", unfiled[0]);
        assert!(filed[0].1.contains("jump the queue"), "{:?}", filed[0]);
    }

    /// A machine that opted into queueing its commands still reads as immediate while the queue is
    /// switched off — there is nothing left to queue into.
    #[test]
    fn the_off_switch_overrides_the_queue_commands_opt_out() {
        let mut cfg = cfg_with(Vec::new());
        cfg.queue.direct = DirectMode::Queued;
        assert_eq!(cfg.queue.direct_mode(false), DirectMode::Queued);
        assert_eq!(cfg.queue.direct_mode(true), DirectMode::Immediate);
        let unfiled = options(&cfg, false, cfg.queue.direct_mode(true));
        assert!(unfiled[0].1.contains("Run it now"), "{:?}", unfiled[0]);
    }

    #[test]
    fn window_names_lose_their_config_prefix_in_a_menu() {
        assert_eq!(pretty("pool-warm-lunch"), "lunch");
        assert_eq!(pretty("nightly"), "nightly");
    }
}
