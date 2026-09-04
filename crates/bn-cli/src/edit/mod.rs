//! `bn edit` — the per-worktree nvim: create it, park it, restore it, find it.
//!
//! Replaces the disposable `prefix e` popup (`display-popup -E 'nvim .'`), where nvim was a direct
//! child of the popup and the only way out was quitting it. Here the editor is a real pane that
//! *hides* — broken out into a detached hold session and joined back on demand — so buffers, undo
//! and LSP survive being dismissed. `config/workflow/docs/tmux.md` has documented this behaviour
//! all along; this is the code catching up to it.
//!
//! Identity, socket derivation and the tmux option names live in [`bn_core::editor`]; the state
//! machine in [`session`]; the tmux argv in [`pane`]. Everything here is orchestration.

use crate::proc::{tmux, tmux_ok};
use bn_core::editor;
use bn_core::render::Envelope;
use bn_core::resolver::Config;
use clap::Subcommand;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub(crate) mod diff;
pub(crate) mod land;
pub(crate) mod pane;
pub(crate) mod rpc;
pub(crate) mod session;
pub(crate) mod target;

use pane::Side;
use session::{EditSession, EditState};
use target::{TargetError, TargetSpec};

/// Exit code for "there is no worktree here" — a normal outcome for a keybinding pressed in a
/// scratch pane, not a failure, so callers can tell it apart from a real error.
const EXIT_NO_TARGET: i32 = 2;

#[derive(Subcommand, Debug)]
pub(crate) enum EditCmd {
    /// Show this worktree's editor: create it at `--size` of the window, or join a parked one back.
    Show(EditArgs),
    /// Park the editor in the hidden hold session — nvim keeps running, the window gets its full
    /// width back. The inverse of `show`, and what the in-editor keymap calls.
    Hide(EditArgs),
    /// Park it if it is showing, show it otherwise. What `prefix e` is bound to.
    Toggle(EditArgs),
    /// Report what bn knows about this worktree's editor without changing anything.
    #[command(visible_alias = "status")]
    Where(EditArgs),
    /// Put the editor on this branch's diff, with the agent terminal spawned. Idempotent: landing
    /// twice focuses the existing view rather than stacking a second one.
    Land(LandArgs),
    /// Pick a worktree and open its diff in *its* editor — the fleet as one list, with the diff as
    /// the preview. Enter switches you there, so the code you read is rooted in the right repo.
    Diff {
        /// Base ref for every row's diff. Default: resolved per worktree.
        #[arg(long)]
        base: Option<String>,
        /// Internal fzf re-entry: emit the row feed rather than opening the picker.
        #[arg(long, hide = true)]
        pick_stage: Option<diff::DiffStage>,
    },
}

impl LandArgs {
    /// The landing the diff picker asks for: this branch's diff against `base`, with the agent
    /// terminal spawned but out of the way — you came here to read a diff.
    pub(crate) fn for_diff(repo: &str, branch: &str, base: Option<&str>) -> LandArgs {
        LandArgs {
            target: EditArgs {
                repo: Some(repo.to_string()),
                branch: Some(branch.to_string()),
                size: "90%".to_string(),
                side: "left".to_string(),
                ..Default::default()
            },
            base: base.map(str::to_string),
            no_diff: false,
            term: "hidden".to_string(),
            agent_cmd: None,
            force: false,
        }
    }
}

#[derive(clap::Args, Debug, Clone)]
pub(crate) struct LandArgs {
    #[command(flatten)]
    pub(crate) target: EditArgs,
    /// Base ref for the diff. Default: origin/HEAD, then the branch's upstream, then the first of
    /// origin/main, main, origin/master, master that resolves.
    #[arg(long)]
    pub(crate) base: Option<String>,
    /// Skip the diff and just spawn the terminal.
    #[arg(long)]
    pub(crate) no_diff: bool,
    /// Whether the agent terminal is spawned hidden (default), shown, or not at all.
    #[arg(long, default_value = "hidden")]
    pub(crate) term: String,
    /// The command the agent terminal runs. Omit for a plain shell.
    #[arg(long)]
    pub(crate) agent_cmd: Option<String>,
    /// Re-apply even if this landing already happened.
    #[arg(long)]
    pub(crate) force: bool,
}

#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct EditArgs {
    /// The tmux pane to resolve from — what a keybinding passes as `#{pane_id}`.
    #[arg(long, conflicts_with_all = ["path", "repo", "branch"])]
    pub(crate) pane: Option<String>,
    /// Any directory inside the worktree.
    #[arg(long, value_name = "DIR", conflicts_with_all = ["repo", "branch"])]
    pub(crate) path: Option<PathBuf>,
    /// Name the worktree explicitly (with `--branch`).
    #[arg(long, requires = "branch")]
    pub(crate) repo: Option<String>,
    /// The branch (with `--repo`).
    #[arg(long, requires = "repo")]
    pub(crate) branch: Option<String>,
    /// How much of the window the editor takes. The default collapses the command pane to a
    /// narrow strip rather than killing it — it often holds a running agent, and `prefix e` gives
    /// it the full width back instantly.
    #[arg(long, default_value = "90%")]
    pub(crate) size: String,
    /// Which side it sits on.
    #[arg(long, default_value = "left")]
    pub(crate) side: String,
}

impl EditArgs {
    fn spec(&self) -> TargetSpec {
        if let Some(p) = &self.pane {
            return TargetSpec::Pane(p.clone());
        }
        if let Some(p) = &self.path {
            return TargetSpec::Path(p.clone());
        }
        match (&self.repo, &self.branch) {
            (Some(r), Some(b)) => TargetSpec::RepoBranch(r.clone(), b.clone()),
            _ => TargetSpec::Infer,
        }
    }
}

/// Dispatch for `bn edit`.
///
/// Every mutating verb is **silent on success**: these run from tmux keybindings via `run-shell`,
/// which displays a command's stdout in view mode after it finishes, so a single stray `println!`
/// would flash a pager over the session on every keypress.
pub(crate) fn route(cfg: &Config, cmd: EditCmd, json: bool) {
    let (args, action) = match &cmd {
        EditCmd::Show(a) => (a, "show"),
        EditCmd::Hide(a) => (a, "hide"),
        EditCmd::Toggle(a) => (a, "toggle"),
        EditCmd::Where(a) => (a, "where"),
        EditCmd::Land(a) => (&a.target, "land"),
        EditCmd::Diff { base, pick_stage } => {
            match pick_stage {
                Some(stage) => diff::pick_stage(cfg, *stage),
                None => diff::run(cfg, base.as_deref()),
            }
            return;
        }
    };

    let target = match target::resolve_target(cfg, &args.spec()) {
        Ok(t) => t,
        Err(e) => no_target(json, &e),
    };
    let mut sess = session::probe(target);
    let before = sess.state;

    // A landing is prepared before the editor is touched: a pane created below is handed the spec
    // on its own command line and lands while starting up, which avoids talking to an nvim that is
    // still loading its plugins.
    let land_args = match &cmd {
        EditCmd::Land(a) => Some(a.clone()),
        _ => None,
    };
    let prepared = match &land_args {
        Some(a) => match prepare_land(cfg, &sess, a) {
            Ok(p) => Some(p),
            Err(e) => fail_edit(json, &e),
        },
        None => None,
    };

    let mut landed: Option<land::LandStatus> = None;
    let outcome = match action {
        "show" | "land" => show(&mut sess, args, prepared.as_deref()).and_then(|created| {
            match (&land_args, created) {
                // Cold start: the spec was on the command line, so it has already landed.
                (Some(_), true) => Ok(()),
                (Some(_), false) => {
                    land::apply(&sess, prepared.as_deref().unwrap_or(Path::new("")))
                        .map(|st| landed = Some(st))
                }
                (None, _) => Ok(()),
            }
        }),
        "hide" => hide(&mut sess, args),
        "toggle" => match sess.state {
            EditState::Shown => hide(&mut sess, args).map(|_| ()),
            _ => show(&mut sess, args, None).map(|_| ()),
        },
        _ => Ok(()),
    };

    if let Err(e) = outcome {
        if json {
            println!("{}", Envelope::failure("edit", "edit_failed", &e).to_json());
        } else {
            eprintln!("bn edit: {e}");
        }
        std::process::exit(1);
    }

    if json {
        println!(
            "{}",
            report(
                &sess,
                action,
                before,
                Side::parse(&args.side),
                landed.as_ref()
            )
            .to_json()
        );
    } else if action == "where" {
        println!("{}", where_line(&sess));
    }
}

fn no_target(json: bool, e: &TargetError) -> ! {
    if json {
        println!(
            "{}",
            Envelope::failure("edit", "no_worktree", e.to_string()).to_json()
        );
    } else {
        eprintln!("bn edit: {e}");
    }
    std::process::exit(EXIT_NO_TARGET);
}

/// Create the editor, or bring a parked/detached one home. Re-probes afterwards so the reported
/// state is observed rather than assumed.
fn show(sess: &mut EditSession, args: &EditArgs, land_spec: Option<&Path>) -> Result<bool, String> {
    let side = Side::parse(&args.side);
    let interactive = std::env::var_os("TMUX").is_some() && std::io::stdout().is_terminal();

    let mut created = false;
    match sess.state {
        EditState::Host => return Ok(false),
        EditState::Shown => {
            if let Some(p) = &sess.pane {
                if interactive {
                    run_all(&pane::focus_cmds(&p.pane, &p.home));
                }
            }
            return Ok(false);
        }
        EditState::Dead => {
            if let Some(p) = &sess.pane {
                let _ = tmux_ok(&["kill-pane", "-t", &p.pane]);
            }
            session::clear_stale(&sess.socket);
            create(sess, side, &args.size, interactive, land_spec)?;
            created = true;
        }
        EditState::Absent => {
            session::clear_stale(&sess.socket);
            create(sess, side, &args.size, interactive, land_spec)?;
            created = true;
        }
        EditState::Hidden | EditState::Detached => {
            let p = sess
                .pane
                .clone()
                .ok_or_else(|| "no editor pane to restore".to_string())?;
            let home = resolve_home(sess, &p.home)?;
            let anchor = first_pane_of(&home)
                .ok_or_else(|| format!("window {home} has no pane to join into"))?;
            run_all(&pane::restore_cmds(
                &p.pane, &home, &anchor, side, &args.size,
            ));
            if interactive {
                run_all(&pane::focus_cmds(&p.pane, &home));
            }
        }
    }

    *sess = session::probe(sess.target.clone());
    Ok(created)
}

/// Park the editor. On an `editor`-shaped window this is deliberately a no-op at the tmux level:
/// nvim *is* the window, so `break-pane` on its only pane would strip the window of its identity
/// and trip `wt`'s respawn-the-last-window path. Hiding there is an in-editor act (step 4).
fn hide(sess: &mut EditSession, _args: &EditArgs) -> Result<(), String> {
    match sess.state {
        EditState::Host | EditState::Absent | EditState::Hidden => return Ok(()),
        EditState::Dead => {
            if let Some(p) = &sess.pane {
                let _ = tmux_ok(&["kill-pane", "-t", &p.pane]);
            }
            session::clear_stale(&sess.socket);
        }
        EditState::Shown | EditState::Detached => {
            let p = sess
                .pane
                .clone()
                .ok_or_else(|| "no editor pane to park".to_string())?;
            ensure_hold();
            let name = window_label(sess);
            // The pane's *current* window is home, not whatever it recorded earlier — it may have
            // been joined somewhere else since.
            let home = if p.window.is_empty() {
                p.home.clone()
            } else {
                p.window.clone()
            };
            run_all(&pane::hide_cmds(&p.pane, &home, &name));
            if std::env::var_os("TMUX").is_some() && std::io::stdout().is_terminal() {
                if let Some(anchor) = first_pane_of(&home) {
                    let _ = tmux_ok(&["select-pane", "-t", &anchor]);
                }
            }
        }
    }
    *sess = session::probe(sess.target.clone());
    Ok(())
}

/// Show (create or restore) an editor for the picker, with the landing spec already written so a
/// cold start bakes it into the pane's own command line. Returns whether it was freshly created.
pub(crate) fn show_for_diff(sess: &mut EditSession, spec: &Path) -> Result<bool, String> {
    let args = EditArgs {
        size: "90%".to_string(),
        side: "left".to_string(),
        ..Default::default()
    };
    show(sess, &args, Some(spec))
}

/// Resolve the base, build the recipe and write the spec — everything a landing needs *before* the
/// editor is touched.
///
/// Two paths, one spec file. A pane created moments later is handed this path on its own command
/// line and lands as part of starting up; an editor already running is told over the socket. Both
/// read the same bytes, so "did the resume land the same way a cold start does?" is answerable by
/// inspection rather than by hope.
pub(crate) fn prepare_land(
    cfg: &Config,
    sess: &EditSession,
    args: &LandArgs,
) -> Result<PathBuf, String> {
    if !pane::is_nvim(&pane::editor_bin()) {
        return Err("landing needs nvim — $EDITOR is something else".to_string());
    }
    let opts = land::LandOpts {
        base: args.base.clone(),
        diff: !args.no_diff,
        term: land::TermMode::parse(&args.term),
        agent_cmd: args.agent_cmd.clone(),
        force: args.force,
    };
    let recipe = land::recipe_for(cfg, &sess.target, &opts);
    land::write_spec(&sess.target, &recipe)
}

fn fail_edit(json: bool, msg: &str) -> ! {
    if json {
        println!(
            "{}",
            Envelope::failure("edit", "edit_failed", msg).to_json()
        );
    } else {
        eprintln!("bn edit: {msg}");
    }
    std::process::exit(1);
}

/// Create the pane and stamp it. The pane id comes back from `split-window -P`, so the stamps land
/// on exactly the pane just made rather than on whatever is currently active.
fn create(
    sess: &mut EditSession,
    side: Side,
    size: &str,
    interactive: bool,
    land_spec: Option<&Path>,
) -> Result<(), String> {
    let home = home_window(sess)?;
    editor::ensure_runtime_dir();
    let bin = pane::editor_bin();
    let anchor =
        first_pane_of(&home).ok_or_else(|| format!("window {home} has no pane to split from"))?;
    let argv = pane::create_split_cmd(
        &anchor,
        &sess.target.worktree,
        &sess.socket,
        side,
        size,
        &bin,
        land_spec,
    );
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let new_pane = tmux(&refs)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("could not split an editor pane in {home}"))?;
    run_all(&pane::stamp_cmds(&new_pane, &sess.target.worktree, &home));
    if interactive {
        run_all(&pane::focus_cmds(&new_pane, &home));
    }
    Ok(())
}

/// The window an editor should live in: the one it recorded, else any window already open on this
/// worktree, else a freshly opened one.
fn home_window(sess: &EditSession) -> Result<String, String> {
    if let Some(p) = &sess.pane {
        if !p.home.is_empty() && window_exists(&p.home) {
            return Ok(p.home.clone());
        }
    }
    existing_or_new_window(sess)
}

/// Same, for restoring: prefer the recorded home, but never join into a window that is gone.
fn resolve_home(sess: &EditSession, recorded: &str) -> Result<String, String> {
    if !recorded.is_empty() && window_exists(recorded) {
        return Ok(recorded.to_string());
    }
    existing_or_new_window(sess)
}

fn existing_or_new_window(sess: &EditSession) -> Result<String, String> {
    let wins = crate::wt::tmux_windows_for(&sess.target.worktree, &sess.target.branch);
    if let Some(w) = wins.first() {
        if let Some(id) = tmux(&["display-message", "-p", "-t", w, "#{window_id}"]) {
            let id = id.trim().to_string();
            if !id.is_empty() {
                return Ok(id);
            }
        }
    }
    crate::wt::open_worktree_window(
        &sess.target.repo,
        &sess.target.branch,
        &sess.target.worktree,
        false,
    );
    let wins = crate::wt::tmux_windows_for(&sess.target.worktree, &sess.target.branch);
    let w = wins
        .first()
        .ok_or_else(|| format!("no tmux window for {}", sess.target.worktree.display()))?;
    tmux(&["display-message", "-p", "-t", w, "#{window_id}"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "could not resolve the worktree's window".to_string())
}

/// The id of a window's first pane. Splits and joins anchor on this rather than a `<window>.0`
/// composite, which tmux only accepts after a window name or index — never after a `@id`.
fn first_pane_of(window: &str) -> Option<String> {
    tmux(&["list-panes", "-t", window, "-F", "#{pane_id}"])?
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `tmux_ok`, never `tmux_run`: the latter inherits stdio, so `display-message -p` would print the
/// window id to the terminal — and every `bn edit` verb runs from a `run-shell` keybinding, where
/// any stdout at all flashes a pager over the session.
fn window_exists(window: &str) -> bool {
    !window.is_empty() && tmux_ok(&["display-message", "-p", "-t", window, "#{window_id}"])
}

/// The parked window's label — the sanitized branch, so the hold session reads like a list of
/// worktrees rather than a wall of ids.
fn window_label(sess: &EditSession) -> String {
    let b = crate::wt::sanitize_branch(&sess.target.branch);
    if b.is_empty() {
        sess.target.repo.clone()
    } else {
        b
    }
}

fn ensure_hold() {
    if tmux_ok(&["has-session", "-t", &format!("={}", editor::HOLD_SESSION)]) {
        return;
    }
    let argv = pane::ensure_hold_cmd();
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let _ = tmux_ok(&refs);
}

fn run_all(cmds: &[Vec<String>]) {
    for cmd in cmds {
        let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
        let _ = tmux_ok(&refs);
    }
}

fn where_line(sess: &EditSession) -> String {
    let p = sess.pane.as_ref();
    format!(
        "{}/{}  {}  {}  {}  {}  sock={}",
        sess.target.repo,
        sess.target.branch,
        sess.state.label(),
        p.map(|p| p.pane.as_str()).unwrap_or("-"),
        p.map(|p| p.window.as_str()).unwrap_or("-"),
        sess.shape.label(),
        sess.socket.display(),
    )
}

fn report(
    sess: &EditSession,
    action: &str,
    before: EditState,
    side: Side,
    landed: Option<&land::LandStatus>,
) -> Envelope {
    let p = sess.pane.as_ref();
    Envelope::success(
        "edit",
        serde_json::json!({
            "action": action,
            "repo": sess.target.repo,
            "branch": sess.target.branch,
            "worktree": sess.target.worktree.display().to_string(),
            "key": editor::worktree_key(&sess.target.worktree),
            "socket": sess.socket.display().to_string(),
            "alive": sess.alive,
            "rpc": pane::is_nvim(&pane::editor_bin()),
            "state": sess.state.label(),
            "prev_state": before.label(),
            "shape": sess.shape.label(),
            "side": side.label(),
            "landed": landed.map(|l| serde_json::json!({
                "ok": l.ok,
                "applied": l.landed,
                "did": l.did,
                "why": l.why,
            })),
            "pane": p.map(|p| p.pane.clone()),
            "window": p.map(|p| p.window.clone()),
            "session": p.map(|p| p.session.clone()),
            "home_window": p.map(|p| p.home.clone()),
        }),
    )
}
