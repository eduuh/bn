//! The verbs a worktree surface can offer — the command half of a picker whose rows are otherwise
//! all places.
//!
//! `bn tat`'s sub-picker lists a repo's whole surface: live windows, idle worktrees, open PRs,
//! codespaces, warm pool slots. Every one of those is a *place*, and Enter goes there. The verbs
//! that act on that surface existed too, but only as chords advertised inside one long header
//! string — `ctrl-w` warmed the pool, `ctrl-d` deleted a worktree — so the commands were present
//! and unfindable at the same time. Everything else meant leaving the picker and remembering a
//! subcommand.
//!
//! This is the roster those rows render from. It is deliberately *pure*: no I/O, no git, no fzf,
//! no `clap`. An action here declares what it is called, what it acts on, and which native job
//! realizes it — nothing about whether it is worth offering right now. That question needs
//! [`crate::pool::PoolStatus`] and a look at the filesystem, so it belongs to the caller, which
//! already has both. Keeping the split here is what lets a second renderer (the web board's repo
//! page) list the same verbs without inheriting the picker's notion of availability.
//!
//! Modelled on [`crate::launchable`], for the same reason: one addressable catalog beats two
//! surfaces each keeping their own copy of the vocabulary and drifting apart.

/// What an action acts on.
///
/// The load-bearing distinction, and the reason this is an enum rather than a `bool`: a
/// [`Scope::Repo`] action is always offerable and needs no target beyond the project, so it can be
/// a row in the list. Every other scope acts on *the row the cursor is on*, which a row cannot do —
/// once the cursor moves onto the command, the thing it was going to act on is no longer focused.
/// Those belong in a menu opened from the row, not beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// The whole project. Always available; rendered as a row.
    Repo,
    /// One worktree or the window/leased slot standing in for it.
    Worktree,
    /// One open pull request that has no worktree here yet.
    Pr,
    /// One warm pool slot.
    PoolSlot,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Worktree => "worktree",
            Self::Pr => "pr",
            Self::PoolSlot => "pool-slot",
        }
    }
}

/// One verb, flattened across every surface that can offer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct WtAction {
    /// Stable identifier. Rides in the picker's hidden payload column and in a URL, so it is the
    /// one field that must never change under a rename.
    pub id: &'static str,
    /// What to call it in a list.
    pub label: &'static str,
    /// What it acts on.
    pub scope: Scope,
    /// The native job that realizes it — the `job` of a [`crate::backlog::ExecSpec::Job`].
    ///
    pub job: &'static str,
    /// Whether the job wants the repo and branch of the focused row passed through.
    ///
    /// False for a repo-scope verb, which needs only the repo. True for a row-scope verb, where
    /// the branch *is* the target — and mandatory for anything touching a pool slot, since a slot
    /// is a detached checkout and `git branch --show-current` is empty there.
    ///
    /// One repo-scope exception is built at runtime rather than listed here: a repo's own job
    /// script (the picker's `script:<name>` rows) pins `main`, because `bn run --repo` refuses a
    /// repo without a branch and the script tiers resolve through the pair.
    pub needs_branch: bool,
}

/// Every action, in the order a list should show them.
///
/// Repo verbs lead with the two people reach for most, then the reclaiming ones. Row verbs follow,
/// grouped by scope. A flat table rather than a per-scope one so `id` uniqueness is checkable in a
/// single test.
const ACTIONS: &[WtAction] = &[
    // --- repo scope: rendered as rows in the picker's command section ---
    WtAction {
        id: "build-main",
        label: "Build latest main",
        scope: Scope::Repo,
        job: "build-main",
        needs_branch: false,
    },
    WtAction {
        id: "pool-warm",
        label: "Warm the pool",
        scope: Scope::Repo,
        job: "pool-warm",
        needs_branch: false,
    },
    WtAction {
        id: "pool-grow",
        label: "Cut another warm slot",
        scope: Scope::Repo,
        job: "pool-grow",
        needs_branch: false,
    },
    WtAction {
        id: "wt-cleanup",
        label: "Clean up merged worktrees",
        scope: Scope::Repo,
        job: "wt-cleanup",
        needs_branch: false,
    },
    WtAction {
        id: "fetch-prs",
        label: "Fetch + refresh PR list",
        scope: Scope::Repo,
        job: "fetch-prs",
        needs_branch: false,
    },
    WtAction {
        id: "pull-prs",
        label: "Pull my open PRs as worktrees",
        scope: Scope::Repo,
        job: "pull-prs",
        needs_branch: false,
    },
    WtAction {
        id: "wt-prune",
        label: "Prune stale worktrees + dead slots",
        scope: Scope::Repo,
        job: "wt-prune",
        needs_branch: false,
    },
    // --- worktree scope: reached from a row's own menu, never as a row of its own ---
    WtAction {
        id: "wt-verify",
        label: "Build / verify this worktree",
        scope: Scope::Worktree,
        job: "verify",
        needs_branch: true,
    },
];

/// Every action, in display order.
pub fn all() -> &'static [WtAction] {
    ACTIONS
}

/// The actions valid for one scope — what a renderer lists.
pub fn for_scope(scope: Scope) -> impl Iterator<Item = &'static WtAction> {
    ACTIONS.iter().filter(move |a| a.scope == scope)
}

/// Look one up by its stable id. `None` for an id no longer in the roster, which is what a picker
/// callback must handle: fzf re-invokes `bn` with whatever payload the row carried, and that row
/// may have been rendered by a previous build of the binary.
pub fn find(id: &str) -> Option<&'static WtAction> {
    ACTIONS.iter().find(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for a in all() {
            assert!(seen.insert(a.id), "duplicate action id: {}", a.id);
        }
    }

    #[test]
    fn find_round_trips_every_action() {
        for a in all() {
            assert_eq!(find(a.id), Some(a));
        }
        assert_eq!(find("no-such-action"), None);
    }

    /// A payload travels through fzf's `|`-delimited row and a `~`-joined `--pick-arg`, so an id
    /// containing either separator would split into the wrong fields rather than fail loudly.
    #[test]
    fn ids_avoid_the_picker_separators() {
        for a in all() {
            assert!(
                !a.id.contains(['|', '~', '\t', ' ']),
                "id {} contains a separator",
                a.id
            );
        }
    }

    #[test]
    fn for_scope_selects_only_that_scope() {
        let repo: Vec<_> = for_scope(Scope::Repo).collect();
        assert!(!repo.is_empty());
        assert!(repo.iter().all(|a| a.scope == Scope::Repo));
        assert_eq!(
            repo.len(),
            all().iter().filter(|a| a.scope == Scope::Repo).count()
        );
    }

    /// A repo verb has no row to take a branch from, so asking for one would silently target
    /// whatever the resolver happened to find in the caller's cwd.
    #[test]
    fn repo_scope_never_needs_a_branch() {
        for a in for_scope(Scope::Repo) {
            assert!(!a.needs_branch, "{} is repo-scope but wants a branch", a.id);
        }
    }
}
