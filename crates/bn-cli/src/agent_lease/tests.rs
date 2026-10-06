use super::*;

const NOW: i64 = 1_700_000_000;

#[test]
fn a_missing_stamp_reads_as_now_not_the_epoch() {
    // The regression: `#{window_created}` is not a tmux variable, so this field arrived empty
    // for every window and every agent was stamped "acquired in 1970" — which is never within
    // the starting grace, so a launching agent had none.
    assert_eq!(stamped_since("", NOW), NOW);
    assert_eq!(stamped_since("   ", NOW), NOW);
    assert_eq!(stamped_since("not-a-number", NOW), NOW);
    // A zero on disk is the old bug's own output; treat it as unknown rather than the epoch.
    assert_eq!(stamped_since("0", NOW), NOW);
}

#[test]
fn a_real_stamp_is_kept() {
    assert_eq!(stamped_since("1699999000", NOW), 1_699_999_000);
}

#[test]
fn an_observed_window_keeps_its_grace_when_unstamped() {
    // Nine tab-separated fields then the stamp and pane_dead: an agent window bn marked before
    // `@bn_agent_since` was written carries no stamp at all.
    let row = "@7\tbn:feat\tbn\tfeat\t/wt/bn/feat\t/wt/bn/feat\tworking\ts-1\tdispatch\t\t0";
    let observed = parse_observed_agent_windows(row, NOW);
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].lease.acquired_at, NOW);
    assert_eq!(observed[0].created_at, NOW);
    // The point of the fix: it lands inside the starting grace, so a lease reserved moments
    // ago is authoritative until its window is observable.
    assert!(NOW - observed[0].lease.acquired_at < bn_core::agent::AGENT_LEASE_STARTING_GRACE_SECS);
}

#[test]
fn a_stamped_window_reports_its_real_age() {
    let row =
        "@7\tbn:feat\tbn\tfeat\t/wt/bn/feat\t/wt/bn/feat\tworking\ts-1\tdispatch\t1699999000\t0";
    let observed = parse_observed_agent_windows(row, NOW);
    assert_eq!(observed[0].lease.acquired_at, 1_699_999_000);
}

#[test]
fn an_unstamped_harness_pane_is_not_ancient() {
    // This zero is what `adopt_occupied_slots` copied into the pool ledger as the lease's `at`,
    // producing a slot that reported an age of 20673 days.
    let row = "bn:feat\t/wt/bn/feat\tclaude\t\t0\t101";
    let panes = parse_harness_panes(row, Path::new("/wt/bn/feat"), NOW);
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].since, NOW);
}

/// The ledger names a tenant by its branch, and `harness_panes_in` fills one in for every pane
/// it finds in a checkout. Before that, occupants here carried no branch and `label()` fell
/// back to `agent:<session>:<window>` — so a window already named `agent:bn:feat`, which is
/// exactly how a dispatched one is named, produced `agent:bn:agent:bn:feat` in the pool ledger
/// and stopped `wt pool return feat` matching it.
#[test]
fn a_named_pane_is_labelled_by_its_branch_not_its_window() {
    let row = "bn:agent:bn:feat\t/wt/bn/feat\tclaude\t\t0\t101";
    let mut panes = parse_harness_panes(row, Path::new("/wt/bn/feat"), NOW);
    assert_eq!(panes.len(), 1);

    // Unnamed — the detached-checkout case, where there is no branch to find. The fallback
    // still doubles, which is why naming them matters.
    assert_eq!(panes[0].label(), "agent:bn:agent:bn:feat");

    // Named, as `harness_panes_in` now does for any checkout that is on a branch.
    panes[0].branch = Some("feat".to_string());
    assert_eq!(panes[0].label(), "feat");
}

#[test]
fn a_version_titled_claude_pane_is_an_occupant() {
    // Claude Code's process title is its bare version, so the command column never says
    // "claude". The regression: every such pane was invisible to occupancy, and a slot with a
    // live agent typing in it was warm-reset underneath it.
    let row = "bn:feat\t/wt/bn/feat\t2.1.226\t\t0\t101";
    let panes = parse_harness_panes(row, Path::new("/wt/bn/feat"), NOW);
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].kind, "claude");
    // Non-version commands stay invisible: a build tool or shell is not an agent.
    for cmd in ["zsh", "node", "2", "gh", "1.2.x", "..", "2..3"] {
        let row = format!("bn:feat\t/wt/bn/feat\t{cmd}\t\t0\t101");
        assert!(
            parse_harness_panes(&row, Path::new("/wt/bn/feat"), NOW).is_empty(),
            "{cmd} must not read as a harness"
        );
    }
}

#[test]
fn an_nvim_hosted_harness_is_an_occupant() {
    let rows = parse_harness_pane_rows(
        "bn:feat\t/wt/bn/feat\tnvim\t\t0\t101",
        Path::new("/wt/bn/feat"),
        NOW,
    );
    let probe = NvimHostProbe {
        pid: 101,
        terminals: vec![NvimTerminal {
            pid: 200,
            name: "term://...:zsh".to_string(),
        }],
    };
    let procs = vec![
        crate::reap::Proc {
            pid: 200,
            ppid: 10,
            command: "zsh".to_string(),
        },
        crate::reap::Proc {
            pid: 210,
            ppid: 200,
            command: "/opt/bin/copilot --resume abc".to_string(),
        },
    ];
    let occupant = nvim_host_occupant_from(&rows, &probe, &procs).expect("occupant");
    assert_eq!(occupant.tmux_target, "bn:feat");
    assert_eq!(occupant.kind, "copilot");
    assert_eq!(occupant.since, NOW);
}

#[test]
fn a_shell_only_nvim_terminal_is_not_an_occupant() {
    let rows = parse_harness_pane_rows(
        "bn:feat\t/wt/bn/feat\tnvim\t\t0\t101",
        Path::new("/wt/bn/feat"),
        NOW,
    );
    let probe = NvimHostProbe {
        pid: 101,
        terminals: vec![NvimTerminal {
            pid: 200,
            name: "term://...:zsh".to_string(),
        }],
    };
    let procs = vec![crate::reap::Proc {
        pid: 200,
        ppid: 10,
        command: "zsh".to_string(),
    }];
    assert!(nvim_host_occupant_from(&rows, &probe, &procs).is_none());
}

#[test]
fn a_version_titled_descendant_counts_as_claude_inside_nvim() {
    let probe = NvimHostProbe {
        pid: 101,
        terminals: vec![NvimTerminal {
            pid: 200,
            name: "term://...:zsh".to_string(),
        }],
    };
    let procs = vec![
        crate::reap::Proc {
            pid: 200,
            ppid: 10,
            command: "zsh".to_string(),
        },
        crate::reap::Proc {
            pid: 210,
            ppid: 200,
            command: "2.1.226".to_string(),
        },
    ];
    assert_eq!(nvim_host_harness(&probe, &procs), Some("claude"));
}
