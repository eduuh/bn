//! The picker frontends, and the only place in `tat` that knows which one is running.
//!
//! Both backends consume the same row builders in [`super::rows`] — fzf gets them flattened into
//! its delimiter-joined feed, the native picker gets them typed. Everything downstream
//! (`resolve_and_open`, the sub-picker dispatch) sees the same values either way, which is what
//! makes `BN_PICKER` a toggle rather than a fork.

use super::commands::CmdStates;
use super::rows::{SubRow, SubRowItem};
use super::*;
use crate::tui::{self, Action, Backend, Item, Key, Outcome, Picker, Reload, Rows};

/// The top-level project picker. `Some((selection, typed))` where `typed` means the user's query
/// was taken as the answer because nothing was selected — the path that clones a pasted URL or
/// cuts a new branch.
pub(super) fn top_level(
    cfg: &Config,
    roots: &Roots,
    root_opt: Option<&str>,
) -> Option<(String, bool)> {
    match tui::backend() {
        Backend::Native => native_top_level(cfg, roots, root_opt),
        Backend::Fzf => fzf_top_level(cfg, roots, root_opt),
    }
}

fn native_top_level(cfg: &Config, roots: &Roots, root_opt: Option<&str>) -> Option<(String, bool)> {
    if !tui::interactive() {
        // Piped or scripted: print the list and stop, the same degrade the fzf path takes when fzf
        // is missing. `bn tat | cat` stays useful instead of drawing frames into a pipe.
        super::rows::build_list(roots, false);
        return None;
    }
    let picker = Picker::new(super::status::status_line(cfg), Vec::new())
        .rows(Rows::Lazy(Box::new(|why| {
            super::rows::list_rows(roots, why == Reload::Hard)
                .into_iter()
                .map(|row| {
                    // Search the ref as well as the decorated display, so typing a repo name
                    // matches whether or not it is wearing a session marker.
                    let haystack = format!("{} {}", row.display, row.r);
                    Item::new(row.display, row.r).searchable(haystack)
                })
                .collect()
        })))
        .preview(50, move |r: &String| {
            tat_stage_output(root_opt, Some(r), "preview", None)
        })
        .bind(Key::Ctrl('x'), "kill session", |ctx| match ctx.focused {
            Some(r) => {
                kill_from_line(r);
                Action::Reload
            }
            None => Action::Stay,
        })
        .bind(Key::Ctrl('r'), "refresh", |_| Action::Refresh);

    match tui::run(picker) {
        Outcome::Selected { items, .. } => items.into_iter().next().map(|sel| (sel, false)),
        Outcome::Query(q) if !q.is_empty() => Some((q, true)),
        _ => None,
    }
}

fn fzf_top_level(cfg: &Config, roots: &Roots, root_opt: Option<&str>) -> Option<(String, bool)> {
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let root_flag = root_flag(root_opt);
    let child = Command::new("fzf")
        .arg("--reverse")
        .arg("--ansi")
        .arg("--print-query")
        .arg("--delimiter=\t")
        .arg("--with-nth=1")
        .arg(format!("--header={}", super::status::header(cfg)))
        .arg(format!(
            "--preview={bn} tat{root_flag} --pick-stage preview {{-1}}"
        ))
        .arg("--preview-window=right:50%:wrap")
        .arg(format!(
            "--bind=start:reload({bn} tat{root_flag} --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-x:execute-silent({bn} tat{root_flag} --pick-stage kill {{-1}})+reload({bn} tat{root_flag} --pick-stage list)"
        ))
        .arg(format!(
            "--bind=ctrl-r:reload({bn} tat{root_flag} --pick-stage refresh)"
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(_) => {
            // No fzf on PATH — degrade to a plain list, then hold.
            super::rows::build_list(roots, false);
            crate::picker::hold();
            return None;
        }
    };
    let out = child.wait_with_output().ok()?;
    if out.status.code() == Some(130) {
        return None; // Esc / ctrl-c
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let query = lines.next().unwrap_or("");
    let selection = lines.next().unwrap_or("");
    let (sel, typed) = if !selection.is_empty() {
        (
            selection
                .rsplit('\t')
                .next()
                .unwrap_or(selection)
                .to_string(),
            false,
        )
    } else {
        (query.to_string(), true)
    };
    (!sel.is_empty()).then_some((sel, typed))
}

/// How a sub-picker session ended. The query rides along because an idle pool slot is leased to
/// whatever the user typed.
pub(super) struct SubPick {
    pub(super) row: Option<SubRow>,
    pub(super) query: String,
    pub(super) key: String,
}

impl SubPick {
    fn cancelled() -> Self {
        Self {
            row: None,
            query: String::new(),
            key: String::new(),
        }
    }
}

/// The per-project sub-picker: live windows, idle worktrees, remote PRs, codespaces, pool slots.
pub(super) fn sub(
    cfg: &Config,
    roots: &Roots,
    name: &str,
    header: &str,
    root_opt: Option<&str>,
    rows: Vec<SubRowItem>,
    states: &CmdStates,
) -> SubPick {
    match tui::backend() {
        Backend::Native => native_sub(cfg, roots, name, header, root_opt, rows),
        Backend::Fzf => fzf_sub(name, header, root_opt, &rows, states),
    }
}

fn native_sub(
    cfg: &Config,
    roots: &Roots,
    name: &str,
    header: &str,
    root_opt: Option<&str>,
    rows: Vec<SubRowItem>,
) -> SubPick {
    let items = |rows: Vec<SubRowItem>| -> Vec<Item<SubRow>> {
        rows.into_iter()
            .map(|item| Item::new(item.display, item.row))
            .collect()
    };
    let mut first = Some(items(rows));
    let picker = Picker::new(header, Vec::new())
        .rows(Rows::Lazy(Box::new(move |why| match first.take() {
            // The caller already built the opening rows (it needed the window count to decide
            // whether to show a picker at all) — don't pay for them twice.
            Some(rows) if why == Reload::Initial => rows,
            _ => items(build_sub_rows(cfg, roots, name, why == Reload::Hard).0),
        })))
        .preview(50, move |row: &SubRow| {
            tat_stage_output(root_opt, Some(name), "subpreview", Some(pick_arg(row)))
        })
        .bind(Key::Ctrl('d'), "delete worktree", |ctx| {
            let Some(row) = ctx.focused.cloned() else {
                return Action::Stay;
            };
            Action::Suspend(Box::new(move || {
                rm_worktree(cfg, roots, name, &pick_arg(&row))
            }))
        })
        .bind(Key::Ctrl('w'), "warm pool", |_| {
            Action::Suspend(Box::new(move || warm_pool(roots, name)))
        })
        .bind(Key::Ctrl('r'), "release lease", |ctx| {
            let Some(row) = ctx.focused.cloned() else {
                return Action::Stay;
            };
            Action::Suspend(Box::new(move || {
                release_pool(cfg, roots, name, &pick_arg(&row))
            }))
        })
        .bind(Key::Ctrl('o'), "row menu", |ctx| {
            let Some(row) = ctx.focused.cloned() else {
                return Action::Stay;
            };
            Action::Suspend(Box::new(move || {
                let payload = row.payload();
                commands::row_menu(cfg, roots, name, row.kind(), &payload);
            }))
        });

    match tui::run(picker) {
        Outcome::Selected { items, query } => SubPick {
            row: items.into_iter().next(),
            query,
            key: String::new(),
        },
        Outcome::Query(query) => SubPick {
            row: None,
            query,
            key: String::new(),
        },
        Outcome::Cancelled => SubPick::cancelled(),
    }
}

/// The `kind~payload` string the guarded actions parse. Kept as a string rather than passing the
/// typed row through: `delete_block` guards on the kind and is the tested pure gate, so it keeps
/// the signature its tests pin.
fn pick_arg(row: &SubRow) -> String {
    format!("{}~{}", row.kind(), row.payload())
}

fn fzf_sub(
    name: &str,
    header: &str,
    root_opt: Option<&str>,
    rows: &[SubRowItem],
    states: &CmdStates,
) -> SubPick {
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::shq(&exe.display().to_string());
    let root_flag = root_flag(root_opt);
    let name_q = crate::shq(name);
    let listen = commands::Listen::reserve();
    let child = Command::new("fzf")
        .arg("--reverse")
        .arg("--ansi")
        .arg("--delimiter=|")
        .arg("--with-nth=3")
        .arg("--print-query")
        .arg("--expect=ctrl-o")
        .arg(format!("--header={header}"))
        .arg(format!(
            "--preview={bn} tat {name_q}{root_flag} --pick-stage subpreview --pick-arg {{1}}~{{2}}"
        ))
        .arg("--preview-window=right:50%:wrap")
        .arg("--bind=ctrl-p:toggle-preview")
        .arg(format!(
            "--bind=ctrl-d:execute({bn} tat {name_q}{root_flag} --pick-stage rmworktree --pick-arg {{1}}~{{2}})+reload({bn} tat {name_q}{root_flag} --pick-stage sublist)"
        ))
        .arg(format!(
            "--bind=ctrl-w:execute({bn} tat {name_q}{root_flag} --pick-stage warmpool)+reload({bn} tat {name_q}{root_flag} --pick-stage sublist)"
        ))
        .arg(format!(
            "--bind=ctrl-r:execute({bn} tat {name_q}{root_flag} --pick-stage releasepool --pick-arg {{1}}~{{2}})+reload({bn} tat {name_q}{root_flag} --pick-stage sublist)"
        ))
        .args(commands::listen_args(listen.as_ref()))
        .envs(commands::listen_env(listen.as_ref()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return SubPick::cancelled();
    };
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        let _ = stdin.write_all(super::rows::sub_rows_feed(rows).as_bytes());
    }
    let _ticker = commands::Ticker::start(listen.as_ref(), &exe, name, &root_flag, states);
    let Ok(out) = child.wait_with_output() else {
        return SubPick::cancelled();
    };
    if out.status.code() == Some(130) {
        return SubPick::cancelled();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let query = lines.next().unwrap_or("").to_string();
    let key = lines.next().unwrap_or("").to_string();
    let selection = lines.next().unwrap_or("");
    SubPick {
        row: parse_piped(selection),
        query,
        key,
    }
}

/// Recover a [`SubRow`] from the `kind|payload|display` line fzf hands back.
fn parse_piped(line: &str) -> Option<SubRow> {
    if line.is_empty() {
        return None;
    }
    let mut parts = line.splitn(3, '|');
    let kind = parts.next().unwrap_or("");
    let payload = parts.next().unwrap_or("").to_string();
    Some(match kind {
        "window" => SubRow::Window(payload),
        "worktree" => SubRow::Worktree(payload),
        "remotepr" => SubRow::RemotePr(payload),
        "codespace" => {
            // The payload packs `csname\tbranch`.
            let (name, branch) = payload.split_once('\t').unwrap_or((&payload, ""));
            SubRow::Codespace {
                name: name.to_string(),
                branch: branch.to_string(),
            }
        }
        "poolbranch" => SubRow::PoolBranch(payload),
        "poolidle" => SubRow::PoolIdle(payload),
        "cmdsep" => SubRow::CmdSep,
        "cmd" => SubRow::Cmd(payload),
        "qset" => SubRow::QueueSettings,
        // An unknown kind means the feed and this parser have drifted; treating it as a worktree
        // is what the pre-typed code did (its final `else` branch).
        _ => SubRow::Worktree(payload),
    })
}

fn root_flag(root_opt: Option<&str>) -> String {
    if root_opt == Some("win") {
        " --root win".to_string()
    } else {
        String::new()
    }
}

fn tat_stage_output(
    root_opt: Option<&str>,
    name: Option<&str>,
    stage: &str,
    pick_arg: Option<String>,
) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let mut args = vec!["tat".to_string()];
    if let Some(name) = name {
        args.push(name.to_string());
    }
    if root_opt == Some("win") {
        args.push("--root".to_string());
        args.push("win".to_string());
    }
    args.push("--pick-stage".to_string());
    args.push(stage.to_string());
    if let Some(pick_arg) = pick_arg {
        args.push("--pick-arg".to_string());
        args.push(pick_arg);
    }
    Command::new(exe)
        .args(args)
        .output()
        .ok()
        .map(|out| {
            if out.status.success() || !out.stdout.is_empty() {
                String::from_utf8_lossy(&out.stdout).to_string()
            } else {
                String::from_utf8_lossy(&out.stderr).to_string()
            }
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piped_row_round_trips_through_the_fzf_feed() {
        for row in [
            SubRow::Window("3".into()),
            SubRow::Worktree("feat-auth".into()),
            SubRow::RemotePr("42".into()),
            SubRow::Codespace {
                name: "fuzzy-space".into(),
                branch: "feat/x".into(),
            },
            SubRow::PoolBranch("feat-auth".into()),
            SubRow::PoolIdle("3".into()),
            SubRow::CmdSep,
            SubRow::Cmd("build-main".into()),
            SubRow::QueueSettings,
        ] {
            let item = SubRowItem {
                display: "whatever the row shows".into(),
                row: row.clone(),
            };
            assert_eq!(parse_piped(&item.piped()), Some(row));
        }
    }

    /// The hazard the typed rows remove: a display column carrying the feed's own delimiter used to
    /// need scrubbing. Parsing stops after two fields, so the third can hold anything.
    #[test]
    fn a_pipe_in_the_display_column_does_not_confuse_the_parser() {
        let item = SubRowItem {
            row: SubRow::RemotePr("42".into()),
            display: "PR: feat | with | pipes".into(),
        };
        assert_eq!(
            parse_piped(&item.piped()),
            Some(SubRow::RemotePr("42".into()))
        );
    }

    #[test]
    fn an_empty_selection_is_no_row() {
        assert_eq!(parse_piped(""), None);
    }

    #[test]
    fn the_guarded_actions_still_get_their_kind_tilde_payload() {
        assert_eq!(
            pick_arg(&SubRow::Worktree("feat-auth".into())),
            "worktree~feat-auth"
        );
        assert_eq!(pick_arg(&SubRow::PoolIdle("3".into())), "poolidle~3");
        assert_eq!(pick_arg(&SubRow::QueueSettings), "qset~");
    }
}
