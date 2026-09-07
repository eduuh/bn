//! The command section of the `bn tat` sub-picker — the verbs, as rows, below the places.
//!
//! Split out of `tat.rs` rather than grown inside it (the 1000-line ratchet; see
//! `.file-size-baseline`). The division is the same one the rows themselves draw: `tat.rs` answers
//! "where can I go?", and this module answers "what can I do?". The roster of verbs lives further
//! out still, in [`bn_core::wt_actions`], so the web board can render the same list.
/// The queue actions `ctrl-o` offers on a command row. Declared here rather than in `tat.rs`
/// because this is its only caller.
pub(super) mod queue_actions;

/// The `Queue settings…` row and its menu.
pub(super) mod settings;

use super::queue_view::{held_reasons, CmdState, CmdStates};
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
    states: &CmdStates,
    now_ms: i64,
    settings_note: &str,
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
        // What the queue is doing wins the note column over what the pool looks like: a warm that
        // is *running* has already answered the question "how many are cold?".
        let (glyph, note) = match states.get(action.id) {
            Some(CmdState::Running(secs)) => (
                spin_frame(now_ms),
                format!("running · {}", brief_elapsed(*secs)),
            ),
            Some(CmdState::Away) => ('⏾', "queued for when you're away".to_string()),
            // Name the reason when the dispatcher has recorded one. "queued" alone reads the same
            // whether the entry starts in two seconds or never, which is precisely the ambiguity
            // that let eight verbs sit for three days looking normal.
            Some(CmdState::Queued(Some(held))) => (
                '⏳',
                match held.waiting_secs {
                    // `in_words` rather than `brief_elapsed`: a spinner's elapsed time is seconds
                    // and minutes, but a *held* entry can wait for hours, and `234m55s` is a number
                    // the reader has to divide before it means anything.
                    Some(secs) if secs >= 60 => format!(
                        "held · {} · {}",
                        held.reason,
                        crate::cmd::queue_when::in_words(secs)
                    ),
                    _ => format!("held · {}", held.reason),
                },
            ),
            Some(CmdState::Queued(None)) => ('⏳', "queued".to_string()),
            None => (CMD_GLYPH, availability),
        };
        // Labels are static and separator-free (pinned by a `wt_actions` test), but the note is
        // built here, so it takes the same `|`-scrub the PR rows take.
        let note = note.replace(['\r', '\n', '|'], " ");
        let display = format!("  {glyph}  {:<30} {note}", action.label);
        rows.push_str(&format!("cmd|{}|{}\n", action.id, display.trim_end()));
    }
    // Last, and its own kind: the verbs above are work, this is the machine's mind about the work.
    // A separate kind keeps it out of `wt_actions`, which is a roster of *jobs* — a settings row
    // there would need a job name it does not have.
    let note = settings_note.replace(['\r', '\n', '|'], " ");
    let row = format!("  \u{2699}  {:<30} {note}", "Queue settings…");
    rows.push_str(&format!("qset||{}\n", row.trim_end()));
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
    if cfg.queue.is_heavy(action.job) {
        println!("      heavy — admitted like an agent, built in its own tmux window");
    }
    if matches!(action.id, "pool-warm" | "pool-grow") && !pool_eligible(roots, project) {
        println!("\n\x1b[1;31mUnavailable:\x1b[0m {repo} takes the cold path — pools are a");
        println!("primary-root, untagged concept.");
        return;
    }
    let id = crate::cmd::wt_action_jobs::action_entry_id(action, repo, None);
    // `latest` rather than `load`: this pane re-execs on every cursor move, and folding the whole
    // append-only store to answer about one entry is the kind of cost a8f5776 spent 150ms of budget
    // bounding elsewhere in this module.
    match bn_core::backlog::latest(&bn_core::backlog::backlog_path(), repo, &id)
        .filter(|e| e.status.is_active())
    {
        Some(entry) => {
            println!("\nQueued as {} ({}).", entry.id, entry.status.label());
            // Why it is waiting, from the dispatcher's own record of the last pass. A pane that
            // said only "queued" left the reader with no way to tell work that is about to start
            // from work that never will.
            if let Some(held) = held_reasons().get(&entry.id) {
                match held.waiting_secs {
                    Some(secs) => println!(
                        "Held: {} — waiting {}.",
                        held.reason,
                        brief_elapsed(secs.max(0))
                    ),
                    None => println!("Held: {}.", held.reason),
                }
                println!("`bn queue why` has the whole pass.");
            }
            println!("\nEnter: take it back off the queue.");
        }
        // Say up front when Enter cannot do what the row implies. The pane is where someone looks
        // before pressing a key, so a verb the machine cannot run should read as unavailable here
        // rather than as a `✓ queued` that never becomes anything.
        None if !bn_core::queue::daemon::is_running() => {
            if !cfg.queue_enabled_here() {
                println!(
                    "\n\x1b[1;31mUnavailable:\x1b[0m the queue is off on this machine ('{}').",
                    cfg.machine()
                );
                println!("Add `queue = true` under [machines.{}].", cfg.machine());
            } else {
                println!("\n\x1b[1;31mUnavailable:\x1b[0m no dispatcher is running.");
                println!("Any `bn` command starts one, or run `bn queue run`.");
            }
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
        // Both keys land on the same place: there is one thing to do to a settings row, and a
        // person who reaches for ctrl-o on it has asked the same question Enter answers.
        (_, "qset") => settings::menu(cfg),
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
    // A command row's menu is a different question: not "what can I do to this branch" but "when
    // should this verb run". `ctrl-o` used to refuse here, which left the schedule with no way in.
    if kind == "cmd" {
        match bn_core::wt_actions::find(payload) {
            Some(action) => queue_actions::menu(cfg, action, ref_repo(project)),
            None => {
                eprintln!("✗ no such command: {payload}");
                crate::picker::hold();
            }
        }
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
    let id = crate::cmd::wt_action_jobs::action_entry_id(action, repo, None);
    let already_filed = bn_core::backlog::latest(&bn_core::backlog::backlog_path(), repo, &id)
        .is_some_and(|e| e.status.is_active());
    if already_filed {
        match bn_core::backlog::cancel(
            &bn_core::backlog::backlog_path(),
            repo,
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
    /// `states` is the section the caller just painted, passed in rather than re-derived: asking
    /// again would cost a second full backlog parse for an answer already in hand.
    pub(super) fn start(
        listen: Option<&Listen>,
        exe: &std::path::Path,
        project: &str,
        root_flag: &str,
        states: &CmdStates,
    ) -> Ticker {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Repaint while anything is *waiting* too, not only while something spins.
        //
        // The condition was `Running` alone, on the reasoning that a static row needs no animation.
        // But a queued row's note now carries a reason and a wait time, and both go stale — and the
        // state it most needs to show, "this finally started", is exactly the transition a frozen
        // row cannot show. Affordable only because the repaint is `subtick` now; against the old
        // `sublist` this would have meant rediscovering worktrees every 700ms for hours.
        let watch = states
            .values()
            .any(|s| matches!(s, CmdState::Running(_) | CmdState::Queued(_)));
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
mod command_row_tests {
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
            command_rows(false, CmdAvail::default(), &CmdStates::new(), 0, ""),
            ""
        );
    }

    #[test]
    fn the_section_leads_with_the_separator() {
        let rows = command_rows(true, CmdAvail::default(), &CmdStates::new(), 0, "");
        assert_eq!(kinds(&rows).first(), Some(&"cmdsep"));
    }

    /// Pools are a primary-root, untagged concept, so offering the pool verbs on a `--root win` or
    /// tagged ref would advertise a key that can only refuse.
    #[test]
    fn an_unpooled_ref_is_offered_no_pool_verbs() {
        let rows = command_rows(true, CmdAvail::default(), &CmdStates::new(), 0, "");
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
        let rows = command_rows(true, avail, &CmdStates::new(), 0, "");
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
        let rows = command_rows(true, avail, &CmdStates::new(), 0, "");
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
        let rows = command_rows(true, avail, &CmdStates::new(), 0, "");
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
        for line in command_rows(true, avail, &CmdStates::new(), 0, "").lines() {
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
        let rows = command_rows(true, avail, &CmdStates::new(), 0, "");
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
        let rows = command_rows(true, pooled(), &states, 0, "");
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
        let rows = command_rows(true, pooled(), &states, 0, "");
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
        let rows = command_rows(true, avail, &states, 0, "");

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
