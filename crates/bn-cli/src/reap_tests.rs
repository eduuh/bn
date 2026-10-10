//! The kill path of a reap pass, over fixture process tables and pane lists: which pid tree a
//! candidate resolves to, which pids that tree covers, which windows are husks to prune, and which
//! verdicts the `--kill` loop acts on. Nothing here spawns `ps`, `tmux` or sends a signal.

use super::*;

fn proc_row(pid: i32, ppid: i32, command: &str) -> Proc {
    Proc {
        pid,
        ppid,
        command: command.to_string(),
    }
}

fn pane(window_id: &str, pane_pid: i32, dead: bool) -> Pane {
    Pane {
        window_id: window_id.to_string(),
        pane_pid,
        dead,
        cwd: "/w/bn/feat-x".to_string(),
    }
}

/// A windowed copilot (pane shell 100 → node wrapper → packaged worker → cargo → rustc), a
/// windowless claude straggler reparented to init, and an unrelated shell.
const PS_DUMP: &str = "\
  100     1 -zsh
  101   100 node /opt/homebrew/bin/copilot --yolo
  102   101 /opt/copilot-darwin-arm64/copilot --yolo
  103   102 cargo build
  104   103 rustc --crate-name bn
  200     1 node /usr/local/bin/claude -p seed
  201   200 /usr/local/lib/claude/claude
  202   201 bash -c ls
  300     1 /bin/zsh
garbage line
";

fn table() -> Vec<Proc> {
    parse_ps(PS_DUMP)
}

fn edges(procs: &[Proc]) -> Vec<(i32, i32)> {
    procs.iter().map(|p| (p.pid, p.ppid)).collect()
}

fn sorted(mut v: Vec<i32>) -> Vec<i32> {
    v.sort_unstable();
    v
}

#[test]
fn ps_rows_parse_and_malformed_rows_are_dropped() {
    let procs = table();
    assert_eq!(procs.len(), 9);
    assert_eq!(
        procs[1],
        proc_row(101, 100, "node /opt/homebrew/bin/copilot --yolo")
    );
    assert_eq!(parse_ps_line("12"), None);
    assert_eq!(parse_ps_line("x 1 cmd"), None);
}

#[test]
fn lsof_cwds_pair_each_pid_with_its_cwd() {
    let out = "p101\nfcwd\nn/w/bn/feat-x\np200\nfcwd\nn\np201\nn/w/bn/_pool/3\n";
    assert_eq!(
        parse_lsof_cwds(out),
        [
            (101, PathBuf::from("/w/bn/feat-x")),
            (201, PathBuf::from("/w/bn/_pool/3")),
        ]
    );
}

#[test]
fn only_real_harness_executables_count() {
    let table: &[(&str, Option<&str>)] = &[
        ("node /opt/homebrew/bin/copilot --yolo", Some("copilot")),
        ("/opt/copilot-darwin-arm64/copilot", Some("copilot")),
        ("claude --resume abc", Some("claude")),
        ("copilot-detached-supervisor --watch", None),
        ("cargo build -p claude-tools", None),
        ("-zsh", None),
    ];
    for (cmd, want) in table {
        assert_eq!(is_harness_command(cmd), *want, "{cmd}");
    }
}

#[test]
fn pr_state_maps_and_unknown_never_reaps() {
    assert_eq!(map_pr_status(" merged "), PrStatus::Merged);
    assert_eq!(map_pr_status("CLOSED"), PrStatus::Closed);
    assert_eq!(map_pr_status("open"), PrStatus::Open);
    assert_eq!(map_pr_status("NONE"), PrStatus::None);
    assert_eq!(map_pr_status("weird"), PrStatus::None);
}

/// A windowed agent's kill root is its pane's shell, so the whole pane tree dies with it.
#[test]
fn a_windowed_agent_resolves_to_its_pane_root() {
    let procs = table();
    let by_pid: BTreeMap<i32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let panes = [pane("@1", 100, false)];
    let live: Vec<&Pane> = panes.iter().collect();
    let live_pids: BTreeSet<i32> = [100].into_iter().collect();

    assert_eq!(
        resolve_root(102, &by_pid, &live_pids, &live),
        (100, Some("@1".to_string()))
    );
    assert_eq!(
        sorted(descendants(&edges(&procs), 100)),
        [100, 101, 102, 103, 104]
    );
}

/// A windowless straggler's kill root is its topmost harness ancestor — the node wrapper, so the
/// worker child is not left reparented — and never init or an unrelated process.
#[test]
fn a_windowless_straggler_resolves_to_its_topmost_harness() {
    let procs = table();
    let by_pid: BTreeMap<i32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();

    let (root, window) = resolve_root(201, &by_pid, &BTreeSet::new(), &[]);

    assert_eq!((root, window), (200, None));
    let tree = sorted(descendants(&edges(&procs), root));
    assert_eq!(tree, [200, 201, 202]);
    assert!(!tree.contains(&300) && !tree.contains(&1));
}

#[test]
fn descendants_survive_a_cycle_and_an_unknown_root() {
    assert_eq!(sorted(descendants(&[(1, 2), (2, 1)], 1)), [1, 2]);
    assert_eq!(descendants(&[(5, 4)], 9), [9]);
}

/// Only windows whose panes are all dead are pruned; a window with one live pane is left alone.
#[test]
fn only_all_dead_windows_are_husks() {
    let panes = [
        pane("@1", 100, false),
        pane("@2", 400, true),
        pane("@3", 500, true),
        pane("@3", 501, false),
        pane("@4", 600, true),
        pane("@4", 601, true),
    ];
    assert_eq!(dead_windows(&panes), ["@2", "@4"]);
    assert!(dead_windows(&[]).is_empty());
}

#[test]
fn target_keys_are_case_insensitive() {
    assert_eq!(target_key("BN", "Feat/X"), "bn:feat/x");
}

/// The `--kill` loop acts on exactly the verdicts `fleet_gc` marks reap, with the idle values
/// `reconcile` supplies: a windowless agent the fleet does not know is past any grace window.
#[test]
fn the_kill_loop_selects_only_reap_verdicts() {
    let stale = AgentObservation {
        has_live_window: false,
        idle_secs: STALE_IDLE_SENTINEL,
        ..AgentObservation::default()
    };
    let rows: &[(&str, AgentObservation, bool)] = &[
        ("windowless straggler", stale.clone(), true),
        (
            "merged pr",
            AgentObservation {
                pr: PrStatus::Merged,
                idle_secs: 3_600,
                ..AgentObservation::default()
            },
            true,
        ),
        ("healthy fresh launch", AgentObservation::default(), false),
        (
            "stale but working",
            AgentObservation {
                is_working: true,
                ..stale
            },
            false,
        ),
        (
            "stale with wip",
            AgentObservation {
                has_uncommitted: true,
                ..stale
            },
            false,
        ),
        (
            "stale but attended",
            AgentObservation {
                attended: true,
                ..stale
            },
            false,
        ),
        (
            "stale orchestrator",
            AgentObservation {
                is_orchestrator: true,
                ..stale
            },
            false,
        ),
        (
            "worktree gone inside grace",
            AgentObservation {
                worktree_exists: false,
                idle_secs: 10,
                ..AgentObservation::default()
            },
            false,
        ),
    ];
    let policy = ReapPolicy::default();
    for (name, obs, reap) in rows {
        assert_eq!(fleet_gc::classify(obs, &policy).is_reap(), *reap, "{name}");
    }

    // `--force` lifts every guard except the orchestrator's.
    let force = ReapPolicy {
        force: true,
        ..policy
    };
    let forced: Vec<&str> = rows
        .iter()
        .filter(|(_, obs, _)| fleet_gc::classify(obs, &force).is_reap())
        .map(|(name, _, _)| *name)
        .collect();
    assert_eq!(
        forced,
        [
            "windowless straggler",
            "merged pr",
            "stale but working",
            "stale with wip",
            "stale but attended",
            "worktree gone inside grace",
        ]
    );
}
