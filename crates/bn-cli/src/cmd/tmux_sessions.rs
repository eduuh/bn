//! Keeping the tmux session namespace to repositories.
//!
//! bn names a session after a repo and a window after a worktree — the layout `tat`, `bn jump` and
//! the status bar all assume. The code that broke that rule is fixed elsewhere; this module deals
//! with the sessions that are *already there*, because tmux-resurrect saves and restores whatever
//! it finds. An orphan killed by hand comes straight back on the next restore.
//!
//! Two surfaces, deliberately asymmetric:
//!
//! * [`emit_snapshot_filter`] runs from resurrect's `post-save-layout` hook and decides what the
//!   snapshot *records*. It destroys nothing — the worst it can do is fail to restore a session
//!   after a reboot, which is one keystroke to reopen — so it is safe to run unattended, on every
//!   save, forever.
//! * [`emit_tmux_prune`] kills live sessions and so is manual and dry-run by default, matching the
//!   stance in `config/crons.toml`: worktree, branch and session reclamation is never automated,
//!   because misidentifying one costs a person their work.

use crate::wt::{GREEN, NC};
use crate::*;
use std::collections::BTreeSet;

/// Sessions that are not repositories because they are not *work*.
///
/// `_bn_edit` is storage — a holding pen for parked editor panes — and any other leading-underscore
/// session is claiming the same private-namespace convention. `host:<alias>` is a `bn connect`
/// attachment to another machine, so the work it shows lives in that machine's repos, not here.
fn not_work(session: &str) -> bool {
    session.starts_with('_') || session.starts_with("host:")
}

/// Shells a pane sits at when nothing is running in it.
const IDLE_COMMANDS: &[&str] = &["zsh", "bash", "sh", "fish", "dash", "tmux"];

/// One window, as the pruner reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowRow {
    pub name: String,
    /// The pane's foreground command — anything but a bare shell means work is happening.
    pub command: String,
    /// Whether a bn agent has stamped this window (`@bn_agent`).
    pub agent: bool,
}

/// One session, as the pruner reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionRow {
    pub name: String,
    pub attached: bool,
    pub windows: Vec<WindowRow>,
}

/// Why a session was spared. Every one of these outranks "not a repo".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    /// Its name is a repository on this machine.
    RepoBacked,
    /// Storage or a remote attachment — not work, so not a repo.
    NotWork,
    /// Someone is looking at it right now.
    Attached,
    /// A pane is running something that is not a shell.
    Busy,
    /// A bn agent lives here.
    Agent,
}

/// Keep or kill, for one session. Pure — rows in, verdicts out, the shape `fleet_gc::classify` uses.
///
/// Ordering is the safety property: every reason to keep is checked before the one reason to kill,
/// and "not repo-backed" is the *last* question rather than the first. A session that is not a repo
/// but is being used is still being used, and the cost of getting this wrong is somebody's work.
pub(crate) fn classify(row: &SessionRow, repo_backed: &BTreeSet<String>) -> Result<(), Keep> {
    if repo_backed.contains(&row.name.to_lowercase()) {
        return Err(Keep::RepoBacked);
    }
    if not_work(&row.name) {
        return Err(Keep::NotWork);
    }
    if row.attached {
        return Err(Keep::Attached);
    }
    if row.windows.iter().any(|w| w.agent) {
        return Err(Keep::Agent);
    }
    if row
        .windows
        .iter()
        .any(|w| !IDLE_COMMANDS.contains(&w.command.trim()))
    {
        return Err(Keep::Busy);
    }
    Ok(())
}

impl Keep {
    fn label(self) -> &'static str {
        match self {
            Keep::RepoBacked => "names a repo",
            Keep::NotWork => "not work (storage / remote attachment)",
            Keep::Attached => "a client is attached",
            Keep::Busy => "a pane is running something",
            Keep::Agent => "an agent lives here",
        }
    }
}

/// Read every session and its windows off the live server.
fn read_sessions() -> Vec<SessionRow> {
    let Some(sessions) = proc::tmux_raw(&[
        "list-sessions",
        "-F",
        "#{session_name}\t#{session_attached}",
    ]) else {
        return Vec::new();
    };
    let windows = proc::tmux_raw(&[
        "list-windows",
        "-a",
        "-F",
        "#{session_name}\t#{window_name}\t#{pane_current_command}\t#{@bn_agent}",
    ])
    .unwrap_or_default();
    sessions
        .lines()
        .filter_map(|line| {
            let (name, attached) = line.split_once('\t')?;
            Some(SessionRow {
                name: name.to_string(),
                attached: attached.trim() != "0",
                windows: windows
                    .lines()
                    .filter_map(|w| {
                        let f: Vec<&str> = w.split('\t').collect();
                        if f.len() < 3 || f[0] != name {
                            return None;
                        }
                        Some(WindowRow {
                            name: f[1].to_string(),
                            command: f[2].to_string(),
                            // The option is set (even to "") for any window bn primed as an agent's.
                            agent: f.len() > 3 && !f[3].is_empty(),
                        })
                    })
                    .collect(),
            })
        })
        .collect()
}

/// `bn tmux-prune` — report, and with `--force` remove, sessions that are not repositories.
///
/// Dry run by default, like `bn reset`. Nothing here is wired to a cron or a hook: killing a
/// session is exactly the class of action `config/crons.toml` keeps in a person's hands.
pub(crate) fn emit_tmux_prune(cfg: &Config, force: bool, json: bool) {
    let repo_backed = crate::tat::discovery::repo_backed_sessions(cfg);
    if repo_backed.is_empty() {
        // No inventory means no basis for calling anything an orphan. Refuse rather than guess: an
        // unreadable root would otherwise read as "nothing is a repo, kill it all".
        let msg = "cannot read any project root — refusing to judge sessions";
        if json {
            println!(
                "{}",
                Envelope::failure("tmux_prune", "no_repo_inventory", msg).to_json()
            );
        } else {
            eprintln!("bn tmux-prune: {msg}");
        }
        return;
    }
    let rows = read_sessions();
    let (orphans, kept): (Vec<_>, Vec<_>) = rows
        .iter()
        .partition(|row| classify(row, &repo_backed).is_ok());

    let mut killed = 0usize;
    if force {
        for row in &orphans {
            if proc::tmux_run(&["kill-session", "-t", &format!("={}", row.name)]) {
                killed += 1;
            } else {
                eprintln!("bn tmux-prune: could not kill '{}'", row.name);
            }
        }
    }

    if json {
        println!(
            "{}",
            Envelope::success(
                "tmux_prune",
                serde_json::json!({
                    "orphans": orphans.iter().map(|r| &r.name).collect::<Vec<_>>(),
                    "kept": kept
                        .iter()
                        .map(|r| serde_json::json!({
                            "session": r.name,
                            "reason": classify(r, &repo_backed).unwrap_err().label(),
                        }))
                        .collect::<Vec<_>>(),
                    "killed": killed,
                    "force": force,
                }),
            )
            .to_json()
        );
        return;
    }

    if orphans.is_empty() {
        println!("{GREEN}every tmux session names a repo — nothing to prune{NC}");
        return;
    }
    for row in &orphans {
        let windows = row.windows.len();
        if force {
            println!("killed {} ({windows} window(s))", row.name);
        } else {
            println!("would kill {} ({windows} window(s))", row.name);
        }
    }
    if !force {
        println!("\n{} session(s) name no repo — pass --force to remove", orphans.len());
    }
}

/// Rewrite a resurrect snapshot so it records only sessions worth restoring. Pure.
///
/// Returns the new body. `repo_backed` empty is the caller's problem, not this function's — see
/// [`emit_snapshot_filter`], which refuses to call it in that case.
fn filter_snapshot(body: &str, repo_backed: &BTreeSet<String>) -> String {
    let keep_session = |name: &str| repo_backed.contains(&name.to_lowercase()) || not_work(name);
    let mut out = String::with_capacity(body.len());
    for line in body.lines() {
        let mut fields: Vec<&str> = line.split('\t').collect();
        match fields.first().copied() {
            // `pane` / `window` / `grouped_session` rows all carry their session in field 1. A row
            // for a session we are not recording simply does not go in the snapshot.
            Some("pane") | Some("window") | Some("grouped_session") => {
                if fields.len() > 1 && !keep_session(fields[1]) {
                    continue;
                }
            }
            // `state <client_session> <client_last_session>` — the session to reattach to and the
            // alternate. Blank a dropped name rather than dropping the row, so restore still finds
            // the fields it reads.
            Some("state") => {
                for f in fields.iter_mut().skip(1) {
                    if !keep_session(f) {
                        *f = "";
                    }
                }
            }
            // Anything unrecognised is carried through untouched — a snapshot format this does not
            // understand is one it has no business editing.
            _ => {}
        }
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    out
}

/// Filter the snapshot at `path` in place, from resurrect's `post-save-layout` hook.
///
/// This is the durable half of the cleanup. `planning` outlived the code that created it because
/// every save copied it forward and every restore brought it back; killing the session does nothing
/// while the snapshot still names it.
///
/// **Fails open, always.** An unreadable file, or an empty repo inventory, means "leave the
/// snapshot exactly as it is". The asymmetry is deliberate: dropping a row that should have stayed
/// costs one restored session, but treating a transient read failure as "nothing is a repo" would
/// drop every row and lose the whole layout.
pub(crate) fn emit_snapshot_filter(cfg: &Config, path: &str) {
    let repo_backed = crate::tat::discovery::repo_backed_sessions(cfg);
    if repo_backed.is_empty() {
        return;
    }
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    let out = filter_snapshot(&body, &repo_backed);
    if out == body {
        return;
    }
    // Write beside the target and rename, so a crash mid-write cannot leave a half-snapshot: the
    // file resurrect symlinks as `last` is either the old one or the new one.
    let tmp = format!("{path}.bn-filter");
    if std::fs::write(&tmp, &out).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repos() -> BTreeSet<String> {
        ["bn", "bigmono", "arena", "personal-notes"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn idle(name: &str) -> SessionRow {
        SessionRow {
            name: name.to_string(),
            attached: false,
            windows: vec![WindowRow {
                name: "main".to_string(),
                command: "zsh".to_string(),
                agent: false,
            }],
        }
    }

    #[test]
    fn a_session_named_after_a_repo_is_never_an_orphan() {
        assert_eq!(classify(&idle("bn"), &repos()), Err(Keep::RepoBacked));
        // Sessions keep the directory's real case; note keys are lowercased.
        assert_eq!(classify(&idle("BigMono"), &repos()), Err(Keep::RepoBacked));
    }

    /// The near-miss this predicate exists for: `arena` and `personal-notes` are flat clones
    /// symlinked onto the Windows filesystem, not bare repos. Judging by the bare dir alone would
    /// have killed three live working sessions.
    #[test]
    fn a_flat_clone_session_counts_as_repo_backed() {
        assert_eq!(classify(&idle("arena"), &repos()), Err(Keep::RepoBacked));
        assert_eq!(
            classify(&idle("personal-notes"), &repos()),
            Err(Keep::RepoBacked)
        );
    }

    #[test]
    fn storage_and_remote_attachments_are_not_work_and_so_are_spared() {
        assert_eq!(classify(&idle("_bn_edit"), &repos()), Err(Keep::NotWork));
        assert_eq!(classify(&idle("host:fedora"), &repos()), Err(Keep::NotWork));
    }

    #[test]
    fn an_idle_session_naming_no_repo_is_the_only_thing_we_kill() {
        assert_eq!(classify(&idle("planning"), &repos()), Ok(()));
        assert_eq!(classify(&idle("machine"), &repos()), Ok(()));
    }

    /// Every "someone is using this" signal outranks the name, because being wrong here costs work.
    #[test]
    fn a_session_in_use_survives_whatever_it_is_called() {
        let mut attached = idle("planning");
        attached.attached = true;
        assert_eq!(classify(&attached, &repos()), Err(Keep::Attached));

        let mut busy = idle("machine");
        busy.windows[0].command = "cargo".to_string();
        assert_eq!(classify(&busy, &repos()), Err(Keep::Busy));

        let mut agent = idle("machine");
        agent.windows[0].agent = true;
        assert_eq!(classify(&agent, &repos()), Err(Keep::Agent));
    }

    fn snapshot() -> String {
        [
            "pane\tbn\t1\t1\t:\t0\t:\t:/home/me\t1\tzsh\t123\t0",
            "pane\tplanning\t1\t1\t:\t0\t:\t:/home/me\t1\tzsh\t124\t0",
            "window\tbn\t1\t:main\t1\t:\tlayout",
            "window\tplanning\t1\t:zsh\t1\t:\tlayout",
            "window\t_bn_edit\t1\t:hold\t1\t:\tlayout",
            "grouped_session\tmachine\tbn\t:1\t:1",
            "state\tplanning\tbn",
        ]
        .join("\n")
            + "\n"
    }

    fn filtered(body: &str) -> String {
        filter_snapshot(body, &repos())
    }

    #[test]
    fn orphan_rows_are_dropped_and_repo_rows_survive() {
        let out = filtered(&snapshot());
        assert!(out.contains("pane\tbn\t"));
        assert!(out.contains("window\tbn\t"));
        assert!(!out.contains("planning"), "{out}");
        assert!(!out.contains("grouped_session\tmachine"), "{out}");
    }

    /// Storage stays in the snapshot: dropping it would leave parked editors homeless on restore.
    #[test]
    fn the_hold_session_is_recorded_like_a_repo() {
        assert!(filtered(&snapshot()).contains("window\t_bn_edit\t"));
    }

    /// `restore.sh` reads both fields of the `state` row, so a dropped name is blanked in place
    /// rather than removed — the row has to keep its shape.
    #[test]
    fn a_dropped_client_session_is_blanked_not_deleted() {
        let out = filtered(&snapshot());
        assert!(out.contains("state\t\tbn\n"), "{out}");
    }

    #[test]
    fn an_unrecognised_row_is_carried_through_untouched() {
        let body = "something-new\tplanning\tvalue\n";
        assert_eq!(filtered(body), body);
    }
}
