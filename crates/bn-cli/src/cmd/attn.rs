//! `bn attn` — the attention board: queue, triage, start, notify, metrics.
use crate::*;

pub(crate) fn emit_attn_list(type_filter: &[String], as_json: bool) {
    let items = filter_by_type(bn_core::attention::open_items(&today()), type_filter);
    if as_json {
        println!(
            "{}",
            Envelope::success("attn", payload::attn_list_data(&items)).to_json()
        );
        return;
    }
    if items.is_empty() {
        println!("No open attention items");
        return;
    }
    for i in &items {
        let loc = match (&i.repo, &i.branch) {
            (Some(r), Some(b)) => format!(" {r}/{b}"),
            (Some(r), None) => format!(" {r}"),
            _ => String::new(),
        };
        println!(
            "{}  [{}] {} ({}){}",
            i.id, i.severity, i.title, i.source, loc
        );
        if let Some(link) = &i.link {
            println!("  {link}");
        }
    }
}

/// Snapshot every tmux window's agent state into a map keyed by canonical `repo:branch`, for
/// joining in-flight attention items to their live window ([`bn_core::attention::classify_started`]).
/// Session = repo dir, window name = `sanitize_branch` (slashes→dashes), agent windows prefixed
/// `⛏ `; both sides key through [`bn_core::attention::window_key_parts`] so they agree. Empty
/// outside tmux / on any error.
pub(crate) fn window_states() -> std::collections::HashMap<String, bn_core::attention::WindowState>
{
    let fmt = "#{session_name}\t#{window_name}\t#{@bn_agent}\t#{@bn_agent_since}\t#{@bn_done}\t#{@bn_blockers}";
    let out = match crate::proc::tmux_raw(&["list-windows", "-a", "-F", fmt]) {
        Some(o) => o,
        None => return Default::default(),
    };
    let mut map = std::collections::HashMap::new();
    for line in out.lines() {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() < 6 {
            continue;
        }
        // Strip the agent-window glyph (`⛏ `) and any leading punctuation before the branch name.
        let branch = c[1].trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric());
        if c[0].is_empty() || branch.is_empty() {
            continue;
        }
        let key = bn_core::attention::window_key_parts(c[0], branch);
        map.insert(
            key,
            bn_core::attention::WindowState {
                agent: c[2].to_string(),
                agent_since: c[3].parse().unwrap_or(0),
                done_flag: !c[4].is_empty(),
                blockers: !c[5].is_empty(),
            },
        );
    }
    map
}

/// The in-flight items with their live status, and the open (to-start) items — the raw material
/// for the three-lane board and the `status` fragment. Reads the event log + one `list-windows`.
pub(crate) fn attn_lanes() -> (
    Vec<(
        bn_core::attention::StartedItem,
        bn_core::attention::WorkStatus,
    )>,
    Vec<bn_core::attention::AttnItem>,
) {
    let now = bn_core::util::now_epoch_secs();
    let classified = bn_core::attention::classify_started(
        bn_core::attention::started_items(),
        &window_states(),
        now,
        bn_core::agent::DEFAULT_IDLE_MINS,
        bn_core::agent::DEFAULT_STUCK_MINS,
    );
    (classified, bn_core::attention::open_items(&today()))
}

/// The status glyph + color for an in-flight item, by its live [`WorkStatus`].
pub(crate) fn status_badge(st: bn_core::attention::WorkStatus) -> (&'static str, &'static str) {
    use bn_core::attention::WorkStatus::*;
    match st {
        Done => ("32", "✓ done"),
        Stuck => ("1;31", "! stuck"),
        Waiting => ("1;33", "● waiting"),
        Blocked => ("31", "⊘ blocked"),
        Working => ("36", "◌ working"),
        Idle => ("2", "○ idle"),
        Launching => ("2", "… launching"),
        Orphaned => ("1;31", "⚠ orphaned"),
    }
}

/// The mode glyph for a to-start item — what an agent can pick (gear = auto / `◐ assist`), else none.
pub(crate) fn mode_badge(mode: &str) -> &'static str {
    match mode {
        "auto" => "\u{f013} ",
        "assist" => "◐ ",
        _ => "",
    }
}

/// A compact pipeline-stage glyph for a to-start item, reusing the [`mode_badge`] idiom. `coding`
/// (the execution default) carries no badge to keep the common line lean; the typed stages of the
/// Plan → Execute → Critique pipeline each get a short marker (#649).
pub(crate) fn type_badge(task_type: &str) -> &'static str {
    match task_type {
        "planning" => "◇ ",
        "critique" => "✎ ",
        "review" => "⊙ ",
        _ => "",
    }
}

/// Validate a `--type` filter against [`bn_core::attention::TASK_TYPES`], aborting on an unknown
/// value, then narrow `items` to those whose effective [`task_type`](bn_core::attention::AttnItem::task_type)
/// is in the set. An empty filter is a pure pass-through (no narrowing). Untyped legacy items fold
/// to `coding`, so `--type coding` includes them.
pub(crate) fn filter_by_type(
    items: Vec<bn_core::attention::AttnItem>,
    type_filter: &[String],
) -> Vec<bn_core::attention::AttnItem> {
    if type_filter.is_empty() {
        return items;
    }
    for t in type_filter {
        if !bn_core::attention::TASK_TYPES.contains(&t.as_str()) {
            fail(&format!(
                "bn attn: --type must be one of {}",
                bn_core::attention::TASK_TYPES.join(", ")
            ));
        }
    }
    items
        .into_iter()
        .filter(|i| type_filter.iter().any(|t| t == i.task_type()))
        .collect()
}

/// Reorder the TO START lane by `bn next`'s ranking (`ranked_ids` = attn ids in `bn next` order) —
/// one ranking brain, not two: the board's top open item IS the answer to "what now?". Items absent
/// from the ranking (focus-suppressed, aged out) sink below the ranked ones, keeping their
/// severity-first order among themselves.
pub(crate) fn rank_to_start(
    open: Vec<bn_core::attention::AttnItem>,
    ranked_ids: &[String],
) -> Vec<bn_core::attention::AttnItem> {
    let rank: std::collections::HashMap<&str, usize> = ranked_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    let mut keyed: Vec<(usize, usize, bn_core::attention::AttnItem)> = open
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            let r = rank.get(item.id.as_str()).copied().unwrap_or(usize::MAX);
            (r, i, item)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    keyed.into_iter().map(|(_, _, item)| item).collect()
}

/// `bn attn` (no subcommand) — the three-lane board: what needs you, what's in progress, and what's
/// left to start. Attention-items only (the cross-source needs-you queue is `bn next`/`bn jump`).
pub(crate) fn emit_attn_board(cfg: &Config, type_filter: &[String], as_json: bool) {
    use bn_core::attention::Lane;
    let (classified, open) = attn_lanes();
    let open = filter_by_type(open, type_filter);
    // One ranking brain: TO START follows `bn next`'s order (warm cache — same feed as the
    // picker), including the read-time momentum re-rank (#246).
    let ranked_ids: Vec<String> = {
        use bn_core::clock::Clock;
        let clock = bn_core::clock::RealClock;
        let today = clock.today();
        let now = clock.now_epoch_secs();
        let actions = bn_core::next::cached_or_compute(cfg, &today, now);
        bn_core::next::apply_momentum(cfg, actions, &today, now)
            .into_iter()
            .filter_map(|a| a.attn_id)
            .collect()
    };
    let open = rank_to_start(open, &ranked_ids);
    // Board view only: a secondary, stable group-by-type on top of the severity/rank ordering, so
    // typed pipeline stages (planning/critique/review) cluster while `coding` keeps leading. The
    // sort is stable, so each type group preserves its existing severity-first order (#649).
    let open = {
        let type_rank = |t: &str| {
            bn_core::attention::TASK_TYPES
                .iter()
                .position(|k| *k == t)
                .unwrap_or(usize::MAX)
        };
        let mut open = open;
        open.sort_by_key(|i| type_rank(i.task_type()));
        open
    };

    if as_json {
        let entry = |s: &bn_core::attention::StartedItem, st: bn_core::attention::WorkStatus| {
            let mut v = serde_json::to_value(s).unwrap_or_default();
            v["status"] = serde_json::json!(st.label());
            v
        };
        let lane = |want: Lane| {
            classified
                .iter()
                .filter(|(_, st)| st.lane() == want)
                .map(|(s, st)| entry(s, *st))
                .collect::<Vec<_>>()
        };
        let data = serde_json::json!({
            "needs_you": lane(Lane::NeedsYou),
            "in_progress": lane(Lane::InProgress),
            "to_start": open,
        });
        println!("{}", Envelope::success("attn", data).to_json());
        return;
    }

    let loc = |repo: &Option<String>, branch: &Option<String>| match (repo, branch) {
        (Some(r), Some(b)) => format!("  \x1b[2m{r}/{b}\x1b[0m"),
        (Some(r), None) => format!("  \x1b[2m{r}\x1b[0m"),
        _ => String::new(),
    };

    let needs: Vec<_> = classified
        .iter()
        .filter(|(_, st)| st.lane() == Lane::NeedsYou)
        .collect();
    let progress: Vec<_> = classified
        .iter()
        .filter(|(_, st)| st.lane() == Lane::InProgress)
        .collect();
    // Orphaned in-flight items fold back into the to-start lane, ahead of the open items.
    let orphaned: Vec<_> = classified
        .iter()
        .filter(|(_, st)| st.lane() == Lane::ToStart)
        .collect();

    if needs.is_empty() && progress.is_empty() && orphaned.is_empty() && open.is_empty() {
        println!("\x1b[1;32m✓ nothing on the attention list\x1b[0m");
        return;
    }

    if !needs.is_empty() {
        println!("\x1b[1mNEEDS YOU\x1b[0m");
        for (s, st) in &needs {
            let (color, tag) = status_badge(*st);
            println!(
                "  \x1b[{color}m{tag}\x1b[0m  {}{}  \x1b[2m{}\x1b[0m",
                s.item.title,
                loc(&s.item.repo, &s.item.branch),
                s.item.id
            );
            if let Some(n) = &s.notes {
                println!("      \x1b[2m{n}\x1b[0m");
            }
            if let Some(l) = &s.item.link {
                println!("      \x1b[2m{l}\x1b[0m");
            }
        }
    }
    if !progress.is_empty() {
        println!("\x1b[1mIN PROGRESS\x1b[0m");
        for (s, st) in &progress {
            let (color, tag) = status_badge(*st);
            println!(
                "  \x1b[{color}m{tag}\x1b[0m  {}{}  \x1b[2m{}\x1b[0m",
                s.item.title,
                loc(&s.item.repo, &s.item.branch),
                s.item.id
            );
        }
    }
    if !orphaned.is_empty() || !open.is_empty() {
        println!("\x1b[1mTO START\x1b[0m");
        for (s, _) in &orphaned {
            println!(
                "  \x1b[1;31m⚠ orphaned\x1b[0m  {}{}  \x1b[2m{}\x1b[0m",
                s.item.title,
                loc(&s.item.repo, &s.item.branch),
                s.item.id
            );
        }
        for i in &open {
            // A pending plan-review is a "plan to review" — call it out explicitly and point at the
            // one-keystroke nvim popup (`bn jump` / `bn doctor --pick`) rather than the generic
            // to-start line (#651).
            if crate::cmd::plan_review::is_plan_review_attn(i) {
                println!(
                    "  \x1b[1;35m◇ plan to review\x1b[0m  {}{}  \x1b[2m{}\x1b[0m",
                    i.title,
                    loc(&i.repo, &i.branch),
                    i.id
                );
                continue;
            }
            println!(
                "  {}{}\x1b[2m[{}]\x1b[0m {}{}  \x1b[2m{}\x1b[0m",
                mode_badge(i.mode()),
                type_badge(i.task_type()),
                i.severity,
                i.title,
                loc(&i.repo, &i.branch),
                i.id
            );
        }
    }
}

/// `bn attn status` — a compact one-line fragment for the tmux badge: `2 working · 1 stuck · 1
/// done · 3 to-start`. Empty when the list is clear. Counts in-flight by lane + the to-start total.
pub(crate) fn emit_attn_status() {
    use bn_core::attention::WorkStatus;
    let (classified, open) = attn_lanes();
    let count = |want: WorkStatus| classified.iter().filter(|(_, st)| *st == want).count();
    let mut parts: Vec<String> = Vec::new();
    let working =
        count(WorkStatus::Working) + count(WorkStatus::Idle) + count(WorkStatus::Launching);
    let stuck = count(WorkStatus::Stuck);
    let done = count(WorkStatus::Done);
    let waiting = count(WorkStatus::Waiting) + count(WorkStatus::Blocked);
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if waiting > 0 {
        parts.push(format!("{waiting} waiting"));
    }
    if stuck > 0 {
        parts.push(format!("\x1b[1;31m{stuck} stuck\x1b[0m"));
    }
    if done > 0 {
        parts.push(format!("\x1b[32m{done} done\x1b[0m"));
    }
    if !open.is_empty() {
        parts.push(format!("{} to-start", open.len()));
    }
    if !parts.is_empty() {
        println!("{}", parts.join(" · "));
    }
}

/// Read stdin when the value is the `-` sentinel, else pass it through unchanged. Lets
/// `--prompt -` / `--detail -`-style flags take long, multi-line input from a pipe.
pub(crate) fn read_dash_stdin(value: Option<&str>) -> Option<String> {
    match value {
        Some("-") => {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
            Some(buf.trim_end().to_string()).filter(|s| !s.is_empty())
        }
        other => other.map(str::to_string),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_attn_add(
    source: &str,
    title: &str,
    severity: &str,
    repo: Option<&str>,
    branch: Option<&str>,
    link: Option<&str>,
    key: Option<&str>,
    mode: Option<&str>,
    prompt: Option<&str>,
    as_json: bool,
) {
    if !bn_core::attention::SEVERITIES.contains(&severity) {
        fail(&format!(
            "bn attn add: --severity must be one of {}",
            bn_core::attention::SEVERITIES.join(", ")
        ));
    }
    if let Some(m) = mode {
        if !bn_core::attention::MODES.contains(&m) {
            fail(&format!(
                "bn attn add: --mode must be one of {}",
                bn_core::attention::MODES.join(", ")
            ));
        }
    }
    // `--prompt -` reads the prompt from stdin. An assist/auto item needs one — that's the whole
    // point of the label (an agent has nothing to run without it).
    let prompt = read_dash_stdin(prompt);
    if matches!(mode, Some("assist") | Some("auto")) && prompt.as_deref().unwrap_or("").is_empty() {
        fail(&format!(
            "bn attn add: --mode {} needs a --prompt (what should the agent do?)",
            mode.unwrap_or("")
        ));
    }
    // A plain `manual` item stores no mode field — keep the common event lean.
    let mode = mode.filter(|m| *m != "manual");
    let out = bn_core::attention::add_new(
        &bn_core::attention::NewAttn {
            source,
            title,
            severity,
            repo,
            branch,
            link,
            key,
            mode,
            prompt: prompt.as_deref(),
            ..Default::default()
        },
        &today(),
    )
    .unwrap_or_else(|e| fail(&format!("bn attn add: {e}")));
    if as_json {
        println!(
            "{}",
            Envelope::success("attn", payload::attn_add_data(&out)).to_json()
        );
    } else if out.skipped {
        println!("{} already open — skipped", out.id);
    } else {
        println!("Raised {}", out.id);
    }
}

pub(crate) fn emit_attn_ingest(source: &str, severity: &str, replace: bool, as_json: bool) {
    use std::io::Read;
    if !bn_core::attention::SEVERITIES.contains(&severity) {
        fail(&format!(
            "bn attn ingest: --severity must be one of {}",
            bn_core::attention::SEVERITIES.join(", ")
        ));
    }
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        fail("bn attn ingest: failed to read stdin");
    }
    let items = bn_core::attention::parse_ingest_items(&input);
    let out = bn_core::attention::ingest(&items, source, severity, replace, &today())
        .unwrap_or_else(|e| fail(&format!("bn attn ingest: {e}")));
    if as_json {
        println!(
            "{}",
            Envelope::success("attn_ingest", payload::attn_ingest_data(&out)).to_json()
        );
    } else {
        println!(
            "attention '{}': +{} raised, {} skipped, {} resolved",
            source, out.raised, out.skipped, out.resolved
        );
    }
}

/// Styled status-bar fragment for attention: ONE number — the total of everything that wants
/// you — with a bell glyph. Sums raised attention (cron failures + urgent/other items, via
/// `doctor::badge`) AND live agent windows (`waiting` needing input + `done`/ready, across all
/// sessions — shared with `bn tmux-attn-count` via [`tmux_status::attn_window_tally`]). Red when
/// anything is urgent (an urgent item, a cron failure, or a wait ignored 15+ minutes), amber
/// otherwise; nothing when all quiet, so the bar stays clean. The single attention producer in
/// status-format (the window tally used to be a separate `bn-attention-count.sh` fragment).
pub(crate) fn emit_attn_badge() {
    let b = bn_core::doctor::badge(&today());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let win = crate::proc::tmux_raw(&[
        "list-windows",
        "-a",
        "-F",
        "#{@bn_agent}\t#{@bn_done}\t#{@bn_agent_since}",
    ])
    .unwrap_or_default();
    let (waiting, ready, wait) = crate::cmd::tmux_status::attn_window_tally(&win, now);
    let total = b.urgent + b.other + waiting as usize + ready as usize;
    if total == 0 {
        return;
    }
    let color = if b.urgent > 0 || wait >= 900 {
        "red"
    } else {
        "colour214"
    };
    print!(
        "#[fg={color}]{total}{} #[fg=#3b4261]\u{b7} ",
        bn_core::glyph::BELL
    );
}

pub(crate) fn emit_attn_to_notify(as_json: bool) {
    let items = bn_core::attention::to_notify(&today());
    if as_json && json_ok("attn", payload::attn_list_data(&items)) {
        return;
    }
    for i in &items {
        println!("{}  {} ({})", i.id, i.title, i.source);
        if let Some(link) = &i.link {
            println!("  {link}");
        }
    }
}

/// Shared tail for the six attention "resolve" mutations
/// (notified/ack/snooze/update/remove/done): print the JSON envelope, else the human `success`
/// line when the mutation hit an item, else fail as not-found. `success` is the fully-rendered
/// human line; `noun` names what was missing in the not-found error (`bn attn <cmd>: <noun> <id>`).
pub(crate) fn emit_attn_resolve(
    cmd: &str,
    noun: &str,
    success: &str,
    id: &str,
    ok: bool,
    as_json: bool,
) {
    if as_json {
        println!(
            "{}",
            Envelope::success("attn", payload::attn_resolve_data(id, ok)).to_json()
        );
    } else if ok {
        println!("{success}");
    } else {
        fail(&format!("bn attn {cmd}: {noun} {id}"));
    }
}

pub(crate) fn emit_attn_notified(id: &str, as_json: bool) {
    let ok = bn_core::attention::mark_notified(id)
        .unwrap_or_else(|e| fail(&format!("bn attn notified: {e}")));
    emit_attn_resolve(
        "notified",
        "no item",
        &format!("Notified {id}"),
        id,
        ok,
        as_json,
    );
}

pub(crate) fn emit_attn_ack(id: &str, as_json: bool) {
    let ok = bn_core::attention::ack(id).unwrap_or_else(|e| fail(&format!("bn attn ack: {e}")));
    emit_attn_resolve(
        "ack",
        "no open item",
        &format!("Acked {id}"),
        id,
        ok,
        as_json,
    );
}

pub(crate) fn emit_attn_snooze(id: &str, until: &str, as_json: bool) {
    let ok = bn_core::attention::snooze(id, until)
        .unwrap_or_else(|e| fail(&format!("bn attn snooze: {e}")));
    emit_attn_resolve(
        "snooze",
        "no item",
        &format!("Snoozed {id} until {until}"),
        id,
        ok,
        as_json,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_attn_update(
    id: &str,
    title: Option<&str>,
    severity: Option<&str>,
    link: Option<&str>,
    repo: Option<&str>,
    branch: Option<&str>,
    detail: Option<&str>,
    mode: Option<&str>,
    prompt: Option<&str>,
    as_json: bool,
) {
    if title.is_none()
        && severity.is_none()
        && link.is_none()
        && repo.is_none()
        && branch.is_none()
        && detail.is_none()
        && mode.is_none()
        && prompt.is_none()
    {
        fail(
            "bn attn update: nothing to update — pass at least one of \
             --title/--severity/--link/--repo/--branch/--detail/--mode/--prompt",
        );
    }
    if let Some(s) = severity {
        if !bn_core::attention::SEVERITIES.contains(&s) {
            fail(&format!(
                "bn attn update: --severity must be one of {}",
                bn_core::attention::SEVERITIES.join(", ")
            ));
        }
    }
    if let Some(m) = mode {
        if !bn_core::attention::MODES.contains(&m) {
            fail(&format!(
                "bn attn update: --mode must be one of {}",
                bn_core::attention::MODES.join(", ")
            ));
        }
    }
    // `--prompt -` reads the (possibly long, multi-line) prompt from stdin.
    let prompt = read_dash_stdin(prompt);
    let ok = bn_core::attention::update(
        id,
        title,
        severity,
        repo,
        branch,
        link,
        detail,
        mode,
        prompt.as_deref(),
    )
    .unwrap_or_else(|e| fail(&format!("bn attn update: {e}")));
    emit_attn_resolve(
        "update",
        "no item",
        &format!("Updated {id}"),
        id,
        ok,
        as_json,
    );
}

pub(crate) fn emit_attn_remove(id: &str, as_json: bool) {
    let ok =
        bn_core::attention::remove(id).unwrap_or_else(|e| fail(&format!("bn attn remove: {e}")));
    emit_attn_resolve(
        "remove",
        "no item",
        &format!("Removed {id}"),
        id,
        ok,
        as_json,
    );
}

/// The result of [`run_attn_start`]: the opened worktree and whether an agent was launched/staged.
pub(crate) struct StartOutcome {
    pub(crate) wt_path: std::path::PathBuf,
    pub(crate) launched: bool,
}

pub(crate) fn pipeline_stage_for_task_type(task_type: &str) -> bn_core::pipeline::stage::Stage {
    use bn_core::pipeline::stage::Stage;
    match task_type {
        "planning" => Stage::Plan,
        "review" => Stage::Consensus,
        "critique" => Stage::Critique,
        _ => Stage::Execute,
    }
}

/// The agent prompt for a started item: context-first (read the branch note — that's why item
/// prompts can be one-liners), then the item's own prompt, then the ship-and-report instructions.
/// Code ships as a draft PR whose URL lands on the item (`done --link`), so the needs-you review
/// is a diff, not prose; and the report is `bn attn done`, never `ack` — the human gate.
pub(crate) fn agent_prompt(id: &str, prompt: &str) -> String {
    format!(
        "First read this branch's note for context — goal, open todos, decisions: run `bn --cat`.\n\n\
         Task: {prompt}\n\nWhen the work is complete:\n\
         - If you changed code: commit with a conventional message, push the branch, and open a \
         DRAFT pull request (`gh pr create --draft`). Then report with:\n  \
         bn attn done {id} --link <pr-url> --notes \"<one-line summary>\"\n\
         - Otherwise report with:\n  bn attn done {id} --notes \"<one-line summary>\"\n\
         Do NOT run `bn attn ack`, and do NOT merge the PR — a human reviews and closes both.",
    )
}

fn planning_agent_prompt(item: &bn_core::attention::AttnItem) -> String {
    let context = item
        .prompt
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(item.detail.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or(&item.title);
    format!(
        "You are scoping repo-less attention item {id}: {title}\n\n\
         Context: {context}\n\n\
         This is a planning-only scratch session. Investigate enough to identify the owning \
         repository and a bounded implementation scope; do not edit product code here. Once the \
         repository is known, hand the task to the real worktree pipeline with:\n\
         bn attn dispatch {id} --repo <repo>\n\n\
         The dispatch command queues the implementation and resolves this planning item. If the \
         repository cannot be determined, leave the item open and report the missing fact.",
        id = item.id,
        title = item.title,
    )
}

fn run_repoless_attn_start(
    cfg: &Config,
    item: &bn_core::attention::AttnItem,
    agent: bool,
) -> Option<StartOutcome> {
    let safe_id = wt::sanitize_branch(&item.id);
    let win = format!("attn-{safe_id}");
    let workspace = cfg.state_dir().join("attention-planning").join(safe_id);
    if let Err(e) = std::fs::create_dir_all(&workspace) {
        eprintln!(
            "bn attn start: failed to create planning workspace {}: {e}",
            workspace.display()
        );
        return None;
    }
    if !wt::open_worktree_window_cmd(
        wt::orchestrator_session(),
        &win,
        &workspace,
        false,
        None,
        agent,
        Some(wt::orchestrator_session()),
    ) {
        return None;
    }
    let _ = bn_core::attention::mark_started(&item.id);

    let launched = agent
        && wt::launch_agent_in_window(
            cfg,
            wt::orchestrator_session(),
            &win,
            &workspace,
            &planning_agent_prompt(item),
            wt::AgentLaunchOpts::new(item.mode() == "auto", item.mode() == "auto"),
        );
    Some(StartOutcome {
        wt_path: workspace,
        launched,
    })
}

/// Open the item's worktree window, mark it in-flight, and — when `agent` — launch (`auto`, which
/// submits + runs unattended with the tool bypass) or stage (`assist`, interactive) the harness with
/// its prompt. Repo-less items open a scratch window in the shared fleet/cockpit session and hand off
/// through `bn attn dispatch` once scoped. `None` if the target window couldn't be opened. Shared by
/// `bn attn start --agent` and the `attention-auto-trigger` job.
pub(crate) fn run_attn_start(
    cfg: &Config,
    item: &bn_core::attention::AttnItem,
    agent: bool,
) -> Option<StartOutcome> {
    run_attn_start_attributed(cfg, item, agent, None)
}

pub(crate) fn run_attn_start_attributed(
    cfg: &Config,
    item: &bn_core::attention::AttnItem,
    agent: bool,
    work_class: Option<bn_core::cost::WorkClass>,
) -> Option<StartOutcome> {
    if item.repo.is_none() && item.branch.is_none() {
        return run_repoless_attn_start(cfg, item, agent);
    }
    let repo = item.repo.as_deref()?;
    let branch = item.branch.as_deref()?;
    let wt_path = wt::open_item_worktree(cfg, repo, branch)?;
    let ctx = Resolved {
        repo: repo.to_string(),
        branch: branch.to_string(),
    };
    if let Err(e) = commands::ensure_note(cfg, &ctx, &today()) {
        eprintln!(
            "bn attn start: failed to create branch note for {repo}/{branch}: {}",
            e.message()
        );
        return None;
    }
    let stage = pipeline_stage_for_task_type(item.task_type());
    match bn_core::pipeline::stage::transition(&cfg.note_file(&ctx), stage) {
        Err(e) => {
            eprintln!(
                "bn attn start: failed to enter pipeline stage {stage} for {repo}/{branch}: {e}"
            );
            return None;
        }
        // The SMART gate (#998) refused. Don't open a window on work that isn't ready — that is
        // precisely the wasted worktree the gate exists to prevent.
        Ok(decision) if !decision.admits() => {
            eprintln!(
                "bn attn start: {}",
                decision.refusal_line().unwrap_or_default()
            );
            return None;
        }
        Ok(_) => {}
    }
    // Only record it in-flight once the window is actually open.
    let _ = bn_core::attention::mark_started(&item.id);

    let mut launched = false;
    if agent {
        if let Some(prompt) = item.prompt.as_deref().filter(|p| !p.is_empty()) {
            // The tmux target is the session/window `open_item_worktree` created — derive both from
            // the worktree path (session = repo dir, window = sanitized branch) so it matches even
            // for an `owner/name` repo. `auto` submits + runs unattended (yolo); `assist` stages.
            let session = wt_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or(repo);
            let win = wt_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(branch);
            let auto = item.mode() == "auto";
            let full = agent_prompt(&item.id, prompt);
            let harness = wt::review_harness_name(cfg);
            let can_pin_session = wt::harness_supports_session_id(&harness);
            let sid = work_class
                .filter(|_| can_pin_session)
                .map(|_| crate::cmd::dispatch::gen_uuid());
            launched = wt::launch_agent_in_window(
                cfg,
                session,
                win,
                &wt_path,
                &full,
                wt::AgentLaunchOpts::new(auto, auto).with_session(sid.as_deref()),
            );
            if launched {
                if let (Some(work_class), Some(sid)) = (work_class, sid.as_deref()) {
                    bn_core::cost::attribute(sid, work_class, repo, branch);
                }
            }
        }
    }
    Some(StartOutcome { wt_path, launched })
}

/// Start item `id`: open/switch to its worktree tmux window and mark it in-flight (the IN PROGRESS
/// lane). The item needs a repo/branch to have a window. With `agent`, also launch the harness
/// seeded with the item's prompt — `auto` submits it, `assist` stages it — appending the
/// `bn attn done` instruction (the human gate: the agent reports done, you ack).
pub(crate) fn emit_attn_start(cfg: &Config, id: &str, agent: bool, as_json: bool) {
    let Some(item) = bn_core::attention::get(id) else {
        fail(&format!("bn attn start: no item {id}"));
    };
    if item.repo.is_some() != item.branch.is_some() {
        fail("bn attn start: item must carry both repo and branch, or neither");
    }
    if agent && item.repo.is_some() && item.prompt.as_deref().filter(|p| !p.is_empty()).is_none() {
        eprintln!(
            "bn attn start --agent: item {id} has no prompt to run (set --mode assist|auto with --prompt)"
        );
    }
    let repo = item.repo.clone();
    let branch = item.branch.clone();
    let Some(outcome) = run_attn_start(cfg, &item, agent) else {
        match (&repo, &branch) {
            (Some(repo), Some(branch)) => fail(&format!(
                "bn attn start: couldn't open a worktree for {repo}/{branch} \
                 (bare repo or branch note missing locally?)"
            )),
            _ => fail("bn attn start: couldn't open the fleet/cockpit session"),
        }
    };
    let (wt_path, launched) = (outcome.wt_path, outcome.launched);
    let target = match (&repo, &branch) {
        (Some(repo), Some(branch)) => format!("{repo}/{branch}"),
        _ => format!("{}/attn-{id}", wt::orchestrator_session()),
    };

    if as_json {
        println!(
            "{}",
            Envelope::success(
                "attn",
                serde_json::json!({
                    "id": id,
                    "repo": repo,
                    "branch": branch,
                    "target": target,
                    "worktree": wt_path.to_string_lossy(),
                    "mode": item.mode(),
                    "agent_launched": launched,
                }),
            )
            .to_json()
        );
    } else {
        println!("Started {id} — {target}");
        println!("  worktree: {}", wt_path.display());
        if launched {
            let how = if item.mode() == "auto" {
                "launched (auto)"
            } else {
                "staged in the pane (assist — press Enter to run)"
            };
            println!("  agent: {how}");
        } else if let Some(p) = &item.prompt {
            println!("  prompt: {p}");
        }
    }
}

/// Format a duration in seconds as a compact `Ns` / `Nm` / `NhMm` string.
pub(crate) fn human_secs(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

/// `bn attn metrics` — the attention lifecycle rollup: how much the list took on and cleared, and
/// how long items sat in-flight. Read from the local `attn_*` events (not the synced PR scorecard).
pub(crate) fn emit_attn_metrics(window: i64, as_json: bool) {
    let now = bn_core::util::now_epoch_secs();
    let m = bn_core::attention::lifecycle_metrics(now, window);
    if as_json {
        println!(
            "{}",
            Envelope::success("attn", serde_json::to_value(&m).unwrap_or_default()).to_json()
        );
        return;
    }
    println!("attention · last {}d", m.window_days);
    println!(
        "  raised    {}   ({} manual · {} assist · {} auto)",
        m.raised, m.raised_manual, m.raised_assist, m.raised_auto
    );
    println!(
        "  started {} · done {} · closed {}",
        m.started, m.done, m.acked
    );
    if let (Some(avg), Some(med)) = (m.avg_inflight_secs, m.median_inflight_secs) {
        println!(
            "  time-in-flight  avg {} · median {}",
            human_secs(avg),
            human_secs(med)
        );
    }
}

/// Promote a branch todo into an attention item (the inverse of `plan`) — existing note work
/// becomes loop fuel with a seeded prompt, without retyping anything.
pub(crate) fn emit_attn_promote(
    cfg: &Config,
    todo_id: &str,
    mode: &str,
    severity: &str,
    repo: Option<String>,
    branch: Option<String>,
    as_json: bool,
) {
    use bn_core::attention::PromoteError;
    if !bn_core::attention::MODES.contains(&mode) {
        fail(&format!(
            "bn attn promote: --mode must be one of {}",
            bn_core::attention::MODES.join(", ")
        ));
    }
    if !bn_core::attention::SEVERITIES.contains(&severity) {
        fail(&format!(
            "bn attn promote: --severity must be one of {}",
            bn_core::attention::SEVERITIES.join(", ")
        ));
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let out = match bn_core::attention::promote_todo(
        cfg,
        &cwd,
        repo,
        branch,
        todo_id,
        mode,
        severity,
        &today(),
    ) {
        Ok(v) => v,
        Err(PromoteError::NoTodo) => fail(&format!("bn attn promote: no open todo {todo_id}")),
        Err(PromoteError::TodoDone) => fail(&format!(
            "bn attn promote: {todo_id} is already done — nothing to queue"
        )),
        Err(PromoteError::NoTarget(resolver::ContextError::PartialTarget)) => {
            fail("bn attn promote: repo and branch must be given together")
        }
        Err(PromoteError::NoTarget(resolver::ContextError::NotInGitRepo)) => {
            fail("bn attn promote: not in a git repo — pass --repo and --branch")
        }
        Err(PromoteError::Read(e)) => fail(&format!("bn attn promote: read note: {e}")),
        Err(PromoteError::Write(e)) => fail(&format!("bn attn promote: {e}")),
    };
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "attn",
                serde_json::json!({
                    "id": out.add.id,
                    "skipped": out.add.skipped,
                    "repo": out.ctx.repo,
                    "branch": out.ctx.branch,
                    "todo": todo_id,
                    "mode": mode,
                }),
            )
            .to_json()
        );
    } else if out.add.skipped {
        println!("{} already queued — skipped", out.add.id);
    } else {
        println!(
            "Queued {} ({mode}) — \"{}\" from {}/{} {todo_id}",
            out.add.id, out.todo_text, out.ctx.repo, out.ctx.branch
        );
    }
}

/// Show the item the autonomous loop would start next — the dry-run of `attention-auto-trigger`
/// (trust + concurrency-cap gate applied; idle gate is the cron's). Prints nothing when at the cap
/// (`BN_ATTN_AUTO_MAX`) or nothing is eligible.
pub(crate) fn emit_attn_next_auto(cfg: &Config, as_json: bool) {
    match bn_core::attention::next_auto(cfg, &today(), attn_auto_max()) {
        Some(item) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::success(
                        "attn",
                        serde_json::json!({
                            "id": item.id,
                            "title": item.title,
                            "repo": item.repo,
                            "branch": item.branch,
                        }),
                    )
                    .to_json()
                );
            } else {
                let loc = match (&item.repo, &item.branch) {
                    (Some(r), Some(b)) => format!(" — {r}/{b}"),
                    _ => String::new(),
                };
                println!("{} {}{loc}", item.id, item.title);
            }
        }
        None if as_json => println!(
            "{}",
            Envelope::success("attn", serde_json::json!(null)).to_json()
        ),
        None => {}
    }
}

/// Mark in-flight item `id` done — awaiting your review. Stays visible until acked (the human gate).
pub(crate) fn emit_attn_done(id: &str, notes: Option<&str>, link: Option<&str>, as_json: bool) {
    // `--notes -` reads a long summary from stdin.
    let notes = read_dash_stdin(notes);
    let ok = bn_core::attention::mark_done(id, notes.as_deref(), link)
        .unwrap_or_else(|e| fail(&format!("bn attn done: {e}")));
    emit_attn_resolve(
        "done",
        "no item",
        &format!("Marked {id} done — awaiting review (`bn attn ack {id}` to close)"),
        id,
        ok,
        as_json,
    );
}

/// Promote attention item `id` to a branch todo, then resolve it off the list. The todo lands on
/// the item's own repo/branch when it carries both, else the current branch.
pub(crate) fn emit_attn_plan(cfg: &Config, id: &str, section: &str, as_json: bool) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let out = match bn_core::attention::plan_item(cfg, &cwd, id, section, &today()) {
        Ok(v) => v,
        Err(bn_core::attention::PlanError::NoItem) => fail(&format!("bn attn plan: no item {id}")),
        Err(bn_core::attention::PlanError::NoTarget(resolver::ContextError::PartialTarget)) => {
            fail("bn attn plan: repo and branch must be given together")
        }
        Err(bn_core::attention::PlanError::NoTarget(resolver::ContextError::NotInGitRepo)) => {
            fail("bn attn plan: not in a git repo — item carries no repo/branch to target")
        }
        Err(bn_core::attention::PlanError::Add(e)) => {
            emit_err("attn plan", &e, as_json);
            return;
        }
        Err(bn_core::attention::PlanError::Resolve(e)) => fail(&format!("bn attn plan: {e}")),
    };
    let todo = out.todo;
    let ctx = out.ctx;
    if as_json {
        println!(
            "{}",
            Envelope::success(
                "attn",
                payload::attn_plan_data(
                    id,
                    payload::PlannedTodo {
                        id: &todo.id,
                        section: todo.section.label(),
                        rb: ctx.rb(),
                    },
                ),
            )
            .to_json()
        );
    } else {
        println!(
            "Planned {id} → {} [{}] on {}/{}: {}",
            todo.section.label(),
            todo.id,
            ctx.repo,
            ctx.branch,
            todo.text
        );
    }
}

/// `bn attn dispatch <id>` (#460) — promote attention item `id` into the orchestrate backlog as a
/// task, then resolve it off the list. The pure [`bn_core::attention::promote_to_backlog`] policy
/// picks the lane: start-new (a fresh pending task), reprioritize (bump an already-queued twin,
/// never duplicate), or interrupt (an urgent item onto the #378 priority lane). Targets the item's
/// own repo when it carries one, else `--repo`, else the current worktree's.
pub(crate) fn emit_attn_dispatch(
    cfg: &Config,
    id: &str,
    repo_override: Option<&str>,
    as_json: bool,
) {
    use bn_core::attention::{self, PromotionAction};

    let Some(item) = attention::get(id) else {
        fail(&format!("bn attn dispatch: no item {id}"));
    };
    // Repo: the item's own, else --repo, else the current worktree's.
    let repo = match item
        .repo
        .clone()
        .or_else(|| repo_override.map(String::from))
    {
        Some(r) => r,
        None => {
            let cwd = std::env::current_dir().unwrap_or_default();
            match resolver::resolve(cfg, &cwd) {
                Ok(ctx) => ctx.repo,
                Err(_) => fail(
                    "bn attn dispatch: item carries no repo and not in a git repo — pass --repo",
                ),
            }
        }
    };

    let now = bn_core::util::now_epoch_secs();
    let path = bn_core::backlog::backlog_path();
    let bl = bn_core::backlog::load(&path);
    let out = attention::promote_to_backlog(&item, &bl, &repo, now);

    // Apply the decision to the durable backlog: bump an existing twin, or append a new entry.
    let persisted = match out.action {
        PromotionAction::Reprioritize => {
            bn_core::backlog::reprioritize(&path, &repo, &out.backlog_id, out.entry.priority, now)
                .map(|_| ())
        }
        PromotionAction::StartNew | PromotionAction::Interrupt => {
            let spec = bn_core::backlog::AddSpec {
                kind: out.entry.kind,
                priority: out.entry.priority,
                touches: out.entry.touches.clone(),
                substrate: out.entry.substrate,
                source: out.entry.source.clone(),
                controller_id: out.entry.controller_id.clone(),
                task: out.entry.task.clone(),
            };
            bn_core::backlog::add(&path, &repo, std::slice::from_ref(&out.subject), &spec, now)
                .map(|_| ())
        }
    };
    if let Err(e) = persisted {
        fail(&format!("bn attn dispatch: could not update backlog: {e}"));
    }
    // Fold the item off the attention list, recording the backlog trail.
    let _ = attention::mark_dispatched(id, &repo, &out.backlog_id, out.action.label());

    if as_json {
        let data = serde_json::json!({
            "id": id,
            "action": out.action.label(),
            "repo": repo,
            "backlog_id": out.backlog_id,
            "subject": out.subject,
            "priority": out.entry.priority,
        });
        println!("{}", Envelope::success("attn", data).to_json());
    } else {
        let verb = match out.action {
            PromotionAction::StartNew => "queued new task",
            PromotionAction::Reprioritize => "reprioritized queued task",
            PromotionAction::Interrupt => "queued on the priority lane",
        };
        println!(
            "Dispatched {id} → {repo} [{}] ({verb}, priority {}): {}",
            out.backlog_id, out.entry.priority, out.subject
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, task_type: Option<&str>) -> bn_core::attention::AttnItem {
        bn_core::attention::AttnItem {
            id: id.into(),
            source: "manual".into(),
            title: id.into(),
            severity: "warn".into(),
            repo: None,
            branch: None,
            link: None,
            key: None,
            detail: None,
            mode: None,
            prompt: None,
            task_type: task_type.map(str::to_string),
            worktree_path: None,
            uncommitted_files: Vec::new(),
            created: 0,
        }
    }

    #[test]
    fn empty_type_filter_is_passthrough() {
        let items = vec![item("a1", Some("planning")), item("a2", None)];
        let out = filter_by_type(items, &[]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn type_filter_narrows_to_requested_types() {
        let items = vec![
            item("a1", Some("planning")),
            item("a2", Some("critique")),
            item("a3", None),
        ];
        let out = filter_by_type(items, &["planning".to_string()]);
        assert_eq!(
            out.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["a1"]
        );
    }

    #[test]
    fn untyped_items_match_coding_filter() {
        // A legacy untyped item folds to `coding`, so `--type coding` surfaces it.
        let items = vec![item("a1", None), item("a2", Some("planning"))];
        let out = filter_by_type(items, &["coding".to_string()]);
        assert_eq!(
            out.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["a1"]
        );
    }

    #[test]
    fn type_filter_accepts_multiple_types() {
        let items = vec![
            item("a1", Some("planning")),
            item("a2", Some("critique")),
            item("a3", Some("review")),
        ];
        let out = filter_by_type(items, &["planning".to_string(), "review".to_string()]);
        assert_eq!(
            out.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["a1", "a3"]
        );
    }

    #[test]
    fn task_types_enter_their_pipeline_stage() {
        use bn_core::pipeline::stage::Stage;
        assert_eq!(pipeline_stage_for_task_type("coding"), Stage::Execute);
        assert_eq!(pipeline_stage_for_task_type("planning"), Stage::Plan);
        assert_eq!(pipeline_stage_for_task_type("review"), Stage::Consensus);
        assert_eq!(pipeline_stage_for_task_type("critique"), Stage::Critique);
        assert_eq!(pipeline_stage_for_task_type("legacy"), Stage::Execute);
    }

    #[test]
    fn repoless_planning_prompt_hands_off_through_dispatch() {
        let mut item = item("a7", Some("planning"));
        item.title = "Find the owning repo".into();
        item.detail = Some("Trace the failing workflow first".into());
        let prompt = planning_agent_prompt(&item);
        assert!(prompt.contains("planning-only scratch session"));
        assert!(prompt.contains("bn attn dispatch a7 --repo <repo>"));
        assert!(prompt.contains("do not edit product code here"));
        assert!(prompt.contains("Trace the failing workflow first"));
    }
}
