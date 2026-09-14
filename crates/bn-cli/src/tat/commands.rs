//! The command section of the `bn tat` sub-picker — the verbs, as rows, below the places.
//!
//! Split out of `tat.rs` rather than grown inside it (the 1000-line ratchet; see
//! `.file-size-baseline`). The division is the same one the rows themselves draw: `tat.rs` answers
//! "where can I go?", and this module answers "what can I do?". The roster of verbs lives further
//! out still, in [`bn_core::wt_actions`], so the web board can render the same list.

/// The repo's own job scripts, as rows below the roster verbs.
pub(in crate::tat) mod scripts;

use super::{pool_eligible, ref_repo, Roots};
use bn_core::resolver::Config;

/// The glyph a command row leads with. Deliberately unlike the place rows' `+` / `PR` / `◍` / `○`,
/// because the two halves of this list are different kinds of thing: a place row answers "go
/// where?", a command row answers "do what?", and Enter means something different on each.
pub(super) const CMD_GLYPH: char = '▸';

/// The band between the places and the verbs.
///
/// It is an ordinary row because fzf has no inert one, and it is made of dashes because that is
/// what keeps it out of the way: typing a branch name never fuzzy-matches a rule. Selecting it does
/// nothing (the `cmdsep` arm in [`open_project`]), which is the [`cs-auth`] precedent — a synthetic
/// row short-circuited before the normal payload parsing.
pub(super) const CMD_SEPARATOR: &str =
    " ───────────────────────────  commands  ───────────────────────────";

/// What one verb's window says right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CmdState {
    /// Running in its window, and for how long when the run's marker says. The row spins.
    Running(Option<i64>),
    /// The last run failed and kept its window to be read, with its exit status when tmux has it.
    Failed(Option<i32>),
}

/// The state of every verb that has a window in the repo's session, keyed by row id.
pub(crate) type CmdStates = std::collections::BTreeMap<String, CmdState>;

/// Build [`CmdStates`] for `repo` from the job windows in its session.
///
/// One `tmux list-panes` per paint answers for the whole section. A window named after a roster job
/// keys that verb's row; any other name keys a script row (`script:<name>`), and the caller trims
/// keys no row was drawn for. A success closes its window, so an idle verb simply has no entry.
pub(crate) fn command_states(repo: &str, now: i64) -> CmdStates {
    use crate::cmd::wt_action_run::WindowState;
    let mut out = CmdStates::new();
    for (name, window) in crate::cmd::wt_action_run::window_states(repo) {
        let key = match bn_core::wt_actions::for_scope(bn_core::wt_actions::Scope::Repo)
            .find(|a| crate::cmd::wt_action_run::window_name(a, None) == name)
        {
            Some(action) => action.id.to_string(),
            None => scripts::id(&name),
        };
        let state = match window {
            WindowState::Live => {
                let job = bn_core::wt_actions::find(&key).map_or(name.as_str(), |a| a.job);
                CmdState::Running(
                    bn_core::events::running_since(job).map(|since| (now - since).max(0)),
                )
            }
            WindowState::Dead(code) => CmdState::Failed(code),
        };
        out.insert(key, state);
    }
    out
}

/// What the repo's live state says about which verbs are worth offering, and how to annotate them.
///
/// Gathered by the caller, which already holds the [`bn_core::pool::PoolStatus`] it comes from, and
/// passed in — so [`command_rows`] itself stays pure and can be pinned by a test with no repo, no
/// pool and no network.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CmdAvail {
    /// Whether this ref has a pool at all. Pools are a primary-root, untagged concept, so a
    /// `--root win` or `repo@tag` ref has none and must not be offered pool verbs it would refuse.
    pub(super) pooled: bool,
    /// Slots inside the pool's derived size with no live checkout yet — what a warm would build.
    pub(super) cold: usize,
    /// Every slot up to [`bn_core::pool::MAX_POOL_SIZE`] exists, so there is nothing left to cut.
    pub(super) at_ceiling: bool,
}

/// The braille frame for a spinner at `elapsed` seconds.
///
/// Derived from the clock rather than stored, which is the property that makes this cheap: each
/// repaint is an independent process, and there is no state for successive ones to keep in step.
fn spin_frame(now_ms: i64) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[((now_ms / 125).rem_euclid(FRAMES.len() as i64)) as usize]
}

/// A compact `1m12s` / `45s` for a row's elapsed column.
fn brief_elapsed(secs: i64) -> String {
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// The command section: a separator, then one `cmd|<action-id>|<display>` row per offerable verb.
///
/// Order comes from [`bn_core::wt_actions`] rather than from here, so the picker and any other
/// renderer of the same roster agree on it.
///
/// Empty for a non-bare ref. A flat clone has no pool, no worktrees and no PR rows, so a commands
/// header over nothing but noise is worse than no header.
pub(super) fn command_rows(
    bare: bool,
    avail: CmdAvail,
    scripts: &[scripts::RepoScript],
    states: &CmdStates,
    now_ms: i64,
) -> String {
    if !bare {
        return String::new();
    }
    let mut rows = format!("cmdsep||{CMD_SEPARATOR}\n");
    for action in bn_core::wt_actions::for_scope(bn_core::wt_actions::Scope::Repo) {
        let availability = match action.id {
            // The pool verbs are only meaningful where there is a pool, and only honest where
            // there is something left to do.
            "pool-warm" | "pool-grow" if !avail.pooled => continue,
            "pool-grow" if avail.at_ceiling => continue,
            "pool-warm" if avail.cold == 0 => "all slots warm".to_string(),
            "pool-warm" => format!("{} cold", avail.cold),
            _ => String::new(),
        };
        rows.push_str(&command_row(
            action.id,
            action.label,
            availability,
            states,
            now_ms,
        ));
    }
    // The repo's own scripts, after the roster: what every repo can do, then what this one can.
    // The tier is the idle note, because it says where to go to edit the thing.
    for script in scripts {
        rows.push_str(&command_row(
            &scripts::id(&script.name),
            &script.name,
            scripts::tier(script.scope).to_string(),
            states,
            now_ms,
        ));
    }
    rows
}

/// One `cmd|<id>|<display>` row, with the run's state winning the note column.
///
/// What a run is doing beats what the pool looks like: a warm that is *running* has already
/// answered the question "how many are cold?". `idle_note` is what to say when there is no run.
fn command_row(
    id: &str,
    label: &str,
    idle_note: String,
    states: &CmdStates,
    now_ms: i64,
) -> String {
    let (glyph, note) = match states.get(id) {
        Some(CmdState::Running(Some(secs))) => (
            spin_frame(now_ms),
            format!("running · {}", brief_elapsed(*secs)),
        ),
        Some(CmdState::Running(None)) => (spin_frame(now_ms), "running".to_string()),
        Some(CmdState::Failed(Some(code))) => ('✗', format!("failed · exit {code} · window kept")),
        Some(CmdState::Failed(None)) => ('✗', "failed · window kept".to_string()),
        None => (CMD_GLYPH, idle_note),
    };
    // Roster labels are static and separator-free (pinned by a `wt_actions` test) and a script
    // name is refused at discovery if it carries one, but the note is built here, so it takes the
    // same `|`-scrub the PR rows take.
    let note = note.replace(['\r', '\n', '|'], " ");
    let display = format!("  {glyph}  {:<30} {note}", label);
    format!("cmd|{id}|{}\n", display.trim_end())
}

/// The side pane for a command row: what this verb will do, and whether a run is already going.
pub(super) fn command_preview(cfg: &Config, roots: &Roots, project: &str, action_id: &str) {
    let repo = ref_repo(project);
    // A script row is described by its file, not by `describe()`, which knows only the roster.
    if let Some(name) = scripts::name_of(action_id) {
        return scripts::preview(cfg, repo, name);
    }
    let Some(action) = bn_core::wt_actions::find(action_id) else {
        println!("(unknown command: {action_id})");
        return;
    };
    println!("\x1b[1;36m{}\x1b[0m\n", action.label);
    println!("{}\n", describe(action.id));
    println!("Repo: {repo}");
    println!("Job:  {}", action.job);
    println!("      runs in a tmux window in the {repo} session; closes when it exits 0");
    if matches!(action.id, "pool-warm" | "pool-grow") && !pool_eligible(roots, project) {
        println!("\n\x1b[1;31mUnavailable:\x1b[0m {repo} takes the cold path — pools are a");
        println!("primary-root, untagged concept.");
        return;
    }
    run_state_lines(repo, action);
}

/// The run's word on one verb, for a side pane: running, failed, or what Enter will do.
///
/// Shared by the roster pane and the script pane, so both say the same thing about the same window.
pub(super) fn run_state_lines(repo: &str, action: &bn_core::wt_actions::WtAction) {
    use crate::cmd::wt_action_run::WindowState;
    let window = crate::cmd::wt_action_run::window_name(action, None);
    match crate::cmd::wt_action_run::window_states(repo).get(&window) {
        Some(WindowState::Live) => {
            println!("\nRunning now in window `{window}` of the {repo} session.");
            println!("Enter: nothing new starts while it runs.");
        }
        Some(WindowState::Dead(code)) => {
            match code {
                Some(code) => {
                    println!("\nLast run failed (exit {code}); window `{window}` is kept.")
                }
                None => println!("\nLast run failed; window `{window}` is kept."),
            }
            println!("Enter: clear that window and run it again.");
        }
        None => println!("\nEnter: start it now, in a tmux window in the {repo} session."),
    }
}

/// One line on what a verb actually does — the roster carries a label, not an explanation, because
/// a label has to fit in a row and this does not.
fn describe(id: &str) -> &'static str {
    match id {
        "build-main" => "Fetch, fast-forward this repo's main, and run its build.sh there.",
        "pool-warm" => "Give every cold slot a checkout at current main, then build it.",
        "pool-grow" => "Cut one more warm slot and build it, before a dispatch needs it.",
        "wt-cleanup" => "Remove worktrees whose PR has merged, and reclaim their slots.",
        "fetch-prs" => "Fetch every remote so the PR rows above are current.",
        "pull-prs" => "Create a worktree for each open PR of yours that lacks one.",
        "wt-prune" => "Drop worktree registrations git has lost, and over-size or orphaned slots.",
        _ => "",
    }
}

/// Whether the sub-picker should paint again after the step that just finished.
///
/// Lives here because it exists for one reason: a command ran, and the list must show its effect
/// rather than dropping you out of the picker. Selecting a *place* has never needed to say this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// The person went somewhere, or dismissed the picker. Done.
    Done,
    /// A command ran; show the list again with its effect visible.
    Reopen,
}

/// Everything a row can do *without* leaving the picker: the expect-key, and the command rows
/// themselves. `None` means this was an ordinary Enter on a place row — the caller's business.
///
/// One entry point because they share the shape that distinguishes them from the Enter chain: each
/// acts and then repaints, where selecting a place *leaves* for wherever it points.
pub(super) fn act_on_row(
    cfg: &Config,
    roots: &Roots,
    project: &str,
    key: &str,
    kind: &str,
    payload: &str,
) -> Option<Step> {
    match (key, kind) {
        ("ctrl-o", _) => row_menu(cfg, roots, project, kind, payload),
        // The rule between the places and the verbs. Selecting it means nothing, so it does
        // nothing — but it must not drop you out of the picker either.
        (_, "cmdsep") => {}
        (_, "cmd") => run_command(cfg, roots, project, payload),
        _ => return None,
    }
    Some(Step::Reopen)
}

/// `ctrl-o` — the verbs that act on *this* row, in a picker of their own.
///
/// A second fzf spawned by the parent once the first has exited, which is how this codebase does a
/// second interactive surface (see `open_project`'s loop): fzf's `execute()` cannot hand a value
/// back, so a nested picker could not report what was chosen.
pub(super) fn row_menu(cfg: &Config, roots: &Roots, project: &str, kind: &str, payload: &str) {
    // A command row is already a verb: ctrl-o on it asks the same question Enter answers.
    if kind == "cmd" {
        run_command(cfg, roots, project, payload);
        return;
    }
    let Some(scope) = scope_for_row(kind) else {
        eprintln!("✗ ctrl-o acts on a command, window, worktree, PR or slot row");
        crate::picker::hold();
        return;
    };
    let actions: Vec<&bn_core::wt_actions::WtAction> =
        bn_core::wt_actions::for_scope(scope).collect();
    if actions.is_empty() {
        eprintln!("✗ nothing to do to a {} row yet", scope.label());
        crate::picker::hold();
        return;
    }
    let Some((branch, _path)) = super::rows::row_branch(cfg, roots, project, kind, payload) else {
        eprintln!("✗ couldn't find a checkout on disk for that row");
        crate::picker::hold();
        return;
    };
    let repo = ref_repo(project);
    let Some(chosen) = pick_action(&actions, &branch) else {
        return; // dismissed
    };
    match crate::cmd::wt_action_run::start_action(cfg, chosen, repo, Some(&branch)) {
        Ok(started) => println!(
            "✓ {} — {} — {}",
            chosen.label,
            branch,
            started.describe(repo)
        ),
        Err(e) => {
            eprintln!("✗ {e}");
            crate::picker::hold();
        }
    }
}

/// Which roster scope a place row belongs to, or `None` for the rows that *are* verbs.
///
/// A window and a leased slot both stand in for a worktree — you act on the branch checked out
/// there, not on the window or the slot number — so all three map to one scope.
fn scope_for_row(kind: &str) -> Option<bn_core::wt_actions::Scope> {
    use bn_core::wt_actions::Scope;
    match kind {
        "worktree" | "window" | "poolbranch" => Some(Scope::Worktree),
        "remotepr" => Some(Scope::Pr),
        "poolidle" => Some(Scope::PoolSlot),
        _ => None,
    }
}

/// Show the scoped verbs in a small fzf and return the chosen one.
fn pick_action<'a>(
    actions: &[&'a bn_core::wt_actions::WtAction],
    branch: &str,
) -> Option<&'a bn_core::wt_actions::WtAction> {
    let choices: Vec<(String, String)> = actions
        .iter()
        .map(|a| (a.id.to_string(), a.label.to_string()))
        .collect();
    let id = pick_one(&choices, branch)?;
    actions.iter().copied().find(|a| a.id == id)
}

/// Show `choices` as `id \t label` in a small fzf and return the chosen id.
///
/// `--with-nth=2` hides the id, matching the hidden-column convention every picker here uses. A
/// second fzf spawned by the parent once the first has exited, because fzf's `execute()` cannot
/// hand a value back — which is why `ctrl-o` is in `--expect` rather than a `--bind`.
///
/// No fzf on PATH is the same fallback the rest of the module takes: say so and do nothing.
pub(super) fn pick_one(choices: &[(String, String)], header: &str) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let rows: String = choices
        .iter()
        .map(|(id, label)| format!("{id}\t  {label}\n"))
        .collect();
    let child = Command::new("fzf")
        .arg("--reverse")
        .arg("--delimiter=\t")
        .arg("--with-nth=2")
        .arg(format!("--header={header}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("✗ fzf is not on PATH — no row menu");
        crate::picker::hold();
        return None;
    };
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        let _ = stdin.write_all(rows.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(text.lines().next()?.split('\t').next()?.to_string())
}

/// The `runcmd` binding's entry point: unpack fzf's `{1}~{2}` and act, or say why not.
///
/// Guarded the way `ctrl-d` and `ctrl-r` are, and for the same reason — fzf fires a binding on
/// whatever happens to be focused, so a stage that trusts its payload will one day be handed a
/// window row. [`command_block`] is the pure half, so the refusal is testable without a picker.
pub(super) fn command_from_pick(cfg: &Config, roots: &Roots, project: &str, arg: &str) {
    let (kind, id) = arg.split_once('~').unwrap_or(("", arg));
    if let Some(why) = command_block(kind, id) {
        eprintln!("✗ {why}");
        crate::picker::hold();
        return;
    }
    run_command(cfg, roots, project, id);
}

/// Why this row cannot be run as a command, or `None` when it can.
///
/// Pure and total, so every refusal has a test rather than a hope.
pub(super) fn command_block(kind: &str, id: &str) -> Option<String> {
    if kind == "cmdsep" {
        return Some("that row is the separator, not a command".to_string());
    }
    if kind != "cmd" {
        return Some(format!(
            "the command keys act on a command row — focus one below the rule (this is a {} row)",
            if kind.is_empty() { "place" } else { kind }
        ));
    }
    if scripts::resolve(id).is_none() {
        return Some(format!("no such command: {id}"));
    }
    None
}

/// Act on a `cmd` row: start the verb it names, now, in a window in the repo's session.
///
/// A verb that is already running is left alone and says so. That is what keeps the command section
/// safe in a list you also type branch names into: "Build latest main" does fuzzy-match a typed
/// `main`, and an accidental Enter on a running build must not start a second one.
pub(super) fn run_command(cfg: &Config, roots: &Roots, project: &str, action_id: &str) {
    let Some(action) = scripts::resolve(action_id) else {
        // The row was painted by an older build of the binary whose roster still had this verb.
        eprintln!("✗ no such command: {action_id}");
        crate::picker::hold();
        return;
    };
    let repo = ref_repo(project);
    // A script row can outlive its file. Refuse here, where it can be read, rather than opening a
    // window that fails with `no such job` and closes.
    if let Some(name) = scripts::name_of(action_id) {
        if scripts::on_disk(cfg, repo, name).is_none() {
            eprintln!("✗ no `{name}.sh` for {repo} any more — ctrl-r reloads the list");
            crate::picker::hold();
            return;
        }
    }
    if matches!(action.id, "pool-warm" | "pool-grow") && !pool_eligible(roots, project) {
        eprintln!("✗ {repo} takes the cold path — pools are a primary-root, untagged concept");
        crate::picker::hold();
        return;
    }
    match crate::cmd::wt_action_run::start_action(cfg, action, repo, None) {
        Ok(started) => println!("✓ {} — {}", action.label, started.describe(repo)),
        Err(e) => {
            eprintln!("✗ {e}");
            crate::picker::hold();
        }
    }
}

// --- the spinner's repaint loop --------------------------------------------

/// A listening endpoint for one picker's fzf, and the key that makes it usable only by us.
///
/// TCP rather than a unix socket: fzf 0.73 advertises `--listen=SOCKET_PATH` but never creates the
/// socket, while the port form works — verified against the installed binary, not assumed.
///
/// **The key is not optional.** `--listen` accepts `reload(<shell command>)`, so an unauthenticated
/// port is arbitrary local command execution for as long as the picker is open — far too much to
/// spend on a spinner. fzf gates the endpoint on `FZF_API_KEY` (401 without it), so each picker
/// gets a fresh random one that never leaves this process and its child.
pub(super) struct Listen {
    pub(super) port: u16,
    pub(super) key: String,
}

impl Listen {
    /// Reserve a free loopback port and mint a key, or `None` if no port is free.
    ///
    /// The port comes from asking the OS for an ephemeral one and immediately releasing it, so the
    /// number is known to have been free a moment ago. That leaves a small race with anything else
    /// binding in between; losing it costs the spinner, not the picker.
    pub(super) fn reserve() -> Option<Listen> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .ok()?
            .local_addr()
            .ok()?
            .port();
        Some(Listen {
            port,
            key: random_key()?,
        })
    }
}

/// 128 bits from the OS CSPRNG, hex-encoded, or `None` when the kernel will not supply them.
///
/// **Deliberately without a fallback**, unlike `dispatch::fallback_uuid`, which degrades to a
/// clock-seeded PRNG when `/dev/urandom` is unreadable. That is right for a UUID, which only has to
/// be *unique*; this key has to be *unguessable*. A key derived from the pid and the clock is
/// derivable by anyone who can read `/proc` — and what it guards is an endpoint that accepts
/// `reload(<shell command>)`, so guessing it is local command execution as this user.
///
/// So a failure here returns `None`, [`Listen::reserve`] gives up, and the picker spawns with no
/// listener at all: the spinner stops animating, which is the correct thing to lose.
fn random_key() -> Option<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The `--listen` flag for a reserved endpoint, or nothing.
///
/// Returned as args rather than applied to a `Command`, so the caller keeps fzf's builder chain and
/// a picker that could not reserve a port (or mint a key) simply spawns without the flag — it still
/// works, it just does not animate.
pub(super) fn listen_args(listen: Option<&Listen>) -> Vec<String> {
    listen
        .map(|l| vec![format!("--listen=127.0.0.1:{}", l.port)])
        .unwrap_or_default()
}

/// The key fzf checks incoming requests against. Reaches the child through its environment and
/// nowhere else — it is never on a command line any other process can read.
pub(super) fn listen_env(listen: Option<&Listen>) -> Vec<(String, String)> {
    listen
        .map(|l| vec![("FZF_API_KEY".to_string(), l.key.clone())])
        .unwrap_or_default()
}

/// Repaints the picker while a command is running, so its row's spinner advances.
///
/// A row can only *show* progress if something repaints it, and fzf has no timer — so a thread
/// posts `reload-sync` to the listener while there is something to watch. Three things keep this
/// cheap and safe:
///
/// * **It only runs while work does.** Nothing running means no thread at all, so a quiet picker
///   costs exactly what it did before this existed.
/// * **`reload-sync`, not `reload`.** The new list is built before the old one replaces it, so the
///   list never blinks empty under the cursor.
/// * **fzf keeps the typed query across a reload**, which matters more here than it sounds: typing
///   a branch name is how you lease a slot in this picker, and a repaint mid-word must not eat it.
///
/// Stops on drop — and independently when the picker exits, since the next post then fails.
pub(super) struct Ticker {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Ticker {
    /// How often to repaint. Fast enough to read as motion, slow enough that each repaint (one
    /// `bn` process building the rows) is a rounding error against a build that runs for minutes.
    const INTERVAL_MS: u64 = 700;

    /// Start repainting, or return an inert ticker when there is nothing to watch.
    /// `states` is the section the caller just painted, passed in rather than re-derived: asking
    /// again would cost a second tmux call for an answer already in hand.
    pub(super) fn start(
        listen: Option<&Listen>,
        exe: &std::path::Path,
        project: &str,
        root_flag: &str,
        states: &CmdStates,
    ) -> Ticker {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Repaint only while something runs: a failed row is static until someone acts on it.
        let watch = states.values().any(|s| matches!(s, CmdState::Running(_)));
        let Some(listen) = listen.filter(|_| watch) else {
            return Ticker { stop };
        };
        let action = format!(
            "reload-sync({} tat {}{} --pick-stage subtick)",
            crate::shq(&exe.display().to_string()),
            crate::shq(project),
            root_flag
        );
        let (port, key, stop_thread) = (listen.port, listen.key.clone(), stop.clone());
        std::thread::spawn(move || {
            use std::io::Write;
            while !stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(Ticker::INTERVAL_MS));
                if stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                // The listener speaks HTTP and the action is the body. A refused connection means
                // the picker has gone, which is the loop's exit condition rather than an error.
                let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
                    return;
                };
                let req = format!(
                    "POST / HTTP/1.1\r\nHost: localhost\r\nX-API-Key: {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{action}",
                    action.len()
                );
                if stream.write_all(req.as_bytes()).is_err() {
                    return;
                }
            }
        });
        Ticker { stop }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        // Not joined: the thread is sleeping up to one interval and the picker has already
        // returned, so waiting on it would add latency to every keystroke that closes the list.
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod command_row_tests;
