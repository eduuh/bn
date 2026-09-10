//! Saying *when* — `bn queue at`, `now`, `away`, and `schedule`.
//!
//! The queue could already express "how long from now" and nothing else. There was no way to name a
//! time, no way to name a window, no way to un-snooze at any layer, and no command at all for the
//! `[[queue.windows]]` that decide what runs at lunch — the only write path was the dashboard's
//! pool form, which could set exactly two built-in windows.
//!
//! So: four verbs, and between them the answer to "run the build at lunch while I'm away, and if I
//! take a break early, start it then".
use crate::cli::queue_cmd::ScheduleCmd;
use crate::*;
use bn_core::backlog;

/// `bn queue at <id> <when>` — hold an entry until a time.
pub(crate) fn emit_at(cfg: &Config, id: &str, when: &str, repo: Option<&str>, now: i64, json: bool) {
    let at = match bn_core::when::parse(when, &cfg.queue.windows, now) {
        Ok(at) => at,
        Err(e) => fail(&format!("bn queue at: {e}")),
    };
    let repo = repo
        .map(str::to_string)
        .unwrap_or_else(|| resolve_or_exit(cfg).repo);
    crate::cmd::queue_scope::require_repo_allowed(cfg, &repo, json);
    let done = backlog::snooze(&backlog::backlog_path(), &repo, id, at, now)
        .unwrap_or_else(|e| fail(&format!("bn queue at: could not update backlog: {e}")));
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-at",
                serde_json::json!({ "repo": repo, "id": id, "at": at, "changed": done }),
            )
            .to_json()
        );
    } else if done {
        println!(
            "{repo}/{id} holds until {} ({})",
            bn_core::util::clock_hm(at),
            in_words(at - now)
        );
    } else {
        println!("{repo}/{id} unchanged (absent, not pending, or already held there)");
    }
}

/// `bn queue now <id>` — start it as soon as the machine can.
pub(crate) fn emit_now(cfg: &Config, id: &str, repo: Option<&str>, now: i64, json: bool) {
    let repo = repo
        .map(str::to_string)
        .unwrap_or_else(|| resolve_or_exit(cfg).repo);
    crate::cmd::queue_scope::require_repo_allowed(cfg, &repo, json);
    let done = backlog::start_next(&backlog::backlog_path(), &repo, id, now)
        .unwrap_or_else(|e| fail(&format!("bn queue now: could not update backlog: {e}")));
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-now",
                serde_json::json!({ "repo": repo, "id": id, "changed": done }),
            )
            .to_json()
        );
        return;
    }
    if !done {
        println!("{repo}/{id} unchanged (absent, or no longer pending)");
        return;
    }
    println!("{repo}/{id} is next — snooze cleared, foreground, top of the queue.");
    // Saying this matters: `now` moves the entry to the front of the *queue*, which is not the same
    // as starting it. Capacity, a pause and the machine's guards all still apply, and a row that
    // then sits would otherwise read as the command having failed.
    if !bn_core::queue::daemon::is_running() {
        println!("  no dispatcher is running, so nothing will pick it up yet — `bn queue run`.");
    }
}

/// `bn queue away --for 45m` / `--back` — declare or end an absence.
pub(crate) fn emit_away(for_duration: &str, back: bool, now: i64, json: bool) {
    if back {
        let ended = bn_core::away::end(now)
            .unwrap_or_else(|e| fail(&format!("bn queue away: could not record it: {e}")));
        if json {
            println!(
                "{}",
                Envelope::success("queue-away", serde_json::json!({ "away": false })).to_json()
            );
        } else {
            match ended {
                Some(_) => println!("welcome back — opportunistic work waits for you again."),
                None => println!("you were not marked away."),
            }
        }
        return;
    }
    let Some(secs) = bn_core::focus::parse_duration(for_duration).filter(|s| *s > 0) else {
        fail("bn queue away: --for must be a positive duration like 45m, 2h, or 1d");
    };
    let open = bn_core::away::start(secs, now)
        .unwrap_or_else(|e| fail(&format!("bn queue away: could not record it: {e}")));
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-away",
                serde_json::json!({ "away": true, "ends_at": open.ends_at }),
            )
            .to_json()
        );
        return;
    }
    println!(
        "away until {} — opportunistic work may start now.",
        bn_core::util::clock_hm(open.ends_at)
    );
    println!("  `bn queue away --back` when you return.");
}

/// `bn queue schedule` — what runs when.
pub(crate) fn emit_schedule(cfg: &Config, now: i64, json: bool) {
    let windows = &cfg.queue.windows;
    if json {
        let rows: Vec<serde_json::Value> = windows
            .iter()
            .map(|w| {
                serde_json::json!({
                    "name": w.name,
                    "at": w.at,
                    "secs": w.secs,
                    "mode": format!("{:?}", w.mode).to_lowercase(),
                    "boost": w.boost,
                    "matches": w.matches,
                    "open": w.is_open(now),
                    "next": bn_core::schedule::next_occurrence(&w.at, now),
                })
            })
            .collect();
        println!(
            "{}",
            Envelope::success("queue-schedule", serde_json::json!({ "windows": rows })).to_json()
        );
        return;
    }
    if windows.is_empty() {
        println!("no windows configured — `bn queue schedule add <name> --at … --for …`");
        return;
    }
    for w in windows {
        let when = match (w.is_open(now), bn_core::schedule::next_occurrence(&w.at, now)) {
            (true, _) => match w.closes_in(now) {
                Some(left) => format!("open now · {} left", in_words(left)),
                None => "open now".to_string(),
            },
            (false, Some(next)) => {
                format!("next {} ({})", bn_core::util::clock_hm(next), in_words(next - now))
            }
            // A spec bn cannot enumerate is worth saying out loud rather than rendering as blank:
            // it will never open, and the window is dead config.
            (false, None) => "never — bn cannot read this schedule".to_string(),
        };
        println!(
            "{:<22} {:<9} {}",
            w.name,
            format!("{:?}", w.mode).to_lowercase(),
            when
        );
        println!("    {}  ·  +{} to {}", w.at, w.boost, w.matches.join(", "));
    }
}

/// `bn queue schedule add|rm` — edit the machine's windows.
pub(crate) fn emit_schedule_edit(cfg: &Config, action: ScheduleCmd, json: bool) {
    let path = bn_core::resolver::machine_config_path();
    let mut windows = cfg.queue.windows.clone();
    let (name, verb) = match action {
        ScheduleCmd::Add {
            name,
            at,
            for_duration,
            mode,
            matches,
            boost,
        } => {
            if !bn_core::schedule::calendar_supported(&at) {
                fail(&format!(
                    "bn queue schedule add: bn cannot read '{at}' as a schedule — \
                     try `Mon..Fri *-*-* 12:30:00`, `*-*-* 17:30`, or `*:0/15`"
                ));
            }
            let Some(secs) = bn_core::focus::parse_duration(&for_duration).filter(|s| *s > 0) else {
                fail("bn queue schedule add: --for must be a positive duration like 90m or 2h");
            };
            let Some(mode) = bn_core::queue::window::WindowMode::parse(&mode) else {
                fail("bn queue schedule add: --mode must be prefer, watched, away, or deadline");
            };
            let matches = match matches.is_empty() {
                true => vec!["*".to_string()],
                false => matches,
            };
            windows.retain(|w| !w.name.eq_ignore_ascii_case(&name));
            windows.push(bn_core::queue::window::PriorityWindow {
                name: name.clone(),
                at,
                secs,
                boost,
                matches,
                mode,
            });
            (name, "added")
        }
        ScheduleCmd::Rm { name } => {
            let before = windows.len();
            windows.retain(|w| !w.name.eq_ignore_ascii_case(&name));
            if windows.len() == before {
                fail(&format!("bn queue schedule rm: no window named '{name}'"));
            }
            (name, "removed")
        }
    };
    if let Err(e) = bn_core::queue::window::write_windows(&path, &windows) {
        fail(&format!("bn queue schedule: could not write {}: {e}", path.display()));
    }
    if json {
        println!(
            "{}",
            Envelope::success(
                "queue-schedule",
                serde_json::json!({ "name": name, "action": verb }),
            )
            .to_json()
        );
    } else {
        println!("{verb} window '{name}' in {}", path.display());
    }
}

/// `2h 15m`, `45m`, `30s` — a duration a person reads rather than counts.
pub(crate) fn in_words(secs: i64) -> String {
    let secs = secs.max(0);
    match (secs / 3600, (secs % 3600) / 60) {
        (0, 0) => format!("{secs}s"),
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_the_way_someone_would_say_them() {
        assert_eq!(in_words(30), "30s");
        assert_eq!(in_words(45 * 60), "45m");
        assert_eq!(in_words(2 * 3600), "2h");
        assert_eq!(in_words(2 * 3600 + 15 * 60), "2h 15m");
        assert_eq!(in_words(-5), "0s", "a window that just closed is not negative");
    }
}
