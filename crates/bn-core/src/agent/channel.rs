//! Join a supervised agent to the tmux window that hosts it — the "can bn write to this agent?"
//! question, answered once per supervisor pass and reused by every steering command.
//!
//! Window names are presentation, not identity (the point [`super::lease`] already makes): dispatch
//! names a window after the branch with `/` → `-`, a hand-made window is named after whatever
//! directory it was opened in, and a pooled worktree (`_pool/N`) shares neither name with its
//! branch. Matching a `repo:branch` string against window names therefore misses agents that are
//! perfectly steerable — which is exactly how the Tmux control channel came to be unreachable code.
//!
//! So the join is tiered, strongest evidence first, and every tier is pure so the whole thing is
//! testable from a captured `list-windows` string:
//!
//! 1. an agent lease recording this harness session id — bn launched it and wrote down where;
//! 2. a window stamped `@bn_session_id` with this session — same evidence, read off the window;
//! 3. the **worktree path**: the agent's transcript records its cwd and every window reports
//!    `pane_current_path`, so a checkout identifies its window without either side being tagged.
//!    This is the tier that resolves a session started by hand;
//! 4. a window stamped `@bn_repo`/`@bn_branch`;
//! 5. the window *name*, `<repo>:<tmux_window_name(branch)>`.
//!
//! Tiers 1-3 identify a *session* or a *checkout*, so two agents on one branch stay individually
//! addressable. Only the branch-keyed tiers 4-5 have to fall back on refusing (`Ambiguous`) when a
//! branch hosts more than one live agent, because there the evidence genuinely cannot tell them
//! apart.

use super::lease::AgentLease;
use super::supervise::ControlChannel;
use std::collections::BTreeMap;
use std::path::Path;

/// The `list-windows -F` format every join reads. One spawn per supervisor pass yields all of it;
/// the parsing is [`parse_window_rows`].
pub const WINDOW_FORMAT: &str = "#{session_name}:#{window_name}\t#{pane_current_path}\t#{@bn_worktree}\t#{@bn_repo}\t#{@bn_branch}\t#{@bn_session_id}\t#{pane_dead}";

/// One live tmux window, as [`WINDOW_FORMAT`] describes it. Unset `@bn_*` options come back as the
/// empty string, which every tier treats as "no evidence" rather than a match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TmuxWindowRow {
    /// `session:window` — the exact target, without the leading `=` that guards fnmatch.
    pub target: String,
    pub pane_path: String,
    pub worktree: String,
    pub repo: String,
    pub branch: String,
    pub session_id: String,
}

impl TmuxWindowRow {
    /// The checkout this window is sitting in: the explicit `@bn_worktree` stamp when a launcher
    /// left one, else wherever the pane actually is.
    fn path(&self) -> &str {
        if self.worktree.is_empty() {
            &self.pane_path
        } else {
            &self.worktree
        }
    }
}

/// What a supervised agent knows about itself before the join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentKey<'a> {
    pub session_id: &'a str,
    pub repo: &'a str,
    pub branch: &'a str,
    /// The transcript's recorded cwd, when the harness wrote one down.
    pub worktree: Option<&'a str>,
}

/// Which kind of evidence produced a match — the difference between "this session" and "something
/// on this branch", which is what [`AgentChannel::ambiguous`] hinges on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchTier {
    Lease,
    WindowSession,
    WorktreePath,
    WindowMeta,
    WindowName,
}

impl MatchTier {
    /// Whether the evidence identifies one session or one checkout, rather than a branch. Exact
    /// tiers survive a branch hosting several agents; the branch-keyed ones cannot.
    pub fn is_exact(self) -> bool {
        matches!(
            self,
            MatchTier::Lease | MatchTier::WindowSession | MatchTier::WorktreePath
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            MatchTier::Lease => "lease",
            MatchTier::WindowSession => "window-session",
            MatchTier::WorktreePath => "worktree-path",
            MatchTier::WindowMeta => "window-meta",
            MatchTier::WindowName => "window-name",
        }
    }
}

/// The join's answer for one agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentChannel {
    /// The exact `session:window` hosting this agent, when one was found.
    pub tmux_target: Option<String>,
    /// Its real checkout — from the lease or the window, which beats reconstructing a path from
    /// `<worktree_dir>/<repo>/<branch>` (wrong for every pooled slot).
    pub worktree: Option<String>,
    pub tier: Option<MatchTier>,
    /// Two agents claimed one window, or this one was matched only by branch while a sibling shares
    /// that branch. Either way no write can be aimed, so the caller must refuse.
    pub ambiguous: bool,
    /// Another live agent shares this agent's `repo:branch`, so every branch-keyed channel (the
    /// note's `agent-session:` record included) is unaimable.
    pub branch_shared: bool,
}

/// Whether the branch note's recorded `agent-session:` names *this* live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedSession {
    /// No `agent-session:` line on the note.
    None,
    /// The record names this session — `--resume` reaches exactly this agent.
    Same,
    /// A record exists but names a different session (a previous agent on the branch, or a
    /// `--continue` launch that adopted the harness's own id).
    Other,
}

/// Resolve every agent's hosting window in one pass.
///
/// Returns one [`AgentChannel`] per input, positionally. Pure: the caller supplies the parsed
/// window list and the lease registry, so the whole tier ladder is testable from a captured string.
pub fn resolve_channels(
    agents: &[AgentKey<'_>],
    windows: &[TmuxWindowRow],
    leases: &[AgentLease],
) -> Vec<AgentChannel> {
    let mut out: Vec<AgentChannel> = agents
        .iter()
        .map(|a| resolve_one(a, windows, leases))
        .collect();

    // A branch hosting several live agents makes every branch-keyed channel unaimable — the tmux
    // window name and the note's `agent-session:` record are both keyed by branch, not session.
    let mut per_branch: BTreeMap<String, usize> = BTreeMap::new();
    for a in agents {
        *per_branch.entry(branch_key(a.repo, a.branch)).or_default() += 1;
    }
    // Two agents pointing at one window is the same problem one level down: a write lands in a pane
    // shared by both, so neither is addressable.
    let mut per_target: BTreeMap<String, usize> = BTreeMap::new();
    for ch in &out {
        if let Some(t) = ch.tmux_target.clone() {
            *per_target.entry(t).or_default() += 1;
        }
    }

    for (ch, a) in out.iter_mut().zip(agents) {
        ch.branch_shared = per_branch
            .get(&branch_key(a.repo, a.branch))
            .is_some_and(|&n| n > 1);
        let collided = ch
            .tmux_target
            .as_deref()
            .and_then(|t| per_target.get(t))
            .is_some_and(|&n| n > 1);
        let branch_keyed = ch.tier.is_some_and(|t| !t.is_exact());
        ch.ambiguous = collided || (ch.branch_shared && branch_keyed);
        if collided {
            // Nothing to hand a caller: the target cannot be aimed and the path may belong to the
            // other agent.
            ch.tmux_target = None;
            ch.worktree = None;
            ch.tier = None;
        }
    }
    out
}

/// Whether the transcript itself can serve as a resume key: supervision found this agent by reading
/// its session log, so it knows the session id, the harness, and the cwd that log recorded. A
/// `--resume <id>` turn run in that checkout reaches exactly that session, whoever started it and
/// whether or not any note wrote it down.
///
/// The checkout is the part that can be missing — a resume turn needs a directory to run in — so
/// this is a two-field fact, not a bool, and [`ObservedSession::usable`] is the only test of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObservedSession<'a> {
    /// The live session id, straight off the transcript.
    pub session_id: &'a str,
    /// The checkout that transcript recorded, once confirmed to still exist.
    pub worktree: Option<&'a str>,
}

impl ObservedSession<'_> {
    /// Whether a resume turn could actually be aimed and run.
    pub fn usable(&self) -> bool {
        !self.session_id.is_empty() && self.worktree.is_some_and(|w| !w.is_empty())
    }
}

/// The channel a resolved agent should advertise, given what its note recorded and what its own
/// transcript proves.
///
/// Ordered so the strongest *aimable* evidence wins: a per-session resume record beats a window,
/// and a branch-keyed anything loses to a branch that hosts more than one agent.
///
/// Every dead end then falls back to the observed session ([`ObservedSession`]) rather than
/// refusing. This is what retires "Observed only": bn discovered the agent by reading its
/// transcript, so it always holds a session id, and a session id is a resume key. It also dissolves
/// most `Ambiguous` cases — that verdict exists because the *branch-keyed* channels (a window named
/// after the branch, the note's branch-keyed `agent-session:` record) genuinely cannot tell two
/// agents on one branch apart. A session id can: two agents on a branch have two transcripts, and
/// each resumes only itself. `Ambiguous` now survives only when the checkout is unknown too, and
/// there is nowhere to run the turn.
///
/// The existing successful channels are untouched — an agent that reaches `Tmux` or `Resume` today
/// still does, by the same evidence. Only the refusals were upgraded.
pub fn control_channel(
    ch: &AgentChannel,
    recorded: RecordedSession,
    observed: ObservedSession<'_>,
) -> ControlChannel {
    // Resume-by-transcript, for the cases the branch-keyed evidence has to refuse.
    let fallback = |refusal: ControlChannel| {
        if observed.usable() {
            ControlChannel::Resume
        } else {
            refusal
        }
    };

    if ch.ambiguous {
        return fallback(ControlChannel::Ambiguous);
    }
    if recorded == RecordedSession::Same {
        return ControlChannel::Resume;
    }
    match (ch.tier, ch.branch_shared, recorded) {
        (Some(_), _, _) => ControlChannel::Tmux,
        (None, true, _) => fallback(ControlChannel::Ambiguous),
        (None, false, RecordedSession::Other) => ControlChannel::Resume,
        (None, false, _) => fallback(ControlChannel::None),
    }
}

fn resolve_one(
    agent: &AgentKey<'_>,
    windows: &[TmuxWindowRow],
    leases: &[AgentLease],
) -> AgentChannel {
    let sid = crate::cost::canonical_session_id(agent.session_id);

    // 1. bn launched it and wrote down where.
    if let Some(l) = leases
        .iter()
        .find(|l| crate::cost::canonical_session_id(&l.session_id) == sid)
    {
        return AgentChannel {
            tmux_target: Some(l.tmux_target.clone()),
            worktree: Some(l.key.worktree.clone()),
            tier: Some(MatchTier::Lease),
            ..Default::default()
        };
    }

    // 2. the window itself carries the session id.
    if let Some(w) = windows.iter().find(|w| {
        !w.session_id.is_empty() && crate::cost::canonical_session_id(&w.session_id) == sid
    }) {
        return hit(w, MatchTier::WindowSession);
    }

    // 3. the checkout — the tier that catches a session bn never launched.
    if let Some(w) = agent.worktree.and_then(|wt| match_by_path(wt, windows)) {
        return hit(w, MatchTier::WorktreePath);
    }

    // 4. the window's own repo/branch stamps.
    let (repo, branch) = (
        agent.repo.trim().to_ascii_lowercase(),
        agent.branch.trim().to_ascii_lowercase(),
    );
    if let Some(w) = windows.iter().find(|w| {
        !w.repo.is_empty()
            && !w.branch.is_empty()
            && w.repo.trim().to_ascii_lowercase() == repo
            && w.branch.trim().to_ascii_lowercase() == branch
    }) {
        return hit(w, MatchTier::WindowMeta);
    }

    // 5. the name dispatch would have given it.
    let named = format!(
        "{repo}:{}",
        crate::orchestrate::tmux_window_name(&branch).to_ascii_lowercase()
    );
    if let Some(w) = windows
        .iter()
        .find(|w| w.target.to_ascii_lowercase() == named)
    {
        return hit(w, MatchTier::WindowName);
    }

    AgentChannel::default()
}

fn hit(w: &TmuxWindowRow, tier: MatchTier) -> AgentChannel {
    AgentChannel {
        tmux_target: Some(w.target.clone()),
        worktree: Some(w.path().to_string()).filter(|p| !p.is_empty()),
        tier: Some(tier),
        ..Default::default()
    }
}

/// The window sitting in `worktree`. An exact path wins outright; otherwise the best *containment*
/// match — one path a proper ancestor of the other, deepest shared prefix first — which covers a
/// pane that has `cd`'d into a subdirectory, or an agent whose transcript cwd is below the window's
/// worktree root. A tie between two unrelated slots (`_pool/4` vs `_pool/6` share only `_pool`)
/// fails the ancestor test and is correctly no match at all.
fn match_by_path<'w>(worktree: &str, windows: &'w [TmuxWindowRow]) -> Option<&'w TmuxWindowRow> {
    let want = Path::new(worktree.trim_end_matches('/'));
    if want.as_os_str().is_empty() {
        return None;
    }
    if let Some(w) = windows
        .iter()
        .find(|w| Path::new(w.path().trim_end_matches('/')) == want)
    {
        return Some(w);
    }
    let mut best: Option<(usize, &TmuxWindowRow)> = None;
    let mut tied = false;
    for w in windows {
        let have = Path::new(w.path().trim_end_matches('/'));
        if have.as_os_str().is_empty() {
            continue;
        }
        let Some(depth) = contained_depth(want, have) else {
            continue;
        };
        match best {
            Some((d, _)) if d > depth => {}
            Some((d, _)) if d == depth => tied = true,
            _ => {
                best = Some((depth, w));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied).map(|(_, w)| w)
}

/// The component count of the shorter path when one of `a`/`b` is an ancestor of the other, else
/// `None`. The depth is the score: a deeper containment is stronger evidence.
fn contained_depth(a: &Path, b: &Path) -> Option<usize> {
    let (short, long) = if a.components().count() <= b.components().count() {
        (a, b)
    } else {
        (b, a)
    };
    long.starts_with(short).then(|| short.components().count())
}

fn branch_key(repo: &str, branch: &str) -> String {
    format!(
        "{}:{}",
        repo.trim().to_ascii_lowercase(),
        branch.trim().to_ascii_lowercase()
    )
}

/// Parse `list-windows -F WINDOW_FORMAT` output. Dead panes are dropped: `remain-on-exit` keeps a
/// crashed harness's window around for inspection, but nothing can be steered into it.
pub fn parse_window_rows(out: &str) -> Vec<TmuxWindowRow> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let target = f.next()?.trim().to_string();
            if target.is_empty() {
                return None;
            }
            let row = TmuxWindowRow {
                target,
                pane_path: f.next().unwrap_or_default().trim().to_string(),
                worktree: f.next().unwrap_or_default().trim().to_string(),
                repo: f.next().unwrap_or_default().trim().to_string(),
                branch: f.next().unwrap_or_default().trim().to_string(),
                session_id: f.next().unwrap_or_default().trim().to_string(),
            };
            let dead = f.next().unwrap_or_default().trim() == "1";
            (!dead).then_some(row)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::lease::{AgentLease, AgentLeaseKey, AgentLeaseKind};

    /// No transcript evidence — isolates the branch-keyed tier ladder from the resume fallback, so
    /// the ladder's own verdicts stay asserted.
    const BLIND: ObservedSession<'static> = ObservedSession {
        session_id: "",
        worktree: None,
    };

    /// A transcript bn actually read: a live session id in a checkout. The resume fallback's input.
    fn seen(id: &str) -> ObservedSession<'_> {
        ObservedSession {
            session_id: id,
            worktree: Some("/Users/e/worktree/bn/_pool/6"),
        }
    }

    /// The real shape of this machine's fleet: three pooled slots whose window names are branches,
    /// one window named after a reused worktree directory that hosts an unrelated branch, and one
    /// plain shell.
    fn windows() -> Vec<TmuxWindowRow> {
        parse_window_rows(
            "bn:main\t/Users/e/worktree/bn/main\t\t\t\t\t0\n\
             bn:bn-web-workflow\t/Users/e/worktree/bn/bn-web-workflow\t\t\t\t\t0\n\
             bn:bn-skills-crud\t/Users/e/worktree/bn/_pool/4\t\t\t\t\t0\n\
             bn:startkworkinbn-zsh\t/Users/e/worktree/bn/_pool/6\t\t\t\t\t0\n",
        )
    }

    fn key<'a>(session: &'a str, branch: &'a str, wt: Option<&'a str>) -> AgentKey<'a> {
        AgentKey {
            session_id: session,
            repo: "bn",
            branch,
            worktree: wt,
        }
    }

    fn lease(session: &str, branch: &str, wt: &str, target: &str) -> AgentLease {
        AgentLease {
            key: AgentLeaseKey {
                repo: "bn".into(),
                branch: branch.into(),
                worktree: wt.into(),
            },
            session_id: session.into(),
            harness: "claude".into(),
            kind: AgentLeaseKind::Dispatch,
            tmux_target: target.into(),
            acquired_at: 0,
        }
    }

    #[test]
    fn parse_window_rows_reads_session_colon_window_and_drops_dead_panes() {
        let rows = parse_window_rows(
            "bn:one\t/wt/one\t/wt/one-stamped\tbn\tone\tSID-1\t0\n\
             bn:dead\t/wt/dead\t\t\t\t\t1\n",
        );
        assert_eq!(rows.len(), 1, "the dead pane is not steerable");
        assert_eq!(rows[0].target, "bn:one");
        assert_eq!(rows[0].session_id, "SID-1");
        // The explicit stamp beats the pane's cwd, which may have wandered into a subdirectory.
        assert_eq!(rows[0].path(), "/wt/one-stamped");
    }

    #[test]
    fn resolve_prefers_lease_session_id_over_every_window_tier() {
        // The window name says `_pool/6`'s branch; the lease says the session lives elsewhere.
        let leases = vec![lease(
            "SID-A",
            "startkworkinbn-zsh",
            "/Users/e/worktree/bn/_pool/9",
            "bn:elsewhere",
        )];
        let got = resolve_channels(
            &[key(
                "sid-a",
                "startkworkinbn-zsh",
                Some("/Users/e/worktree/bn/_pool/6"),
            )],
            &windows(),
            &leases,
        );
        assert_eq!(got[0].tier, Some(MatchTier::Lease));
        assert_eq!(got[0].tmux_target.as_deref(), Some("bn:elsewhere"));
        assert_eq!(
            got[0].worktree.as_deref(),
            Some("/Users/e/worktree/bn/_pool/9")
        );
    }

    #[test]
    fn resolve_matches_a_reused_worktree_dir_by_pane_path() {
        // Window `bn:bn-web-workflow` hosts branch `backlog-task-invariant` — no name tier can find
        // it, and this is the case that leaves an agent stranded on "Observed only" today.
        let got = resolve_channels(
            &[key(
                "sid",
                "backlog-task-invariant",
                Some("/Users/e/worktree/bn/bn-web-workflow"),
            )],
            &windows(),
            &[],
        );
        assert_eq!(got[0].tier, Some(MatchTier::WorktreePath));
        assert_eq!(got[0].tmux_target.as_deref(), Some("bn:bn-web-workflow"));
        assert_eq!(
            control_channel(&got[0], RecordedSession::None, BLIND),
            ControlChannel::Tmux
        );
    }

    #[test]
    fn resolve_matches_a_pool_slot_by_pane_path() {
        let got = resolve_channels(
            &[key(
                "sid",
                "startkworkinbn-zsh",
                Some("/Users/e/worktree/bn/_pool/6"),
            )],
            &windows(),
            &[],
        );
        assert_eq!(got[0].tier, Some(MatchTier::WorktreePath));
        assert_eq!(got[0].tmux_target.as_deref(), Some("bn:startkworkinbn-zsh"));
    }

    #[test]
    fn sibling_pool_slots_do_not_match_each_other() {
        // `_pool/4` and `_pool/6` share a prefix but neither contains the other.
        let got = resolve_channels(
            &[key("sid", "nowhere", Some("/Users/e/worktree/bn/_pool/7"))],
            &windows(),
            &[],
        );
        assert_eq!(got[0].tier, None);
        assert_eq!(
            control_channel(&got[0], RecordedSession::None, BLIND),
            ControlChannel::None
        );
    }

    #[test]
    fn resolve_normalizes_slashes_in_the_window_name_tier() {
        let windows = parse_window_rows("bn:dispatch-foo\t\t\t\t\t\t0\n");
        let got = resolve_channels(&[key("sid", "dispatch/foo", None)], &windows, &[]);
        assert_eq!(got[0].tier, Some(MatchTier::WindowName));
        assert_eq!(got[0].tmux_target.as_deref(), Some("bn:dispatch-foo"));
    }

    #[test]
    fn window_meta_matches_when_the_name_says_nothing() {
        let windows = parse_window_rows("bn:3\t/wt\t\tbn\tfeature-x\t\t0\n");
        let got = resolve_channels(&[key("sid", "feature-x", None)], &windows, &[]);
        assert_eq!(got[0].tier, Some(MatchTier::WindowMeta));
        assert_eq!(got[0].tmux_target.as_deref(), Some("bn:3"));
    }

    #[test]
    fn two_sessions_on_one_branch_stay_ambiguous_only_when_name_matched() {
        let windows = windows();
        // Both agents are on one branch. Only the name tier is available, so neither is addressable.
        let named = resolve_channels(
            &[
                key("sid-1", "startkworkinbn-zsh", None),
                key("sid-2", "startkworkinbn-zsh", None),
            ],
            &windows,
            &[],
        );
        assert!(named.iter().all(|c| c.ambiguous));
        assert_eq!(
            control_channel(&named[0], RecordedSession::None, BLIND),
            ControlChannel::Ambiguous
        );

        // Same branch, but each session's checkout is known — so each keeps its own window.
        let paths = resolve_channels(
            &[
                key(
                    "sid-1",
                    "startkworkinbn-zsh",
                    Some("/Users/e/worktree/bn/_pool/6"),
                ),
                key(
                    "sid-2",
                    "startkworkinbn-zsh",
                    Some("/Users/e/worktree/bn/_pool/4"),
                ),
            ],
            &windows,
            &[],
        );
        assert!(paths.iter().all(|c| !c.ambiguous));
        assert_eq!(
            paths[0].tmux_target.as_deref(),
            Some("bn:startkworkinbn-zsh")
        );
        assert_eq!(paths[1].tmux_target.as_deref(), Some("bn:bn-skills-crud"));
        assert_eq!(
            control_channel(&paths[1], RecordedSession::None, BLIND),
            ControlChannel::Tmux
        );
    }

    #[test]
    fn two_agents_claiming_one_window_are_both_refused() {
        let windows = windows();
        let got = resolve_channels(
            &[
                key("sid-1", "one", Some("/Users/e/worktree/bn/_pool/6")),
                key("sid-2", "two", Some("/Users/e/worktree/bn/_pool/6")),
            ],
            &windows,
            &[],
        );
        assert!(got.iter().all(|c| c.ambiguous && c.tmux_target.is_none()));
        assert_eq!(
            control_channel(&got[0], RecordedSession::Same, BLIND),
            ControlChannel::Ambiguous,
            "a collided window is unaimable even with a matching resume record"
        );
    }

    #[test]
    fn a_recorded_session_for_this_agent_beats_the_window() {
        let got = resolve_channels(
            &[key(
                "sid",
                "startkworkinbn-zsh",
                Some("/Users/e/worktree/bn/_pool/6"),
            )],
            &windows(),
            &[],
        );
        assert_eq!(
            control_channel(&got[0], RecordedSession::Same, BLIND),
            ControlChannel::Resume
        );
    }

    #[test]
    fn a_lone_agent_with_a_stale_record_keeps_resume_but_a_shared_branch_does_not() {
        let unmatched = AgentChannel::default();
        assert_eq!(
            control_channel(&unmatched, RecordedSession::Other, BLIND),
            ControlChannel::Resume
        );
        let shared = AgentChannel {
            branch_shared: true,
            ..Default::default()
        };
        assert_eq!(
            control_channel(&shared, RecordedSession::Other, BLIND),
            ControlChannel::Ambiguous
        );
    }

    // --- the transcript is a resume key: no agent is "observed only" ---

    #[test]
    fn a_session_started_outside_bn_is_steerable_by_its_own_transcript() {
        // No window, no lease, no note record — the plain `claude` someone ran in a terminal, which
        // used to land on ControlChannel::None and a dead "Observed only" panel.
        let unmatched = AgentChannel::default();
        assert_eq!(
            control_channel(&unmatched, RecordedSession::None, BLIND),
            ControlChannel::None,
            "with nothing observed there is genuinely no channel"
        );
        assert_eq!(
            control_channel(&unmatched, RecordedSession::None, seen("sid-live")),
            ControlChannel::Resume,
            "but bn read this agent's transcript, so it holds a session id to resume"
        );
    }

    #[test]
    fn a_transcript_without_a_live_checkout_is_still_refused() {
        // A resume turn has to run *somewhere*; promising one with no directory would fail after a
        // full harness invocation, which is worse than saying "no channel".
        let unmatched = AgentChannel::default();
        let no_wt = ObservedSession {
            session_id: "sid-live",
            worktree: None,
        };
        assert_eq!(
            control_channel(&unmatched, RecordedSession::None, no_wt),
            ControlChannel::None
        );
        let no_sid = ObservedSession {
            session_id: "",
            worktree: Some("/Users/e/worktree/bn/_pool/6"),
        };
        assert_eq!(
            control_channel(&unmatched, RecordedSession::None, no_sid),
            ControlChannel::None
        );
    }

    #[test]
    fn two_agents_on_one_branch_each_resume_their_own_transcript() {
        // Ambiguity is a property of *branch-keyed* evidence. Two agents on one branch have two
        // transcripts, and a session id names exactly one of them.
        let shared = AgentChannel {
            branch_shared: true,
            ..Default::default()
        };
        assert_eq!(
            control_channel(&shared, RecordedSession::Other, BLIND),
            ControlChannel::Ambiguous
        );
        assert_eq!(
            control_channel(&shared, RecordedSession::Other, seen("sid-1")),
            ControlChannel::Resume
        );

        let collided = AgentChannel {
            ambiguous: true,
            ..Default::default()
        };
        assert_eq!(
            control_channel(&collided, RecordedSession::Same, BLIND),
            ControlChannel::Ambiguous
        );
        assert_eq!(
            control_channel(&collided, RecordedSession::Same, seen("sid-2")),
            ControlChannel::Resume,
            "two agents sharing one pane are still individually resumable"
        );
    }

    #[test]
    fn the_fallback_never_downgrades_a_channel_that_already_worked() {
        // Every verdict that was steerable before must stay exactly what it was — the change only
        // upgrades refusals.
        let windowed = resolve_channels(
            &[key(
                "sid",
                "startkworkinbn-zsh",
                Some("/Users/e/worktree/bn/_pool/6"),
            )],
            &windows(),
            &[],
        );
        assert_eq!(
            control_channel(&windowed[0], RecordedSession::None, seen("sid")),
            ControlChannel::Tmux,
            "a real window still steers by tmux, not by a headless resume turn"
        );
        assert_eq!(
            control_channel(&windowed[0], RecordedSession::Same, seen("sid")),
            ControlChannel::Resume
        );
    }
}
