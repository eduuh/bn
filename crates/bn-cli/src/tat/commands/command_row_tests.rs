//! Tests for the command section — split out of `commands.rs` for the 1000-line ratchet.
use super::super::queue_view::Held;
use super::*;

fn kinds(rows: &str) -> Vec<&str> {
    rows.lines().filter_map(|l| l.split('|').next()).collect()
}

fn payloads(rows: &str) -> Vec<&str> {
    rows.lines()
        .filter(|l| l.starts_with("cmd|"))
        .filter_map(|l| l.split('|').nth(1))
        .collect()
}

/// A flat clone has no pool, no worktrees and no PR rows — a commands header over nothing.
#[test]
fn a_non_bare_ref_gets_no_command_section() {
    assert_eq!(
        command_rows(false, CmdAvail::default(), &[], &CmdStates::new(), 0, ""),
        ""
    );
}

#[test]
fn the_section_leads_with_the_separator() {
    let rows = command_rows(true, CmdAvail::default(), &[], &CmdStates::new(), 0, "");
    assert_eq!(kinds(&rows).first(), Some(&"cmdsep"));
}

/// Pools are a primary-root, untagged concept, so offering the pool verbs on a `--root win` or
/// tagged ref would advertise a key that can only refuse.
#[test]
fn an_unpooled_ref_is_offered_no_pool_verbs() {
    let rows = command_rows(true, CmdAvail::default(), &[], &CmdStates::new(), 0, "");
    let ids = payloads(&rows);
    assert!(!ids.contains(&"pool-warm"), "{ids:?}");
    assert!(!ids.contains(&"pool-grow"), "{ids:?}");
    assert!(ids.contains(&"wt-cleanup"), "{ids:?}");
}

#[test]
fn a_pooled_ref_is_offered_both_pool_verbs() {
    let avail = CmdAvail {
        pooled: true,
        cold: 2,
        at_ceiling: false,
    };
    let rows = command_rows(true, avail, &[], &CmdStates::new(), 0, "");
    let ids = payloads(&rows);
    assert!(ids.contains(&"pool-warm"), "{ids:?}");
    assert!(ids.contains(&"pool-grow"), "{ids:?}");
}

/// At the ceiling there is nothing left to cut, so the row would be a lie.
#[test]
fn growing_is_withheld_at_the_ceiling() {
    let avail = CmdAvail {
        pooled: true,
        cold: 0,
        at_ceiling: true,
    };
    let rows = command_rows(true, avail, &[], &CmdStates::new(), 0, "");
    let ids = payloads(&rows);
    assert!(!ids.contains(&"pool-grow"), "{ids:?}");
    assert!(ids.contains(&"pool-warm"), "{ids:?}");
}

#[test]
fn the_warm_row_says_how_much_is_cold() {
    let avail = CmdAvail {
        pooled: true,
        cold: 3,
        at_ceiling: false,
    };
    let rows = command_rows(true, avail, &[], &CmdStates::new(), 0, "");
    let warm = rows
        .lines()
        .find(|l| l.starts_with("cmd|pool-warm|"))
        .unwrap();
    assert!(warm.contains("3 cold"), "{warm}");

    let none = command_rows(
        true,
        CmdAvail {
            pooled: true,
            cold: 0,
            at_ceiling: false,
        },
        &[],
        &CmdStates::new(),
        0,
        "",
    );
    let warm = none
        .lines()
        .find(|l| l.starts_with("cmd|pool-warm|"))
        .unwrap();
    assert!(warm.contains("all slots warm"), "{warm}");
}

/// `|` is the row delimiter, so a display field carrying one would split into the wrong
/// columns and hand the Enter arm a truncated payload rather than failing loudly.
#[test]
fn no_row_smuggles_the_delimiter_into_its_display() {
    let avail = CmdAvail {
        pooled: true,
        cold: 2,
        at_ceiling: false,
    };
    for line in command_rows(true, avail, &[], &CmdStates::new(), 0, "").lines() {
        assert_eq!(line.matches('|').count(), 2, "{line}");
    }
}

/// The separator must not be reachable by typing a branch name, which is the primary way a
/// branch gets leased in this picker. Dashes give a query nothing to match.
#[test]
fn the_separator_holds_no_letters_to_fuzzy_match() {
    assert!(
        !CMD_SEPARATOR
            .chars()
            .any(|c| c.is_alphanumeric() && !"commands".contains(c)),
        "{CMD_SEPARATOR}"
    );
}

/// A verb added to the roster without a line here renders an empty paragraph in the side pane —
/// silently, since a blank line looks like spacing. The roster is the source of truth, so the
/// description table has to keep up with it rather than the other way round.
#[test]
fn every_repo_verb_has_a_description() {
    for a in bn_core::wt_actions::for_scope(bn_core::wt_actions::Scope::Repo) {
        assert!(
            !describe(a.id).is_empty(),
            "{} has no describe() line",
            a.id
        );
    }
}

/// A window and a leased slot both stand in for a worktree — you act on the branch checked out
/// there. The rows that *are* verbs get no menu, or ctrl-o would offer to act on an action.
#[test]
fn every_place_row_maps_to_a_scope_and_no_verb_row_does() {
    use bn_core::wt_actions::Scope;
    for kind in ["worktree", "window", "poolbranch"] {
        assert_eq!(scope_for_row(kind), Some(Scope::Worktree), "{kind}");
    }
    assert_eq!(scope_for_row("remotepr"), Some(Scope::Pr));
    assert_eq!(scope_for_row("poolidle"), Some(Scope::PoolSlot));
    assert_eq!(scope_for_row("cmd"), None);
    assert_eq!(scope_for_row("cmdsep"), None);
}

/// The worktree verb must never render as a row: it needs a focused row to act on, and a row
/// cannot act on another row once the cursor has moved onto it.
#[test]
fn the_command_section_holds_only_repo_verbs() {
    let avail = CmdAvail {
        pooled: true,
        cold: 1,
        at_ceiling: false,
    };
    let rows = command_rows(true, avail, &two_scripts(), &CmdStates::new(), 0, "");
    for line in rows.lines().filter(|l| l.starts_with("cmd|")) {
        let id = line.split('|').nth(1).unwrap();
        let action = scripts::resolve(id).expect(id);
        assert_eq!(action.scope, bn_core::wt_actions::Scope::Repo, "{id}");
    }
    assert!(!rows.contains("wt-verify"), "{rows}");
}

fn two_scripts() -> Vec<scripts::RepoScript> {
    use bn_core::resolver::JobScope;
    vec![
        scripts::RepoScript {
            name: "start".to_string(),
            scope: JobScope::Repo,
            path: "/notes/jobs/bigmono/start.sh".into(),
        },
        scripts::RepoScript {
            name: "e2e".to_string(),
            scope: JobScope::Project,
            path: "/wt/BigMono/main/.bn/jobs/e2e.sh".into(),
        },
    ]
}

/// A repo's own scripts come after the roster — what every repo can do, then what this one
/// can — and before the settings row, which is about the machine rather than the work.
#[test]
fn script_rows_sit_between_the_roster_and_the_settings_row() {
    let rows = command_rows(true, pooled(), &two_scripts(), &CmdStates::new(), 0, "");
    let ids: Vec<&str> = rows.lines().filter_map(|l| l.split('|').nth(1)).collect();
    let last_roster = ids
        .iter()
        .rposition(|id| bn_core::wt_actions::find(id).is_some());
    let first_script = ids.iter().position(|id| id.starts_with("script:"));
    let settings = rows.lines().position(|l| l.starts_with("qset|"));
    assert!(last_roster.unwrap() < first_script.unwrap(), "{ids:?}");
    assert!(first_script.unwrap() < settings.unwrap(), "{ids:?}");
    assert_eq!(kinds(&rows).last(), Some(&"qset"));
}

/// The idle note names the tier, which is where to go to edit the thing; the queue's word
/// still wins it, keyed the way `queue_view` keys a script entry.
#[test]
fn a_script_row_names_its_tier_until_the_queue_has_something_to_say() {
    let idle = command_rows(true, pooled(), &two_scripts(), &CmdStates::new(), 0, "");
    let start = idle
        .lines()
        .find(|l| l.starts_with("cmd|script:start|"))
        .unwrap();
    assert!(start.contains("repo script"), "{start}");
    let e2e = idle
        .lines()
        .find(|l| l.starts_with("cmd|script:e2e|"))
        .unwrap();
    assert!(e2e.contains("project script"), "{e2e}");

    let mut states = CmdStates::new();
    states.insert(scripts::id("start"), CmdState::Running(9));
    let busy = command_rows(true, pooled(), &two_scripts(), &states, 0, "");
    let start = busy
        .lines()
        .find(|l| l.starts_with("cmd|script:start|"))
        .unwrap();
    assert!(start.contains("running · 9s"), "{start}");
    assert!(!start.contains("repo script"), "{start}");
}

/// Script rows take the same delimiter discipline as the rest of the section.
#[test]
fn script_rows_hold_exactly_two_delimiters() {
    let rows = command_rows(true, pooled(), &two_scripts(), &CmdStates::new(), 0, "");
    for line in rows.lines().filter(|l| l.starts_with("cmd|script:")) {
        assert_eq!(line.matches('|').count(), 2, "{line}");
    }
}

/// The frame comes from the clock, not from a counter, because every repaint is a *separate
/// process* — there is nothing for successive frames to keep in step with.
#[test]
fn the_spinner_advances_with_the_clock_and_never_leaves_the_cycle() {
    let frames: std::collections::BTreeSet<char> = (0..10).map(|i| spin_frame(i * 125)).collect();
    assert_eq!(
        frames.len(),
        10,
        "each step in the cycle is a distinct frame"
    );
    // A full turn returns to the start, and the same instant always renders the same frame.
    assert_eq!(spin_frame(0), spin_frame(10 * 125));
    assert_eq!(spin_frame(7_000), spin_frame(7_000));
    // Negative never panics on an index: a clock that reads before the epoch is absurd, but a
    // spinner is not the place to find that out.
    let _ = spin_frame(-1);
}

#[test]
fn elapsed_reads_as_minutes_past_a_minute() {
    assert_eq!(brief_elapsed(0), "0s");
    assert_eq!(brief_elapsed(45), "45s");
    assert_eq!(brief_elapsed(60), "1m00s");
    assert_eq!(brief_elapsed(72), "1m12s");
    assert_eq!(brief_elapsed(3_600), "60m00s");
}

/// What the queue is doing outranks what the pool looks like: a warm that is *running* has
/// A repo with a warm pool, so the pool verbs are offered at all.
fn pooled() -> CmdAvail {
    CmdAvail {
        pooled: true,
        cold: 3,
        at_ceiling: false,
    }
}

/// A held row names the reason, so "waiting" and "never going to run" stop looking the same.
///
/// This is the whole point of the note: eight verbs sat at a bare `⏳ queued` for three and a
/// half days on a machine with no dispatcher, and the row said exactly what it says for work
/// that is two seconds from starting.
#[test]
fn a_held_row_says_why_and_for_how_long() {
    let mut states = CmdStates::new();
    states.insert(
        "pool-warm".to_string(),
        CmdState::Queued(Some(Held {
            reason: "mandatory-warming".to_string(),
            waiting_secs: Some(272),
        })),
    );
    let rows = command_rows(true, pooled(), &[], &states, 0, "");
    let row = rows
        .lines()
        .find(|l| l.starts_with("cmd|pool-warm|"))
        .unwrap();
    assert!(row.contains("held · mandatory-warming"), "{row}");
    assert!(row.contains("4m"), "{row}");
}

/// A wait too short to be interesting drops the duration rather than flashing `0s` — the reason
/// is the useful half, and a row has one narrow column.
#[test]
fn a_briefly_held_row_names_the_reason_without_a_stopwatch() {
    let mut states = CmdStates::new();
    states.insert(
        "pool-warm".to_string(),
        CmdState::Queued(Some(Held {
            reason: "capacity".to_string(),
            waiting_secs: Some(3),
        })),
    );
    let rows = command_rows(true, pooled(), &[], &states, 0, "");
    let row = rows
        .lines()
        .find(|l| l.starts_with("cmd|pool-warm|"))
        .unwrap();
    assert!(row.contains("held · capacity"), "{row}");
    assert!(!row.contains("3s"), "{row}");
}

/// already answered "how many are cold?".
#[test]
fn queue_state_replaces_the_availability_note() {
    let avail = CmdAvail {
        pooled: true,
        cold: 3,
        at_ceiling: false,
    };
    let mut states = CmdStates::new();
    states.insert("pool-warm".to_string(), CmdState::Running(72));
    states.insert("wt-cleanup".to_string(), CmdState::Away);
    states.insert("pull-prs".to_string(), CmdState::Queued(None));
    let rows = command_rows(true, avail, &[], &states, 0, "");

    let warm = rows
        .lines()
        .find(|l| l.starts_with("cmd|pool-warm|"))
        .unwrap();
    assert!(warm.contains("running · 1m12s"), "{warm}");
    assert!(!warm.contains("3 cold"), "{warm}");

    let away = rows
        .lines()
        .find(|l| l.starts_with("cmd|wt-cleanup|"))
        .unwrap();
    assert!(away.contains("when you're away"), "{away}");

    let queued = rows
        .lines()
        .find(|l| l.starts_with("cmd|pull-prs|"))
        .unwrap();
    assert!(queued.contains("queued"), "{queued}");

    // A verb the queue says nothing about keeps its ordinary glyph.
    let idle = rows
        .lines()
        .find(|l| l.starts_with("cmd|build-main|"))
        .unwrap();
    assert!(idle.contains(CMD_GLYPH), "{idle}");
}

#[test]
fn a_place_row_is_refused_by_the_command_keys() {
    assert!(command_block("window", "4").is_some());
    assert!(command_block("worktree", "docs").is_some());
    assert!(command_block("cmdsep", "").is_some());
    assert!(command_block("cmd", "not-a-verb").is_some());
    assert_eq!(command_block("cmd", "pool-warm"), None);
    // A script id is a command too — syntactically; whether the file still exists is asked
    // at run time, where the refusal can be read.
    assert_eq!(command_block("cmd", "script:start"), None);
    assert!(command_block("cmd", "script:a|b").is_some());
    assert!(command_block("cmd", "script:").is_some());
}
