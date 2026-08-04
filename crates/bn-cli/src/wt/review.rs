//! Auto-review worktree harness (backing `bn review-pr`): sparse review worktrees,
//! agent launch, prompt plumbing, and teardown. Moved verbatim from the former
//! wt.rs god file (issue #351).

use super::*;

// --- auto-review entry points (used by the `bn review-pr` command) ----------

/// Outcome of preparing a review worktree.
pub(crate) struct PrSetup {
    pub branch: String,
    pub worktree: PathBuf,
    pub remote_label: String,
    pub favourited: bool,
}

/// Resolve a PR's source branch via the host CLI for the `<repo>.git` bare repo: `az repos pr
/// show` (ADO) or `gh pr view` (GitHub). `None` if it can't be determined.
/// bn keys bare repos, worktrees, and tmux windows by a repo's **short name** (the bare is
/// `<short>.git` under the layout's bare dir), while the review queue / ledger / CLI carry the full
/// `owner/repo` slug — which `gh --repo` and the review-cache lookups need. Reduce a slug to that
/// short key for the filesystem / bare / tmux layer; an already-short name passes through unchanged.
/// Kept separate so the full slug still flows to ownership lookups, `gh`, and the review prompt.
fn repo_key(repo: &str) -> &str {
    repo.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(repo)
}

pub(crate) fn resolve_pr_branch(cfg: &Config, repo: &str, number: i64) -> Option<String> {
    let bare = bare_for(cfg, repo_key(repo))?;
    match remote_for(&bare) {
        Remote::Ado { org, .. } => {
            let out = Command::new("az")
                .args([
                    "repos",
                    "pr",
                    "show",
                    "--id",
                    &number.to_string(),
                    "--organization",
                    &format!("https://dev.azure.com/{org}"),
                    "--query",
                    "sourceRefName",
                    "--output",
                    "tsv",
                ])
                .output()
                .ok()?;
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (out.status.success() && !s.is_empty())
                .then(|| s.trim_start_matches("refs/heads/").to_string())
        }
        Remote::GitHub { slug } => crate::gh::gh_capture(&[
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            &slug,
            "--json",
            "headRefName",
            "--jq",
            ".headRefName",
        ]),
        Remote::Other => None,
    }
}

/// Prepare the review worktree for `branch` off `<repo>.git` (the respective repo the PR belongs
/// to). The order matters and encodes a hard-won finding: Office-scale ADO repos use *limited ref
/// advertisement*, so an unfavourited branch is invisible to `git fetch` ("couldn't find remote
/// ref"). Hence: **favourite → fetch → worktree**. No tmux window — the orchestrator owns the
/// agent session. `None` if the bare repo is missing or any step fails.
pub(crate) fn setup_review_worktree(
    cfg: &Config,
    repo: &str,
    number: i64,
    branch: &str,
    favourite: bool,
    launch_agent: bool,
) -> Option<PrSetup> {
    let key = repo_key(repo);
    let bare = bare_for(cfg, key)?;
    let bare_s = bare.to_string_lossy().to_string();
    let remote = remote_for(&bare);

    // 1) Favourite the branch FIRST (ADO only) so the server advertises the ref to us. GitHub /
    //    other hosts advertise everything, so there's nothing to favourite there.
    let favourited = if favourite && matches!(remote, Remote::Ado { .. }) {
        let ok = favourite_branch(&remote, branch);
        if !ok {
            eprintln!(
                "{YELLOW}review: could not favourite {branch} — fetch may fail (unadvertised ref in a limited-ref repo){NC}"
            );
        }
        ok
    } else {
        false
    };

    // 2) Fetch the branch into the bare repo (credential.useHttpPath is required for dev.azure.com).
    if !fetch_branch(&bare_s, branch) {
        eprintln!(
            "{RED}review: failed to fetch {branch} from {}{NC}",
            remote.label()
        );
        return None;
    }

    // 3) Create (or refresh) the worktree via the shared path — `pull = false` because we just
    //    fetched the exact tip and re-pulling the Office monorepo is needlessly slow.
    let sanitized = sanitize_branch(branch);
    let (_, worktree_dir) = layout(cfg);
    let wt_path = worktree_dir.join(key).join(&sanitized);

    // Edge case: the PR branch is already checked out in *another* worktree (a bn pool slot, or
    // you're working on your own PR there). git refuses a second worktree for the same branch, so
    // rather than failing we review it **in place** — a vertical pane whose cwd is that worktree.
    if let Some(existing) = worktree_for_branch(&bare_s, branch) {
        if existing != wt_path {
            let launched = launch_agent && launch_review_pane(cfg, repo, branch, number, &existing);
            if launch_agent && !launched {
                eprintln!(
                    "{YELLOW}review: {branch} is already checked out at {} — open a vertical pane there and run the review (no tmux window hosting it to split into){NC}",
                    existing.display()
                );
            }
            return Some(PrSetup {
                branch: branch.to_string(),
                worktree: existing,
                remote_label: remote.label(),
                favourited,
            });
        }
    }

    let _ = std::fs::create_dir_all(worktree_dir.join(key));
    let wt_s = wt_path.to_string_lossy().to_string();
    if wt_path.is_dir() {
        // Re-review of an existing worktree: fast-forward to the freshly fetched tip, then
        // re-expand the sparse cone in case new commits touched new directories.
        let _ = crate::git::git_ok(&[
            "-C",
            &wt_s,
            "merge",
            "--ff-only",
            &format!("origin/{branch}"),
        ]);
        set_review_sparse(&bare_s, branch, &wt_s);
    } else if !create_sparse_review_worktree(&bare_s, branch, &wt_s) {
        return None;
    }

    // 4) Open the worktree's tmux window (same as `wt add`), so the review runs in a real session
    //    that `bn agent supervise` can watch and the user can attach to.
    open_worktree_window(key, &sanitized, &wt_path, false);

    // 5) Trigger the review agent inside that window (copilot folder-trust is handled by the
    //    shared launcher).
    if launch_agent {
        launch_review_agent(cfg, repo, &sanitized, number);
    }

    Some(PrSetup {
        branch: branch.to_string(),
        worktree: wt_path,
        remote_label: remote.label(),
        favourited,
    })
}

/// The path of an existing worktree that already has `branch` checked out, if any — parsed from
/// `git worktree list --porcelain` on the bare. git allows a branch in only one worktree, so this
/// is exactly what makes a second `worktree add` fail; when it's set, the review runs in that
/// worktree via a pane instead. Detached / bare entries (no `branch` line) are skipped.
fn worktree_for_branch(bare_s: &str, branch: &str) -> Option<PathBuf> {
    let listing = crate::git::git_capture(
        &["--git-dir", bare_s, "worktree", "list", "--porcelain"],
        None,
        true,
    )?;
    worktree_path_in_listing(&listing, branch)
}

/// Parse `git worktree list --porcelain` output for the worktree holding `branch`. Pure so the
/// branch-already-checked-out detection is unit-testable without a real repo. Detached / bare
/// entries (no `branch` line) are skipped.
fn worktree_path_in_listing(listing: &str, branch: &str) -> Option<PathBuf> {
    let want = format!("refs/heads/{branch}");
    let mut path: Option<String> = None;
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.trim().to_string());
        } else if line.strip_prefix("branch ").map(str::trim) == Some(want.as_str()) {
            if let Some(p) = path.take() {
                return Some(PathBuf::from(p));
            }
        } else if line.trim().is_empty() {
            path = None;
        }
    }
    None
}

/// Edge-case launcher for [`setup_review_worktree`]: when the PR branch is already checked out in
/// another worktree, review it **in place** — split a vertical pane in that worktree's *existing*
/// tmux window (so it sits beside the branch's session, not wherever `bn` was invoked) and run the
/// review pipeline (the rendered `auto-review-pr` seed) in it. `-h` splits left/right, i.e. a
/// vertical divider; cwd is the worktree. Returns false when no tmux window hosts the worktree (the
/// caller then just reports the path). Autonomy is ownership-gated (see below). The full slug flows
/// to the seed for ownership; the pane's prompt file is keyed by the short name.
fn launch_review_pane(cfg: &Config, repo: &str, branch: &str, number: i64, wt_path: &Path) -> bool {
    // The review pane must land in the SAME tmux window that already hosts this worktree. `None` →
    // nothing hosts it (or no tmux server), so the caller reports the path for a manual review.
    let Some(target) = tmux_windows_for(wt_path, branch).into_iter().next() else {
        return false;
    };
    let harness = review_harness_name(cfg);
    if harness == "copilot" {
        ensure_copilot_trusts(wt_path);
    }
    // Autonomy is gated on ownership (the trusted signal): your *own* PR runs yolo — the
    // fix-and-push review needs no per-command approval — while another author's PR stays
    // interactive so a human approves each tool, since that PR's content is untrusted.
    let yolo = review_autonomy(review_pr_ownership(repo, number).0);
    let prompt = review_seed_prompt(cfg, repo, number, wt_path);
    let sanitized = sanitize_branch(branch);
    let cmd = match prompt_to_file("review-pane", repo_key(repo), &sanitized, &prompt) {
        Some(f) => harness_cmd_from_file(&harness, &f, yolo, None),
        None => harness_cmd(&harness, &prompt, yolo, None),
    };
    let wt = wt_path.to_string_lossy();
    tmux(&[
        "split-window",
        "-h",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &target,
        "-c",
        wt.as_ref(),
        cmd.as_str(),
    ])
    .is_some()
}

/// A compact "check against these references" clause for the review prompt (#623): the coding
/// standards by name, with a hint to run `bn ref <name>` for each one's full text — so a review
/// checks the diff against your standards. Empty string when no references exist (nothing to add).
fn review_references_clause() -> String {
    let names = bn_core::references::names();
    if names.is_empty() {
        return String::new();
    }
    format!(
        "Also check the diff against my coding-standards references — run `bn ref <name>` for the \
         full text of each and flag any violations: {}. ",
        names.join(", ")
    )
}

/// Decide whether PR `<repo>#<number>` is *your own* (`mine`) and recover its url, from the cached
/// review queue. Ownership is computed **here, in the trusted launcher** — never inferred from PR
/// content — so the strictly-read-only guarantee for other authors' PRs can't be flipped by a
/// prompt-injected file. The decision itself is the pure [`is_mine`]; unknown author or unset login
/// ⇒ `false` (treat as someone else's PR, the safe default).
fn review_pr_ownership(repo: &str, number: i64) -> (bool, String) {
    let cache = bn_core::reviews::read();
    let pr = cache.prs.iter().find(|p| p.repo == repo && p.number == number);
    let url = pr.map(|p| p.url.clone()).unwrap_or_default();
    let me = bn_core::reviews::my_login();
    (is_mine(pr.map(|p| p.author.as_str()), me.as_deref()), url)
}

/// The pure ownership rule: a PR is *yours* only when its author is known, non-empty, and matches
/// your login case-insensitively. Any gap — unknown author, empty author, or no configured login —
/// resolves to `false`, the safe default that keeps a review strictly read-only.
fn is_mine(author: Option<&str>, me: Option<&str>) -> bool {
    match (author, me) {
        (Some(a), Some(m)) if !a.is_empty() => a.eq_ignore_ascii_case(m),
        _ => false,
    }
}

/// The shipped `auto-review-pr` pipeline skill: resolved first from the review worktree (a repo-local
/// override wins), then from the global shipped/personal skill dirs — the PR's own repo usually
/// doesn't carry bn's prompt, so the global anchor is the reliable source. `None` when it can't be
/// found; the caller then falls back to [`inline_review_prompt`], so a review still works even before
/// the prompt set is installed.
fn load_review_skill(cfg: &Config, wt_path: &std::path::Path) -> Option<bn_core::skills::Skill> {
    bn_core::skills::load_skill(cfg, wt_path, "auto-review-pr").or_else(|| {
        bn_core::skills::load_skill_set(&bn_core::skills::global_skill_dirs())
            .into_iter()
            .find(|s| s.name == "auto-review-pr")
    })
}

/// Build the review agent's seed: the rendered `auto-review-pr` workflow (the single source of truth
/// for the review process) wrapped with the worktree operating context and the MCP→CLI tool map, or
/// the inline fallback when the skill isn't installed. Ownership (`mine`) is baked into the render
/// args + the stage-tracker flag, so the agent inherits the fix-vs-read-only decision from the
/// trusted launcher rather than deciding it from PR content.
fn review_seed_prompt(cfg: &Config, repo: &str, number: i64, wt_path: &std::path::Path) -> String {
    let (mine, url) = review_pr_ownership(repo, number);
    let Some(skill) = load_review_skill(cfg, wt_path) else {
        return inline_review_prompt(repo, number);
    };
    let stage_flag = if mine { " --mine" } else { "" };
    let mut args = serde_json::Map::new();
    args.insert("repo".into(), serde_json::Value::String(repo.to_string()));
    args.insert(
        "number".into(),
        serde_json::Value::String(number.to_string()),
    );
    args.insert("url".into(), serde_json::Value::String(url));
    args.insert(
        "mine".into(),
        serde_json::Value::String(if mine { "yes" } else { "no" }.to_string()),
    );
    args.insert(
        "stage_flag".into(),
        serde_json::Value::String(stage_flag.to_string()),
    );
    let body = skill.render(Some(&args));
    format!(
        "You're in a fresh, sparse review worktree for a pull request you were dispatched to review. \
         Follow bn's `auto-review-pr` workflow below — it is your process for this task. The workflow \
         references bn's MCP tools; without them use the CLI: `bn_cat` -> `bn --cat`, `bn_add` -> \
         `bn add <section> <text>`.\n\n--- WORKFLOW: auto-review-pr ---\n{body}\n--- END WORKFLOW ---\n"
    )
}

/// The inline review prompt used when the `auto-review-pr` skill can't be loaded (e.g. before the
/// prompt set is installed on a box). Kept as a self-contained fallback so a review still works; the
/// skill is the source of truth when present. Read-only framing, since the fallback can't know
/// ownership — the safe default is to never write to the PR.
fn inline_review_prompt(repo: &str, number: i64) -> String {
    let refs_clause = review_references_clause();
    format!(
        "Review PR {number} ({repo}), checked out in this worktree. \
         This is a SPARSE checkout - only the PR changed paths are materialized; read any other \
         file from the object store with: git show HEAD:<path> (or git show origin/main:<path>). \
         This is a STRICTLY READ-ONLY review of a pull request. NEVER edit, stage, \
         commit, or push files, and NEVER comment on, approve, or vote on the PR. \
         Diff against main: git diff origin/main...HEAD. \
         Fan out the pr-review-toolkit subagents in parallel over the changed files \
         (code-reviewer, silent-failure-hunter, type-design-analyzer, pr-test-analyzer); \
         if they are unavailable, review inline. \
         {refs_clause}\
         Synthesize one clear recommendation, approve or reject, with concrete reasons. \
         Record the verdict with: bn review-pr record {repo} {number} VERDICT --sha SHA --notes NOTES, \
         where VERDICT is approve or reject, SHA is the output of git rev-parse HEAD, and NOTES is a \
         substantive summary of your findings and the reason. This one command persists the full \
         judgement to the branch note folder (review.md / review2.md ...) AND the attention queue - \
         use --notes - to pipe a long multi-line write-up. It is the ONLY write you should make; \
         NEVER write to the PR or its branch. \
         Present the recommendation to me here."
    )
}

/// The autonomy boundary for a review. A review agent runs **yolo** (tools auto-approved, no
/// per-command prompt) *only* when the PR is `mine` — my own, trusted code. Another author's PR
/// (`mine == false`) is untrusted content and a prompt-injection risk, so it stays interactive and
/// a human approves each tool. `mine` is computed by the trusted launcher via [`review_pr_ownership`]
/// (never inferred from PR content), so a malicious PR can't flip itself autonomous. Ownership also
/// shapes *what* the agent does — fix-and-push on your own PR vs. a read-only comment on someone
/// else's (see [`review_seed_prompt`]). Single source of truth for both [`launch_review_agent`] and
/// [`launch_review_pane`].
fn review_autonomy(mine: bool) -> bool {
    mine
}

/// Launch the PR-review agent in the worktree's tmux window, seeded with the `auto-review-pr`
/// workflow so the user can watch and drive it. The seed is built by [`review_seed_prompt`] (skill
/// render, or the inline fallback), and the mechanical delivery is the shared
/// [`launch_agent_in_window`].
pub(crate) fn launch_review_agent(cfg: &Config, repo: &str, win_branch: &str, number: i64) {
    let wt_path = review_worktree_path(cfg, repo, win_branch);
    let yolo = review_autonomy(review_pr_ownership(repo, number).0);
    let prompt = review_seed_prompt(cfg, repo, number, &wt_path);
    // Autonomy is gated on ownership (the trusted signal): your *own* PR runs yolo — the agent
    // fixes + pushes without babysitting — while another author's PR stays interactive (yolo=false)
    // so a human approves each tool, since that PR's content is untrusted. See `review_autonomy`.
    if launch_agent_in_window(
        cfg,
        repo_key(repo),
        win_branch,
        &wt_path,
        &prompt,
        AgentLaunchOpts::new(true, yolo),
    ) {
        println!("{GREEN}review agent launched in {repo}:{win_branch}{NC}");
    } else {
        eprintln!(
            "{YELLOW}review: could not send to tmux window {repo}:{win_branch} (tmux absent?){NC}"
        );
    }
}

/// Launch (or stage) an agent in the worktree tmux window for `repo`/`win_branch`, seeded with
/// `prompt`. Delivered as one tmux paste-buffer block rather than `send-keys -l` (char-by-char): a
/// long prompt typed key-by-key into an interactive shell with syntax-highlight / autosuggest
/// plugins gets mangled and never submits; a paste lands it intact. `submit` sends a trailing Enter
/// to run it (auto / review); when false the command is left staged in the pane for you to review
/// and run yourself (assist). Pre-trusts the worktree for copilot so a fresh worktree doesn't block
/// on the first-run folder-trust prompt. Returns whether the paste reached tmux. The harness is
/// `$BN_REVIEW_HARNESS`, else whatever drives bn, else copilot — a full interactive session (tools
/// are approved per-call, not blanket-bypassed), keeping a human able to intervene.
pub(crate) fn launch_agent_in_window(
    cfg: &Config,
    repo: &str,
    win_branch: &str,
    wt_path: &Path,
    prompt: &str,
    opts: AgentLaunchOpts<'_>,
) -> bool {
    let harness = review_harness_name(cfg);
    if harness == "copilot" {
        ensure_copilot_trusts(wt_path);
    }
    // File-based prompt: a multi-KB prompt pasted inline can get cut mid-string by the receiving
    // TUI/shell, leaving an unbalanced quote and a stuck pane. Park the prompt in a file and paste
    // a short `"$(cat <file>)"` command instead; inline only as a fallback if the write fails.
    let cmd = match prompt_to_file("launch", repo, win_branch, prompt) {
        Some(f) => harness_cmd_from_file(&harness, &f, opts.yolo, opts.session_id),
        None => harness_cmd(&harness, prompt, opts.yolo, opts.session_id),
    };
    let target = format!("={repo}:{win_branch}");
    let buf = format!("bn-agent-{repo}-{win_branch}");
    let tmux = |args: &[&str]| tmux_run(args);
    let pasted = tmux(&["set-buffer", "-b", &buf, &cmd])
        && tmux(&["paste-buffer", "-d", "-b", &buf, "-t", &target]);
    if pasted && opts.submit {
        let _ = tmux(&["send-keys", "-t", &target, "Enter"]);
    }
    pasted
}

#[derive(Clone, Copy)]
pub(crate) struct AgentLaunchOpts<'a> {
    submit: bool,
    yolo: bool,
    session_id: Option<&'a str>,
}

impl<'a> AgentLaunchOpts<'a> {
    pub(crate) fn new(submit: bool, yolo: bool) -> Self {
        Self {
            submit,
            yolo,
            session_id: None,
        }
    }

    pub(crate) fn with_session(mut self, session_id: Option<&'a str>) -> Self {
        self.session_id = session_id;
        self
    }
}

/// The review harness to launch: config (`[review].harness` / `$BN_REVIEW_HARNESS`, via
/// [`Config::review_harness`]), else whatever is driving bn, else copilot.
pub(crate) fn review_harness_name(cfg: &Config) -> String {
    cfg.review_harness
        .clone()
        .or_else(bn_core::util::detect_harness)
        .unwrap_or_else(|| "copilot".to_string())
}

pub(crate) fn harness_supports_session_id(harness: &str) -> bool {
    harness != "copilot"
}

/// Best-effort: add `worktree` to Copilot's trusted-folders so a fresh review worktree doesn't
/// block on the "Do you trust this folder?" prompt. `~/.copilot/config.json` is JSONC (a
/// `//`-comment header) that Copilot manages; we read it tolerantly, add the path if absent, and
/// write it back (preserving the header). Any failure is ignored — Copilot just prompts as before.
pub(crate) fn ensure_copilot_trusts(worktree: &Path) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let cfg_path = PathBuf::from(home).join(".copilot").join("config.json");
    add_trusted_folder(&cfg_path, &worktree.to_string_lossy());
}

/// Add `path_str` to the `trustedFolders` array of a Copilot-style JSONC config at `cfg_path`,
/// preserving its `//` header. No-op on a missing/unparseable file, an unexpected shape, a
/// non-absolute `path_str` (a folder trust must be a real absolute path — never a bare token like a
/// harness name), or a path already present. Split out from [`ensure_copilot_trusts`] so it's
/// testable without `$HOME`.
pub(crate) fn add_trusted_folder(cfg_path: &Path, path_str: &str) {
    // A trusted *folder* is always an absolute path. Refuse anything else so a bare token (e.g. a
    // harness name mistaken for a folder) can never be persisted into copilot's `trustedFolders`.
    if !Path::new(path_str).is_absolute() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(cfg_path) else {
        return;
    };
    // Split off the leading `//` comment header so we can put it back verbatim.
    let header: Vec<&str> = text
        .lines()
        .take_while(|l| l.trim_start().starts_with("//"))
        .collect();
    let body: String = text
        .lines()
        .skip(header.len())
        .collect::<Vec<_>>()
        .join("\n");
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&body) else {
        return;
    };
    let Some(arr) = json
        .get_mut("trustedFolders")
        .and_then(|v| v.as_array_mut())
    else {
        return; // unexpected shape — leave Copilot's file untouched
    };
    if arr.iter().any(|v| v.as_str() == Some(path_str)) {
        return; // already trusted
    }
    arr.push(serde_json::Value::String(path_str.to_string()));
    if let Ok(pretty) = serde_json::to_string_pretty(&json) {
        let mut out = header.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&pretty);
        out.push('\n');
        let _ = std::fs::write(cfg_path, out);
    }
}

/// The session command for a resolved harness. Anything other than `copilot` is treated as claude
/// (the safe default). `yolo` grants the blanket tool bypass for *unattended* runs — only ever set
/// for a self-authored, trusted task (an `auto` attention item; R4 restricts those to
/// `source=manual`), never for a review of untrusted PR content, where prompt-injected files could
/// otherwise run arbitrary commands. Without `yolo` the harness prompts per tool (human approves).
pub(crate) fn harness_cmd(
    harness: &str,
    prompt: &str,
    yolo: bool,
    session_id: Option<&str>,
) -> String {
    // The prompt can contain apostrophes ("someone else's"), so single-quote it safely — an
    // unescaped `'` would close the quote early and mangle the command.
    let p = shell_single_quote(prompt);
    match harness {
        // Copilot's interactive-with-initial-prompt flag is `-i` (a bare positional is rejected:
        // "Invalid command format"). It consumes the *next token* as the prompt, so `-i` must come
        // last with the prompt right after it — `copilot -i --allow-all-tools ...` makes copilot
        // read `--allow-all-tools` as the prompt and die. Booleans first, `-i <prompt>` last. The
        // blanket bypass is gated behind `yolo`: safe for a self-authored auto task, unsafe for an
        // untrusted review.
        "copilot" if yolo => format!("copilot {} -i {p}", copilot_yolo_flags()),
        "copilot" => format!("copilot -i {p}"),
        // `--strict-mcp-config` ignores the worktree's `.mcp.json`, so a fresh worktree doesn't
        // block on the first-run "N new MCP servers found" picker. The yolo bypass lets an
        // unattended auto run proceed without a human to approve each tool call.
        _ if yolo => format!(
            "claude --strict-mcp-config {}{} {p}",
            claude_yolo_flags(),
            session_id_fragment(session_id)
        ),
        _ => format!(
            "claude --strict-mcp-config{} {p}",
            session_id_fragment(session_id)
        ),
    }
}

/// Like [`harness_cmd`], but the prompt lives in `file` and the command reads it with
/// `"$(cat '<file>')"` — so the command that gets typed/pasted into a pane stays short no matter
/// how long the prompt is. A ~2 KB inline prompt has been observed truncating mid-string in
/// transit (unbalanced quote, stuck shell); a file-based launch can't lose the tail.
pub(crate) fn harness_cmd_from_file(
    harness: &str,
    file: &Path,
    yolo: bool,
    session_id: Option<&str>,
) -> String {
    let p = format!("\"$(cat {})\"", shell_single_quote(&file.to_string_lossy()));
    match harness {
        // Same flag-order rule as harness_cmd: booleans first, `-i <prompt>` last.
        "copilot" if yolo => format!("copilot {} -i {p}", copilot_yolo_flags()),
        "copilot" => format!("copilot -i {p}"),
        _ if yolo => format!(
            "claude --strict-mcp-config {}{} {p}",
            claude_yolo_flags(),
            session_id_fragment(session_id)
        ),
        _ => format!(
            "claude --strict-mcp-config{} {p}",
            session_id_fragment(session_id)
        ),
    }
}

/// The space-joined Copilot yolo bypass flags for a shell command line (single source of truth in
/// [`bn_core::mcp::Harness::yolo_args`]).
fn copilot_yolo_flags() -> String {
    bn_core::mcp::Harness::Copilot.yolo_args().join(" ")
}

/// The space-joined Claude yolo bypass flags for a shell command line.
fn claude_yolo_flags() -> String {
    bn_core::mcp::Harness::Claude.yolo_args().join(" ")
}

fn session_id_fragment(session_id: Option<&str>) -> String {
    session_id
        .filter(|s| !s.is_empty())
        .map(|s| format!(" --session-id {}", shell_single_quote(s)))
        .unwrap_or_default()
}

/// Persist a launch prompt to a stable per-window file so launches can read it via `--seed-file`
/// instead of embedding it inline. Tries `$TMPDIR` then `/tmp` (a slow/removable notes mount can
/// fail the write, #646); `None` only if every candidate fails (caller then refuses to inline a
/// long seed rather than garble the command). Overwritten on every launch.
///
/// The temp dir can be world-writable (`/tmp` on Linux) and the name is predictable, so the write
/// must not follow a pre-planted symlink or leave the prompt readable/swappable by other users —
/// this file can feed a `--dangerously-skip-permissions` launch. Unlink the previous launch's
/// file, then `O_EXCL`-create fresh with owner-only mode: a symlink (or any file) parked at the
/// path makes `create_new` fail instead of following it (sticky-bit `/tmp` stops others unlinking
/// ours), and the caller falls back inline.
pub(crate) fn prompt_to_file(
    kind: &str,
    repo: &str,
    branch: &str,
    prompt: &str,
) -> Option<PathBuf> {
    let clean = |s: &str| {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
    };
    let name = format!("bn-{kind}-prompt-{}-{}.md", clean(repo), clean(branch));
    // Prefer $TMPDIR; on a slow/removable mount (WSL notes on a Windows share) that write can fail
    // or race (#646), so fall back to `/tmp` — a reliable local dir the launcher can always read.
    let mut dirs = vec![std::env::temp_dir()];
    let slash_tmp = PathBuf::from("/tmp");
    if !dirs.contains(&slash_tmp) {
        dirs.push(slash_tmp);
    }
    prompt_file_in_dirs(&dirs, &name, prompt)
}

/// Write `prompt` to `<dir>/<name>` for the first `dir` whose owner-only O_EXCL write succeeds;
/// `None` if every candidate fails. Split out from [`prompt_to_file`] so the fallback order is
/// unit-testable without mutating the global `TMPDIR`.
pub(crate) fn prompt_file_in_dirs(dirs: &[PathBuf], name: &str, prompt: &str) -> Option<PathBuf> {
    dirs.iter()
        .find_map(|dir| write_prompt_file(&dir.join(name), prompt))
}

/// O_EXCL-create `path` owner-only and write `prompt`; `None` on any failure. Unlinks a prior file
/// first so `create_new` won't follow a pre-planted symlink parked at the predictable path.
fn write_prompt_file(path: &std::path::Path, prompt: &str) -> Option<PathBuf> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).ok()?;
    f.write_all(prompt.as_bytes()).ok()?;
    Some(path.to_path_buf())
}

/// Wrap `s` in single quotes for a POSIX shell, escaping embedded single quotes as `'\''`.
pub(crate) fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Tear a review down: kill its tmux window and remove the worktree + the local branch the
/// review created. Best-effort throughout (a missing window/worktree is fine). Returns true if
/// the bare repo resolved (so the worktree ops were attempted).
pub(crate) fn teardown_review_worktree(cfg: &Config, repo: &str, branch: &str) -> bool {
    let repo = repo_key(repo);
    let sanitized = sanitize_branch(branch);
    // Kill the tmux window (best-effort — `=repo:branch` exact-matches the window).
    let _ = tmux_ok(&["kill-window", "-t", &format!("={repo}:{sanitized}")]);

    let Some(bare) = bare_for(cfg, repo) else {
        return false;
    };
    let bare_s = bare.to_string_lossy().to_string();
    let wt_path = review_worktree_path(cfg, repo, branch);
    let _ = run_git(&[
        "--git-dir",
        &bare_s,
        "worktree",
        "remove",
        "--force",
        &wt_path.to_string_lossy(),
    ]);
    let _ = run_git(&["--git-dir", &bare_s, "branch", "-D", branch]);
    true
}

/// Create a review worktree that materializes only the PR's changed paths (cone-mode sparse
/// checkout) rather than all 404k files of a monorepo — a review needs the diff, and anything
/// else is still readable from the object store via `git show`. Falls back to a full checkout if
/// the diff is empty or sparse-checkout is unavailable, so the tree is never left empty. Returns
/// whether the worktree ended up created.
pub(crate) fn create_sparse_review_worktree(bare_s: &str, branch: &str, wt_s: &str) -> bool {
    let start = format!("origin/{branch}");
    if !run_git(&[
        "--git-dir",
        bare_s,
        "worktree",
        "add",
        "--no-checkout",
        "-B",
        branch,
        wt_s,
        &start,
    ]) {
        return false;
    }
    set_review_sparse(bare_s, branch, wt_s)
}

/// Point a review worktree's sparse cone at the PR's changed directories (vs `origin/main`) and
/// materialize them. Used on first checkout and again on re-review (new commits may touch new
/// dirs). Full-checkout fallback when there's no diff or sparse-checkout fails.
pub(crate) fn set_review_sparse(bare_s: &str, branch: &str, wt_s: &str) -> bool {
    let diff = git_out(&[
        "--git-dir",
        bare_s,
        "diff",
        "--name-only",
        &format!("origin/main...origin/{branch}"),
    ])
    .unwrap_or_default();
    let mut dirs: Vec<String> = diff
        .lines()
        .filter_map(|f| std::path::Path::new(f).parent())
        .map(|p| p.to_string_lossy().to_string())
        .filter(|d| !d.is_empty())
        .collect();
    dirs.sort();
    dirs.dedup();

    // No measurable diff (or against an absent main) → full checkout, so the agent has a tree.
    if dirs.is_empty() || !run_git(&["-C", wt_s, "sparse-checkout", "init", "--cone"]) {
        return run_git(&["-C", wt_s, "checkout"]);
    }
    let mut args: Vec<&str> = vec!["-C", wt_s, "sparse-checkout", "set"];
    args.extend(dirs.iter().map(String::as_str));
    if !run_git(&args) {
        let _ = run_git(&["-C", wt_s, "sparse-checkout", "disable"]);
        return run_git(&["-C", wt_s, "checkout"]);
    }
    // The worktree was created `--no-checkout`, so materialize the cone now.
    run_git(&["-C", wt_s, "checkout"])
}

/// Fetch a single branch into the bare repo's remote-tracking refs. `-c credential.useHttpPath`
/// is passed inline (not written to config) because GCM needs the URL path to derive the ADO org
/// — without it, dev.azure.com fetches fail with "Cannot determine the organization name".
pub(crate) fn fetch_branch(bare_s: &str, branch: &str) -> bool {
    let refspec = format!("refs/heads/{branch}:refs/remotes/origin/{branch}");
    run_git(&[
        "--git-dir",
        bare_s,
        "-c",
        "credential.useHttpPath=true",
        "fetch",
        "origin",
        &refspec,
    ])
}

/// The worktree path a review for `<repo>`'s `branch` lives at under the active layout.
pub(crate) fn review_worktree_path(cfg: &Config, repo: &str, branch: &str) -> PathBuf {
    let (_, worktree_dir) = layout(cfg);
    worktree_dir.join(repo_key(repo)).join(sanitize_branch(branch))
}

/// PR lifecycle state for `<repo>`'s `branch`: MERGED (completed) / OPEN (active) / CLOSED
/// (abandoned) / NONE. The poll job's reclaim gate.
pub(crate) fn review_pr_state(cfg: &Config, repo: &str, branch: &str) -> String {
    let repo = repo_key(repo);
    match bare_for(cfg, repo) {
        Some(bare) => pr_state(&remote_for(&bare), branch),
        None => "NONE".to_string(),
    }
}

/// Re-fetch an active reviewed branch and return its current `origin/<branch>` tip sha, so the
/// poll job can spot commits pushed since the last review. `None` if the bare repo / fetch / tip
/// can't be resolved.
pub(crate) fn refresh_review_branch(cfg: &Config, repo: &str, branch: &str) -> Option<String> {
    let bare = bare_for(cfg, repo_key(repo))?;
    let bare_s = bare.to_string_lossy().to_string();
    if !fetch_branch(&bare_s, branch) {
        return None;
    }
    git_out(&[
        "--git-dir",
        &bare_s,
        "rev-parse",
        &format!("refs/remotes/origin/{branch}"),
    ])
}

