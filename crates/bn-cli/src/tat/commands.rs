//! The command section of the `bn tat` sub-picker — the verbs, as rows, below the places.
//!
//! Split out of `tat.rs` rather than grown inside it (the 1000-line ratchet; see
//! `.file-size-baseline`). The division is the same one the rows themselves draw: `tat.rs` answers
//! "where can I go?", and this module answers "what can I do?". The roster of verbs lives further
//! out still, in [`bn_core::wt_actions`], so the web board can render the same list.
use super::{pool_eligible, ref_repo, Roots};
use bn_core::resolver::Config;
use bn_core::wt_actions::When;

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

/// What the queue says about one verb right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CmdState {
    /// Running, and for how long. The row spins.
    Running(i64),
    /// Filed to run now, still waiting for capacity.
    Queued,
    /// Filed for the away window.
    Away,
}

/// The queue state of every verb, keyed by action id.
///
/// Read once per paint rather than per row: one backlog load answers for the whole section.
/// Deliberately reports work **this picker did not start** too — a `pool-warm` the cron began is
/// still a pool warm, and a row that only admitted to your own keypresses would be lying about what
/// the machine is doing.
pub(super) type CmdStates = std::collections::BTreeMap<String, CmdState>;

/// Build [`CmdStates`] for `repo` from the backlog.
pub(super) fn command_states(repo: &str, now: i64) -> CmdStates {
    use bn_core::backlog::{BacklogStatus, ExecSpec};
    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    let mut out = CmdStates::new();
    for entry in backlog.entries() {
        if !entry.status.is_active() {
            continue;
        }
        // The entry's own `repo` is the queue's home (`machine`); the repo the work *targets* is
        // inside the exec, which is what a row is asking about.
        let ExecSpec::Job {
            job, repo: target, ..
        } = &entry.exec
        else {
            continue;
        };
        if target.as_deref() != Some(repo) {
            continue;
        }
        let Some(action) = bn_core::wt_actions::all().iter().find(|a| a.job == *job) else {
            continue;
        };
        let state = match entry.status {
            // `updated_at` is stamped when the entry is leased, so on a running entry it is the
            // start time — no separate clock to keep.
            BacklogStatus::Leased => CmdState::Running((now - entry.updated_at).max(0)),
            _ if entry.class_override == Some(bn_core::backlog::ClassOverride::Opportunistic) => {
                CmdState::Away
            }
            _ => CmdState::Queued,
        };
        out.insert(action.id.to_string(), state);
    }
    out
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
pub(super) fn command_rows(bare: bool, avail: CmdAvail, states: &CmdStates, now_ms: i64) -> String {
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
        // What the queue is doing wins the note column over what the pool looks like: a warm that
        // is *running* has already answered the question "how many are cold?".
        let (glyph, note) = match states.get(action.id) {
            Some(CmdState::Running(secs)) => (
                spin_frame(now_ms),
                format!("running · {}", brief_elapsed(*secs)),
            ),
            Some(CmdState::Away) => ('⏾', "queued for when you're away".to_string()),
            Some(CmdState::Queued) => ('⏳', "queued".to_string()),
            None => (CMD_GLYPH, availability),
        };
        // Labels are static and separator-free (pinned by a `wt_actions` test), but the note is
        // built here, so it takes the same `|`-scrub the PR rows take.
        let note = note.replace(['\r', '\n', '|'], " ");
        let display = format!("  {glyph}  {:<30} {note}", action.label);
        rows.push_str(&format!("cmd|{}|{}\n", action.id, display.trim_end()));
    }
    rows
}

/// The side pane for a command row: what this verb will do, and whether it is already filed.
///
/// The queue state is the load-bearing half. Enter on a queued verb *cancels* it, so a pane that
/// only described the verb would leave the one case where Enter does the opposite of what the row
/// says unexplained.
pub(super) fn command_preview(cfg: &Config, roots: &Roots, project: &str, action_id: &str) {
    let Some(action) = bn_core::wt_actions::find(action_id) else {
        println!("(unknown command: {action_id})");
        return;
    };
    let repo = ref_repo(project);
    println!("\x1b[1;36m{}\x1b[0m\n", action.label);
    println!("{}\n", describe(action.id));
    println!("Repo: {repo}");
    println!("Job:  {}", action.job);
    if bn_core::queue::jobs::is_heavy(action.job) {
        println!("      heavy — admitted like an agent, built in its own tmux window");
    }
    if matches!(action.id, "pool-warm" | "pool-grow") && !pool_eligible(roots, project) {
        println!("\n\x1b[1;31mUnavailable:\x1b[0m {repo} takes the cold path — pools are a");
        println!("primary-root, untagged concept.");
        return;
    }
    let queue_repo = cfg.queue_home_repo();
    let id = crate::cmd::wt_action_jobs::action_entry_id(action, repo, None);
    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    match backlog
        .get(&queue_repo, &id)
        .filter(|e| e.status.is_active())
    {
        Some(entry) => {
            println!("\nQueued as {} ({}).", entry.id, entry.status.label());
            println!("\nEnter: take it back off the queue.");
        }
        None => println!("\nEnter: queue it to run now."),
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

/// Everything a row can do *without* leaving the picker: the two expect-keys, and the command rows
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
        ("ctrl-s", _) => defer_from_row(cfg, roots, project, kind, payload),
        ("ctrl-o", _) => row_menu(cfg, roots, project, kind, payload),
        // The rule between the places and the verbs. Selecting it means nothing, so it does
        // nothing — but it must not drop you out of the picker either.
        (_, "cmdsep") => {}
        (_, "cmd") => run_command(cfg, roots, project, payload, When::Now),
        _ => return None,
    }
    Some(Step::Reopen)
}

/// `ctrl-s` — file the focused command row for when you are idle or gone.
///
/// The end-of-day key. It is the same enqueue Enter performs, differing only in the class the entry
/// carries ([`When::Away`] → opportunistic), which the existing away window already admits. Refuses
/// a place row in one line rather than guessing: "later" is a statement about a verb, and a
/// worktree is not one.
pub(super) fn defer_from_row(
    cfg: &Config,
    roots: &Roots,
    project: &str,
    kind: &str,
    payload: &str,
) {
    if let Some(why) = command_block(kind, payload) {
        eprintln!("✗ {why}");
        crate::picker::hold();
        return;
    }
    run_command(cfg, roots, project, payload, When::Away);
}

/// `ctrl-o` — the verbs that act on *this* row, in a picker of their own.
///
/// A second fzf spawned by the parent once the first has exited, which is how this codebase does a
/// second interactive surface (see `open_project`'s loop): fzf's `execute()` cannot hand a value
/// back, so a nested picker could not report what was chosen.
pub(super) fn row_menu(cfg: &Config, roots: &Roots, project: &str, kind: &str, payload: &str) {
    let Some(scope) = scope_for_row(kind) else {
        eprintln!(
            "✗ {kind} rows are commands already — ctrl-o acts on a window, worktree, PR or slot"
        );
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
    match crate::cmd::wt_action_jobs::enqueue_action(cfg, chosen, repo, Some(&branch), When::Now) {
        Ok(outcome) => println!("✓ {} — {} ({})", chosen.label, branch, outcome.label()),
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
///
/// `id \t label` with `--with-nth=2`, matching the hidden-column convention every picker here uses.
/// No fzf on PATH is the same fallback the rest of the module takes: say so and do nothing.
fn pick_action<'a>(
    actions: &[&'a bn_core::wt_actions::WtAction],
    branch: &str,
) -> Option<&'a bn_core::wt_actions::WtAction> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let rows: String = actions
        .iter()
        .map(|a| format!("{}\t  {}\n", a.id, a.label))
        .collect();
    let child = Command::new("fzf")
        .arg("--reverse")
        .arg("--delimiter=\t")
        .arg("--with-nth=2")
        .arg(format!("--header={branch}"))
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
    let id = text.lines().next()?.split('\t').next()?;
    actions.iter().find(|a| a.id == id).copied()
}

/// The `runcmd` bindings' entry point: unpack fzf's `{1}~{2}` and act, or say why not.
///
/// Guarded the way `ctrl-d` and `ctrl-r` are, and for the same reason — fzf fires a binding on
/// whatever happens to be focused, so a stage that trusts its payload will one day be handed a
/// window row. [`command_block`] is the pure half, so the refusal is testable without a picker.
pub(super) fn command_from_pick(cfg: &Config, roots: &Roots, project: &str, arg: &str, when: When) {
    let (kind, id) = arg.split_once('~').unwrap_or(("", arg));
    if let Some(why) = command_block(kind, id) {
        eprintln!("✗ {why}");
        crate::picker::hold();
        return;
    }
    run_command(cfg, roots, project, id, when);
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
    if bn_core::wt_actions::find(id).is_none() {
        return Some(format!("no such command: {id}"));
    }
    None
}

/// Act on a `cmd` row: queue the verb it names, or take it back if it is already filed.
///
/// Queued rather than run here, and that is the load-bearing choice. A row can only show progress
/// for work something other than this short-lived process can see, and the queue's heavy path
/// already opens a visible tmux window to build in — so nothing is lost by not spawning one
/// directly, and the row gains a state, a start time and a finish callback.
///
/// **Selecting a verb that is already queued cancels it.** That is what makes the command section
/// safe to put in a list you also type branch names into: "Build latest main" does fuzzy-match a
/// typed `main`, so an accidental Enter is possible — and the answer is to make the mistake cost one
/// keystroke to undo rather than to contort the labels until they stop matching.
pub(super) fn run_command(cfg: &Config, roots: &Roots, project: &str, action_id: &str, when: When) {
    let Some(action) = bn_core::wt_actions::find(action_id) else {
        // The row was painted by an older build of the binary whose roster still had this verb.
        eprintln!("✗ no such command: {action_id}");
        crate::picker::hold();
        return;
    };
    let repo = ref_repo(project);
    if matches!(action.id, "pool-warm" | "pool-grow") && !pool_eligible(roots, project) {
        eprintln!("✗ {repo} takes the cold path — pools are a primary-root, untagged concept");
        crate::picker::hold();
        return;
    }
    let queue_repo = cfg.queue_home_repo();
    let id = crate::cmd::wt_action_jobs::action_entry_id(action, repo, None);
    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    if backlog.is_active(&queue_repo, &id) {
        match bn_core::backlog::cancel(
            &bn_core::backlog::backlog_path(),
            &queue_repo,
            &id,
            bn_core::util::now_epoch_secs(),
        ) {
            Ok(_) => println!("✓ un-queued {}", action.label),
            Err(e) => {
                eprintln!("✗ could not un-queue {}: {e}", action.label);
                crate::picker::hold();
            }
        }
        return;
    }
    match crate::cmd::wt_action_jobs::enqueue_action(cfg, action, repo, None, when) {
        Ok(outcome) => {
            let tail = match when {
                When::Now => "queued",
                When::Away => "queued for when you're away",
            };
            println!("✓ {} — {tail} ({})", action.label, outcome.label());
        }
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
    pub(super) fn start(
        listen: Option<&Listen>,
        exe: &std::path::Path,
        project: &str,
        root_flag: &str,
    ) -> Ticker {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let repo = ref_repo(project);
        let running = command_states(repo, bn_core::util::now_epoch_secs())
            .values()
            .any(|s| matches!(s, CmdState::Running(_)));
        let Some(listen) = listen.filter(|_| running) else {
            return Ticker { stop };
        };
        let action = format!(
            "reload-sync({} tat {}{} --pick-stage sublist)",
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
mod command_row_tests {
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
            command_rows(false, CmdAvail::default(), &CmdStates::new(), 0),
            ""
        );
    }

    #[test]
    fn the_section_leads_with_the_separator() {
        let rows = command_rows(true, CmdAvail::default(), &CmdStates::new(), 0);
        assert_eq!(kinds(&rows).first(), Some(&"cmdsep"));
    }

    /// Pools are a primary-root, untagged concept, so offering the pool verbs on a `--root win` or
    /// tagged ref would advertise a key that can only refuse.
    #[test]
    fn an_unpooled_ref_is_offered_no_pool_verbs() {
        let rows = command_rows(true, CmdAvail::default(), &CmdStates::new(), 0);
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
        let rows = command_rows(true, avail, &CmdStates::new(), 0);
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
        let rows = command_rows(true, avail, &CmdStates::new(), 0);
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
        let rows = command_rows(true, avail, &CmdStates::new(), 0);
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
            &CmdStates::new(),
            0,
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
        for line in command_rows(true, avail, &CmdStates::new(), 0).lines() {
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
        let rows = command_rows(true, avail, &CmdStates::new(), 0);
        for line in rows.lines().filter(|l| l.starts_with("cmd|")) {
            let id = line.split('|').nth(1).unwrap();
            let action = bn_core::wt_actions::find(id).expect(id);
            assert_eq!(action.scope, bn_core::wt_actions::Scope::Repo, "{id}");
        }
        assert!(!rows.contains("wt-verify"), "{rows}");
    }

    /// The frame comes from the clock, not from a counter, because every repaint is a *separate
    /// process* — there is nothing for successive frames to keep in step with.
    #[test]
    fn the_spinner_advances_with_the_clock_and_never_leaves_the_cycle() {
        let frames: std::collections::BTreeSet<char> =
            (0..10).map(|i| spin_frame(i * 125)).collect();
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
        states.insert("pull-prs".to_string(), CmdState::Queued);
        let rows = command_rows(true, avail, &states, 0);

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
    }
}
