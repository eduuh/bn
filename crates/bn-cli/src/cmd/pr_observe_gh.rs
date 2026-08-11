//! The GitHub half of `pr-observe`: the `gh search prs` sweep and the per-PR `gh pr view`
//! enrichment. Split out of `jobs.rs` — which is twice the file-size limit and may not grow — so
//! this plumbing has room to carry the fields readers need.
//!
//! **Recording is not reporting.** This sweep observes *every* repo, including the ones
//! [`bn_core::metrics::is_excluded_repo`] names. That exclusion is a reporting rule, applied at
//! read time by `bn_core::metrics::visible_events`, which every metrics surface funnels through —
//! so the scorecard stays unflattered by tooling churn while surfaces that ask "what did I
//! actually finish yesterday" (the standup fold) still see the work. Filtering at ingest instead
//! erased it from both, which is why a day spent entirely in `bn` read back as "no completed
//! tasks recorded".

/// One row from the `pr-observe` search sweep.
pub(crate) struct MyPrRow {
    /// gh `owner/repo`.
    pub(crate) repo: String,
    pub(crate) number: i64,
    /// PR title, carried through to the `pr_merged` event so readers can name what shipped.
    pub(crate) title: String,
    pub(crate) created_at: String,
    /// Search-level state is only open/closed — merged needs the per-PR view.
    pub(crate) closed: bool,
    pub(crate) url: String,
}

/// How many PRs one `pr-observe` search sweep pulls. Sized well above a busy month (the old 100
/// silently truncated: a PR that closed weeks ago got pushed out by newer ones and never got its
/// `pr_closed` event, which left the goals hanging off it looking live forever). Only the search
/// grows — the expensive per-PR `gh pr view` stays gated by [`bn_core::pr_observe::needs_enrichment`].
pub(crate) const PR_OBSERVE_SEARCH_LIMIT: usize = 500;

/// Your own PRs updated since `since` (`YYYY-MM-DD`), any state, via `gh search prs`.
pub(crate) fn gh_my_prs(since: &str) -> Vec<MyPrRow> {
    #[derive(serde::Deserialize)]
    struct SearchPr {
        repository: Repo,
        number: i64,
        #[serde(default)]
        title: String,
        #[serde(default, rename = "createdAt")]
        created_at: String,
        #[serde(default)]
        state: String,
        url: String,
    }
    #[derive(serde::Deserialize)]
    struct Repo {
        #[serde(rename = "nameWithOwner")]
        name_with_owner: String,
    }
    let updated = format!(">={since}");
    let limit = PR_OBSERVE_SEARCH_LIMIT.to_string();
    let rows = bn_core::runner::gh_json::<Vec<SearchPr>>(
        &bn_core::runner::RealRunner,
        &[
            "search",
            "prs",
            "--author=@me",
            "--updated",
            &updated,
            "--limit",
            &limit,
            "--json",
            "repository,number,title,createdAt,state,url",
        ],
    )
    .unwrap_or_default();
    // Say so rather than truncating silently — at the cap the oldest PRs in the window are missing
    // their lifecycle events, and everything downstream reads that as "still open".
    if rows.len() >= PR_OBSERVE_SEARCH_LIMIT {
        eprintln!(
            "pr-observe: hit the {PR_OBSERVE_SEARCH_LIMIT}-PR search cap — PRs updated just after {since} may be missing"
        );
    }
    rows.into_iter()
    .map(|pr| MyPrRow {
        repo: pr.repository.name_with_owner,
        number: pr.number,
        title: pr.title,
        created_at: pr.created_at,
        closed: pr.state.eq_ignore_ascii_case("closed"),
        url: pr.url,
    })
    .collect()
}

/// Fill a PR's terminal/review/lead fields from `gh pr view` (the one per-PR call). Best-effort:
/// on any gh failure the PR keeps only its search-row fields. The first review excludes the
/// author's own review-comments; the lead anchor is the earliest commit's authored date.
pub(crate) fn gh_enrich_pr(pr: &mut bn_core::pr_observe::ObservedPr, url: &str) {
    #[derive(serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct View {
        #[serde(default)]
        merged_at: Option<String>,
        #[serde(default)]
        closed_at: Option<String>,
        #[serde(default)]
        author: Option<Login>,
        #[serde(default)]
        reviews: Vec<Review>,
        #[serde(default)]
        commits: Vec<Commit>,
    }
    #[derive(serde::Deserialize, Default)]
    struct Login {
        #[serde(default)]
        login: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Review {
        #[serde(default)]
        author: Option<Login>,
        #[serde(default)]
        submitted_at: Option<String>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Commit {
        #[serde(default)]
        authored_date: Option<String>,
        #[serde(default)]
        committed_date: Option<String>,
    }
    let Some(v) = bn_core::runner::gh_json::<View>(
        &bn_core::runner::RealRunner,
        &[
            "pr",
            "view",
            url,
            "--json",
            "mergedAt,closedAt,author,reviews,commits",
        ],
    ) else {
        return;
    };
    let ts = |s: &Option<String>| s.as_deref().and_then(bn_core::util::parse_ts);
    pr.merged_ts = ts(&v.merged_at);
    pr.closed_ts = ts(&v.closed_at);
    let me = v.author.map(|a| a.login).unwrap_or_default();
    pr.first_review_ts = v
        .reviews
        .iter()
        .filter(|r| {
            r.author
                .as_ref()
                .map(|a| !a.login.eq_ignore_ascii_case(&me))
                .unwrap_or(true)
        })
        .filter_map(|r| ts(&r.submitted_at))
        .min();
    pr.first_commit_ts = v
        .commits
        .iter()
        .filter_map(|c| ts(&c.authored_date).or_else(|| ts(&c.committed_date)))
        .min();
}
