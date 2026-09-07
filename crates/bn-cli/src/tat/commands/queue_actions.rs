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
use bn_core::wt_actions::{When, WtAction};

/// Offer the queue actions for `action` on `repo`, and carry out the chosen one.
pub(super) fn menu(cfg: &Config, action: &WtAction, repo: &str) {
    // The entry lives under the repo it targets — the `machine` sentinel was retired in #1317.
    let id = crate::cmd::wt_action_jobs::action_entry_id(action, repo, None);
    let filed = bn_core::backlog::latest(&bn_core::backlog::backlog_path(), repo, &id)
        .filter(|e| e.status.is_active());
    let choices = options(cfg, filed.is_some());
    let header = match &filed {
        Some(e) => format!("{} — {} ({})", action.label, repo, e.status.label()),
        None => format!("{} — {repo}", action.label),
    };
    let Some(choice) = pick_one(&choices, &header) else {
        return; // dismissed
    };
    let now = bn_core::util::now_epoch_secs();
    let outcome = match choice.as_str() {
        "now" if filed.is_some() => {
            bn_core::backlog::start_next(&bn_core::backlog::backlog_path(), repo, &id, now)
                .map(|done| match done {
                    true => format!("{} is next", action.label),
                    false => format!("{} could not be moved", action.label),
                })
                .map_err(|e| e.to_string())
        }
        "now" => queue(cfg, action, repo, When::Now),
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
fn options(cfg: &Config, filed: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    out.push(match filed {
        true => (
            "now".to_string(),
            "Start it now — jump the queue".to_string(),
        ),
        false => ("now".to_string(), "Queue it to run now".to_string()),
    });
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

    /// The menu grows with the machine's schedule — a window someone adds shows up without this
    /// module being taught about it, which is the whole reason it reads config rather than a list.
    #[test]
    fn configured_windows_become_menu_entries() {
        let cfg = cfg_with(vec![
            window("pool-warm-lunch", WindowMode::Away),
            window("nightly", WindowMode::Deadline),
        ]);
        assert_eq!(
            ids(&options(&cfg, false)),
            vec!["now", "away", "pool-warm-lunch", "nightly"]
        );
    }

    /// A `prefer` window says which work to rank higher, not whether it may run at all, so holding
    /// a verb until one opens would delay it and change nothing.
    #[test]
    fn preference_only_windows_are_not_offered_as_a_schedule() {
        let cfg = cfg_with(vec![window("busy-hours", WindowMode::Prefer)]);
        assert_eq!(ids(&options(&cfg, false)), vec!["now", "away"]);
    }

    /// A menu that offers what the CLI would refuse is a menu that lies: there is nothing to cancel
    /// before the verb is filed.
    #[test]
    fn cancel_is_offered_only_once_something_is_filed() {
        let cfg = cfg_with(Vec::new());
        assert!(!ids(&options(&cfg, false)).contains(&"cancel"));
        assert!(ids(&options(&cfg, true)).contains(&"cancel"));
    }

    /// And "now" reads differently either side of that line — queueing it, versus jumping a queue
    /// it is already in.
    #[test]
    fn the_now_label_says_which_of_the_two_things_it_will_do() {
        let cfg = cfg_with(Vec::new());
        let unfiled = options(&cfg, false);
        let filed = options(&cfg, true);
        assert!(unfiled[0].1.contains("Queue it"), "{:?}", unfiled[0]);
        assert!(filed[0].1.contains("jump the queue"), "{:?}", filed[0]);
    }

    #[test]
    fn window_names_lose_their_config_prefix_in_a_menu() {
        assert_eq!(pretty("pool-warm-lunch"), "lunch");
        assert_eq!(pretty("nightly"), "nightly");
    }
}
