//! `bn agent run` — launch a catalog agent or workflow skill by name, seeded with an instruction.
//!
//! bn could already list what it supports (`bn agents list`) and could already launch a harness
//! with a prompt (`bn agent launch --seed`), but nothing joined the two: picking an agent by name
//! and starting it meant rendering the body yourself, or reaching for one of the bespoke launchers
//! (`bn control start --agent`, `bn today agent start`) that each re-implemented the same
//! render → write seed → launch dance. This is that dance, once.
//!
//! The name resolves through [`bn_core::launchable`], so agents and skills share one namespace —
//! from here a "persona to launch" is a Markdown body plus declared arguments, and which catalog
//! it came from only changes how it renders.
//!
//! What this deliberately does *not* own: process spawning, session pinning, or the one-agent-
//! per-worktree placement guard. Those belong to [`crate::cmd::agent::emit_agent_launch`], which
//! this calls and which `exec`s — so a successful run never returns.

use crate::{fail, Config, Envelope, Resolved};
use bn_core::launchable::{self, Launchable, Source};
use serde_json::{Map, Value};
use std::io::Write;
use std::process::{Command, Stdio};

/// The hidden callback stages the fzf picker re-invokes (`bn agent run --pick-stage <s>`).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum RunPickStage {
    /// Print the row list (the initial feed and every `reload`).
    List,
    /// Render the detail panel for the focused row.
    Preview,
}

/// fzf field separator. Field 1 is the coloured display; field 2 is the name to launch.
const TAB: char = '\t';

/// How many near-miss suggestions an unknown name is worth.
const SUGGESTIONS: usize = 6;

/// `bn skills --pick` / `bn agents --pick` — the same picker, narrowed to one catalog.
///
/// Both land here rather than growing their own so there is exactly one row format, one preview,
/// and one no-fzf fallback. Selecting a row launches it, which is what makes the picker a way to
/// *start* work rather than a prettier `list`.
pub(crate) fn emit_pick(cfg: &Config, ctx: Option<&Resolved>, only: Source, json: bool) {
    emit_run(
        cfg,
        ctx,
        None,
        Vec::new(),
        None,
        false,
        None,
        None,
        false,
        Vec::new(),
        Vec::new(),
        false,
        false,
        Some(only),
        json,
    )
}

/// What to call the filtered set in a header or an empty-list line.
fn noun(only: Option<Source>) -> &'static str {
    only.map(Source::plural).unwrap_or("agents and skills")
}

/// One picker row: what fzf shows, plus the hidden column its bindings reference.
struct Row {
    display: String,
    name: String,
}

impl Row {
    /// `display \t name` — the exact line fed to fzf and re-emitted by the `list` stage, so a
    /// reload reproduces the same columns.
    fn line(&self) -> String {
        format!("{}{TAB}{}", self.display, self.name)
    }
}

fn rows(entries: &[Launchable], only: Option<Source>) -> Vec<Row> {
    entries
        .iter()
        // A shadowed skill is unreachable by name — the agent wins it — so offering it in the
        // picker would hand back a row that launches something else.
        .filter(|e| !e.shadowed)
        .filter(|e| only.is_none_or(|want| e.source == want))
        .map(|e| {
            let tag = match e.source {
                Source::Agent => "\x1b[35magent\x1b[0m",
                Source::Skill => "\x1b[36mskill\x1b[0m",
            };
            let args = if e.needs_args() {
                format!(" \x1b[33m[needs {}]\x1b[0m", e.required_args().join(", "))
            } else {
                String::new()
            };
            let desc = e.description.as_deref().unwrap_or("");
            Row {
                display: format!("{tag}  \x1b[1m{:<30}\x1b[0m {desc}{args}", e.name),
                name: e.name.clone(),
            }
        })
        .collect()
}

/// `bn agent run` with no name — the interactive picker over everything launchable.
///
/// Mirrors the established shape (`bn jump`, `bn doctor --pick`): the same binary renders the rows
/// and the preview, fzf's bindings just re-invoke it with a hidden stage, and a machine without
/// fzf degrades to the plain list rather than failing.
fn pick(
    cfg: &Config,
    ctx: Option<&Resolved>,
    base: &std::path::Path,
    only: Option<Source>,
) -> Option<String> {
    let entries = launchable::discover(cfg, ctx, base);
    let rows = rows(&entries, only);
    if rows.is_empty() {
        println!("No launchable {} found.", noun(only));
        return None;
    }
    // The reload and preview callbacks must reproduce this exact row set, so the filter travels
    // with them — a `ctrl-r` that widened the list would renumber every hidden column under the
    // cursor.
    let filter = only
        .map(|s| format!(" --pick-source {}", s.label()))
        .unwrap_or_default();

    let exe = std::env::current_exe().unwrap_or_else(|_| "bn".into());
    let bn = crate::wt::shq(&exe.display().to_string());
    let header = format!(
        "{} {} · ↵ run · esc cancel",
        rows.len(),
        noun(only)
    );

    let child = Command::new("fzf")
        .arg("--ansi")
        .arg("--reverse")
        .arg("--delimiter=\t")
        .arg("--with-nth=1")
        .arg(format!(
            "--preview={bn} agent run --pick-stage preview{filter} {{2}}"
        ))
        // Adaptive: a side-by-side preview needs room, and bn panes are routinely half a screen.
        // Under 100 columns fzf drops the preview below the list instead of squeezing both into
        // unreadable ribbons — measured at 27 columns, where the fixed layout wrapped every row
        // to two characters.
        .arg("--preview-window=right,55%,wrap,<100(down,60%)")
        .arg(format!("--header={header}"))
        .arg(format!(
            "--bind=ctrl-r:reload({bn} agent run --pick-stage list{filter})"
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(_) => {
            // No fzf on PATH — degrade to the plain list, then hold so a tmux popup stays
            // readable instead of closing on the error.
            for row in &rows {
                println!("{}", row.display);
            }
            crate::picker::hold();
            return None;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let body = rows.iter().map(Row::line).collect::<Vec<_>>().join("\n");
        let _ = stdin.write_all(body.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    let chosen = String::from_utf8_lossy(&out.stdout);
    // Field 2 is the name; an empty selection means the user pressed Esc.
    chosen
        .lines()
        .next()
        .and_then(|l| l.split(TAB).nth(1))
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Serve one hidden picker stage.
pub(crate) fn emit_pick_stage(
    cfg: &Config,
    ctx: Option<&Resolved>,
    base: &std::path::Path,
    stage: RunPickStage,
    name: Option<&str>,
    only: Option<Source>,
) {
    let entries = launchable::discover(cfg, ctx, base);
    match stage {
        RunPickStage::List => {
            for row in rows(&entries, only) {
                println!("{}", row.line());
            }
        }
        RunPickStage::Preview => {
            let Some(entry) = name.and_then(|n| launchable::find(&entries, n)) else {
                return;
            };
            println!("\x1b[1m{}\x1b[0m  ({})", entry.name, entry.source.label());
            if let Some(desc) = &entry.description {
                println!("{desc}");
            }
            println!();
            println!("scope     {}", entry.scope);
            println!("schedule  {}", entry.schedule_label());
            if let Some(kind) = entry.kind {
                println!("kind      {}", kind.label());
            }
            if let Some(harness) = &entry.harness {
                println!("harness   {harness}");
            }
            if let Some(repo) = &entry.requires_repo {
                println!("repo      {repo}");
            }
            if !entry.skills.is_empty() {
                println!("skills    {}", entry.skills.join(", "));
            }
            if !entry.arguments.is_empty() {
                println!();
                println!("\x1b[1mArguments\x1b[0m");
                for arg in &entry.arguments {
                    let req = if arg.required { " (required)" } else { "" };
                    let desc = arg.description.as_deref().unwrap_or("");
                    println!("  --arg {}=…{req}  {desc}", arg.name);
                }
            }
            println!();
            println!("\x1b[1mBody\x1b[0m");
            match std::fs::read_to_string(&entry.path) {
                Ok(text) => println!("{text}"),
                Err(e) => println!("({}: {e})", entry.path.display()),
            }
        }
    }
}

/// Print the launchable catalog — the scriptable half of the picker.
fn emit_list(
    cfg: &Config,
    ctx: Option<&Resolved>,
    base: &std::path::Path,
    only: Option<Source>,
    json: bool,
) {
    let entries: Vec<Launchable> = launchable::discover(cfg, ctx, base)
        .into_iter()
        .filter(|e| only.is_none_or(|want| e.source == want))
        // A shadowed skill is not launchable — `find` resolves its name to the agent that took it
        // — so it belongs in neither listing. Filtering here rather than at the print loop keeps
        // the JSON `count` equal to the rows a human sees.
        .filter(|e| !e.shadowed)
        .collect();
    if json {
        println!(
            "{}",
            Envelope::success(
                "agent run",
                serde_json::json!({ "count": entries.len(), "launchable": entries }),
            )
            .to_json()
        );
        return;
    }
    if entries.is_empty() {
        println!("No launchable {} found.", noun(only));
        return;
    }
    println!("{:<30} {:<7} {:<11} DESCRIPTION", "NAME", "SOURCE", "KIND");
    for e in &entries {
        let kind = e.kind.map(|k| k.label()).unwrap_or("-");
        let desc = e.description.as_deref().unwrap_or("");
        println!(
            "{:<30} {:<7} {:<11} {desc}",
            e.name,
            e.source.label(),
            kind
        );
    }
}

/// Parse repeatable `--arg key=value` pairs into the strict template argument map.
///
/// Refuses a pair with no `=` rather than silently dropping it: a mistyped argument that vanishes
/// would surface much later as a missing-required-value error naming a different key.
fn parse_args(pairs: &[String]) -> Result<Option<Map<String, Value>>, String> {
    if pairs.is_empty() {
        return Ok(None);
    }
    let mut map = Map::new();
    for pair in pairs {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(format!("--arg must be KEY=VALUE, got '{pair}'"));
        };
        if key.trim().is_empty() {
            return Err(format!("--arg has an empty key: '{pair}'"));
        }
        map.insert(key.trim().to_string(), Value::String(value.to_string()));
    }
    Ok(Some(map))
}

/// Render the entry's Markdown body with `args`, strictly.
///
/// Strict is the point. `today-agent` and `controller` declare required arguments, and a body
/// launched with `{{date}}` still in it is worse than a refusal: the agent reads a literal
/// placeholder as if it were content. `render_checked` already returns the naming error — this
/// only surfaces it.
fn render_body(
    cfg: &Config,
    ctx: Option<&Resolved>,
    base: &std::path::Path,
    entry: &Launchable,
    args: Option<&Map<String, Value>>,
) -> Result<String, String> {
    match entry.source {
        Source::Agent => {
            let catalog = bn_core::agents::discover(cfg, ctx)
                .map_err(|e| format!("agent catalog: {e}"))?;
            let def = catalog
                .require(&entry.name)
                .map_err(|e| format!("{e}"))?;
            def.render(args)
                .map_err(|e| format!("render agent '{}': {e}", entry.name))
        }
        Source::Skill => {
            let skill = bn_core::skills::load_skill(cfg, base, &entry.name)
                .map_err(|e| format!("skill catalog: {e}"))?
                .ok_or_else(|| format!("skill '{}' not found", entry.name))?;
            skill
                .render_checked(args)
                .map_err(|e| format!("render skill '{}': {e}", entry.name))
        }
    }
}

/// Compose the seed the harness starts on: the rendered body, what the entry is equipped with,
/// and the caller's instruction.
///
/// The equipped block is [`bn_core::staff::equip_preamble`] — the same text a headless staff run
/// gets, rather than a second phrasing of the same list. An entry with no declared skills or MCPs
/// contributes nothing, so this is a no-op for most skills.
pub(crate) fn compose_seed(body: &str, entry: &Launchable, instruction: &str) -> String {
    let mut seed = String::from(body);
    if !seed.ends_with('\n') {
        seed.push('\n');
    }
    if let Some(equipped) = bn_core::staff::equip_preamble(&entry.skills, &[]) {
        seed.push('\n');
        seed.push_str(&equipped);
    }
    if !instruction.trim().is_empty() {
        seed.push_str("\n## Your task\n\n");
        seed.push_str(instruction.trim());
        seed.push('\n');
    }
    seed
}

/// Warn about declared skills this machine cannot actually serve.
///
/// A warning, never a refusal: the agent's body is still worth running, and the person launching
/// it is better served by knowing which skill is missing than by being stopped. Reuses the labels
/// [`bn_core::worker::advertise`] already publishes, so "installed" means the same thing here as
/// it does to queue placement.
fn warn_missing_skills(cfg: &Config, entry: &Launchable) {
    if entry.skills.is_empty() {
        return;
    }
    let advertised = bn_core::worker::advertise::advertised_labels(cfg, &std::env::current_dir().unwrap_or_default());
    let missing: Vec<&str> = entry
        .skills
        .iter()
        .filter(|s| {
            !advertised
                .iter()
                .any(|l| l.eq_ignore_ascii_case(&format!("{}{s}", bn_core::capability::SKILL_PREFIX)))
        })
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "bn agent run: {} declares skill(s) this machine has not installed: {} \
             (launching anyway; `bn skills install` may fix it)",
            entry.name,
            missing.join(", ")
        );
    }
}

/// Refuse a launch this machine or configuration cannot honour.
///
/// Deliberately **not** [`crate::cmd::cron::agent_gate::refuse`]: that also calls
/// `ensure_runnable()`, which refuses `kind: specialist` because the *cron* `bn run <name>` path
/// cannot execute one. A specialist is precisely what an interactive `bn agent run
/// <repo>-pull-request-reviewer` should launch, so the runnable check does not belong here — only
/// the repo-availability and opt-in gates do.
fn refuse_reason(cfg: &Config, ctx: Option<&Resolved>, entry: &Launchable) -> Option<String> {
    if entry.source != Source::Agent {
        return None;
    }
    let catalog = bn_core::agents::discover(cfg, ctx).ok()?;
    let def = catalog.find(&entry.name)?;
    if !def.repo_available_here(cfg) {
        return Some(format!(
            "{} requires the '{}' repository, which is not on this machine",
            def.name,
            def.requires_repo.as_deref().unwrap_or("?")
        ));
    }
    let enabled = bn_core::resolver::JobConfig::load().agents_enabled;
    def.off_reason(&enabled)
}

/// `bn agent run [name] [instruction…]`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_run(
    cfg: &Config,
    ctx: Option<&Resolved>,
    name: Option<String>,
    instruction: Vec<String>,
    harness: Option<String>,
    worktree: bool,
    repo: Option<String>,
    base_branch: Option<String>,
    yolo: bool,
    arg_pairs: Vec<String>,
    add_dirs: Vec<String>,
    list: bool,
    print_seed: bool,
    only: Option<Source>,
    json: bool,
) {
    let cwd = std::env::current_dir().unwrap_or_default();

    if list {
        emit_list(cfg, ctx, &cwd, only, json);
        return;
    }

    // No name given: pick one. A cancelled picker is a normal exit, not a failure.
    let name = match name {
        Some(n) => n,
        None => match pick(cfg, ctx, &cwd, only) {
            Some(n) => n,
            None => return,
        },
    };

    let entries = launchable::discover(cfg, ctx, &cwd);
    let Some(entry) = launchable::find(&entries, &name).cloned() else {
        let near = launchable::near_misses(&entries, &name, SUGGESTIONS);
        let mut msg = format!("bn agent run: no agent or skill named '{name}'");
        if !near.is_empty() {
            let names: Vec<&str> = near.iter().map(|r| r.name.as_str()).collect();
            msg.push_str(&format!("\n  did you mean: {}", names.join(", ")));
        }
        msg.push_str("\n  `bn agent run --list` shows everything launchable");
        fail(&msg);
    };

    // A name held by both catalogs resolves to the agent. Say so — the skill of the same name is
    // still there and reachable by editing one of them, and silence here reads as a bug.
    if launchable::shadowed_skill(&entries, &name).is_some() {
        eprintln!(
            "bn agent run: '{name}' resolves to the agent; a skill of the same name is shadowed"
        );
    }

    if let Some(reason) = refuse_reason(cfg, ctx, &entry) {
        fail(&format!("bn agent run: {reason}"));
    }

    let args = match parse_args(&arg_pairs) {
        Ok(a) => a,
        Err(e) => fail(&format!("bn agent run: {e}")),
    };
    let body = match render_body(cfg, ctx, &cwd, &entry, args.as_ref()) {
        Ok(b) => b,
        Err(e) => {
            let mut msg = format!("bn agent run: {e}");
            if entry.needs_args() {
                msg.push_str(&format!(
                    "\n  {} requires: {}",
                    entry.name,
                    entry
                        .required_args()
                        .iter()
                        .map(|a| format!("--arg {a}=…"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            fail(&msg);
        }
    };

    warn_missing_skills(cfg, &entry);
    let instruction = instruction.join(" ");
    let seed = compose_seed(&body, &entry, &instruction);

    // Inspect and stop. Deliberately before every launch decision below, so what this prints is
    // the same string a real run would hand the harness — not a reconstruction of it.
    if print_seed {
        if json {
            println!(
                "{}",
                Envelope::success(
                    "agent run",
                    serde_json::json!({ "name": entry.name, "source": entry.source, "seed": seed }),
                )
                .to_json()
            );
        } else {
            print!("{seed}");
        }
        return;
    }

    // Harness precedence: an explicit flag, then what the manifest asks for, then the launch
    // stage's own default (which `emit_agent_launch` resolves from `None`).
    let harness = harness.or_else(|| entry.harness.clone());

    if worktree {
        return run_in_worktree(
            cfg,
            ctx,
            &entry,
            &instruction,
            &seed,
            harness.as_deref(),
            repo.as_deref(),
            base_branch.as_deref(),
            yolo,
            json,
        );
    }

    // Write the seed to a file rather than passing it inline: bodies are routinely well past the
    // 512-byte inline ceiling, and the file write is owner-only and symlink-safe.
    let (repo_label, branch_label) = ctx
        .map(|c| (c.repo.clone(), c.branch.clone()))
        .unwrap_or_else(|| ("bn".to_string(), "run".to_string()));
    let seed_file = crate::wt::prompt_to_file("run", &repo_label, &branch_label, &seed);

    match seed_file {
        Some(path) => crate::cmd::agent::emit_agent_launch(
            cfg,
            harness,
            None,
            Some(path.display().to_string()),
            yolo,
            true,
            None,
            None,
            add_dirs,
            Vec::new(),
        ),
        // Every temp dir refused the write. Pass the seed inline instead of launching an agent
        // with no instructions at all — `emit_agent_launch` takes it as a String, so unlike the
        // shell-command lanes there is no quoting ceiling to respect here.
        None => crate::cmd::agent::emit_agent_launch(
            cfg,
            harness,
            Some(seed),
            None,
            yolo,
            true,
            None,
            None,
            add_dirs,
            Vec::new(),
        ),
    }
}

/// How a `--worktree` run reaches `bn dispatch`: `(skill, seed_override)`.
///
/// Exactly one is ever `Some`. That is the invariant worth pinning: passing both would let the
/// skill envelope wrap an agent body that already carries its own completion protocol, and
/// passing neither would dispatch the default `ship-solo` skill under the chosen entry's name —
/// launching something the caller did not ask for, under a name that says it did.
fn worktree_route<'a>(
    entry: &'a Launchable,
    seed: &'a str,
) -> (Option<&'a str>, Option<&'a str>) {
    match entry.source {
        Source::Skill => (Some(entry.name.as_str()), None),
        Source::Agent => (None, Some(seed)),
    }
}

/// `--worktree`: run the entry in a fresh isolated worktree instead of here.
///
/// Delegates to `bn dispatch` wholesale — it already builds the worktree, the branch, the note and
/// the tmux window, leases the slot and launches the agent, and none of that is worth a second
/// copy. The two catalogs differ only in where the prompt comes from:
///
/// * a **skill** is what dispatch natively seeds from, so it goes through `--skill` and picks up
///   the whole ship/scout envelope (plan gate, verify gate, completion protocol) built around it;
/// * an **agent** manifest is already self-contained, so its composed body is handed over as the
///   seed verbatim and the skill envelope is skipped.
#[allow(clippy::too_many_arguments)]
fn run_in_worktree(
    cfg: &Config,
    ctx: Option<&Resolved>,
    entry: &Launchable,
    instruction: &str,
    seed: &str,
    harness: Option<&str>,
    repo: Option<&str>,
    base_branch: Option<&str>,
    yolo: bool,
    json: bool,
) {
    // The item names the work in the note, the branch and the tmux window. An instruction is the
    // better name when there is one; the entry's own name is the honest fallback.
    let item = if instruction.trim().is_empty() {
        entry.name.clone()
    } else {
        instruction.to_string()
    };
    let (skill, seed_override) = worktree_route(entry, seed);
    crate::cmd::dispatch::run_dispatch(
        cfg,
        ctx,
        repo,
        base_branch,
        &item,
        None,
        None,
        false,
        Some(harness.unwrap_or("claude")),
        false,
        false,
        skill,
        false,
        yolo,
        crate::cmd::dispatch::PlanGate::Auto,
        false,
        false,
        false,
        false,
        crate::cli::OnPoolFull::default(),
        None,
        false,
        &[],
        None,
        seed_override,
        json,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::template::ArgumentDef;

    fn entry(name: &str, source: Source) -> Launchable {
        Launchable {
            name: name.to_string(),
            source,
            kind: None,
            description: None,
            scope: "global-shared".to_string(),
            schedule: None,
            arguments: Vec::new(),
            skills: Vec::new(),
            harness: None,
            requires_repo: None,
            shadowed: false,
            path: std::path::PathBuf::new(),
        }
    }

    #[test]
    fn an_arg_without_an_equals_is_refused() {
        let err = parse_args(&["date".to_string()]).unwrap_err();
        assert!(err.contains("KEY=VALUE"), "{err}");
    }

    #[test]
    fn an_arg_with_an_empty_key_is_refused() {
        assert!(parse_args(&["=today".to_string()]).is_err());
    }

    #[test]
    fn args_parse_into_the_template_map() {
        let map = parse_args(&["date=2026-09-05".to_string()])
            .unwrap()
            .expect("map");
        assert_eq!(map.get("date").and_then(Value::as_str), Some("2026-09-05"));
    }

    #[test]
    fn no_args_is_none_not_an_empty_map() {
        // An empty map and `None` are not the same to `render_checked`: a declared-but-optional
        // argument resolves differently, so the distinction has to survive parsing.
        assert!(parse_args(&[]).unwrap().is_none());
    }

    #[test]
    fn the_instruction_lands_under_its_own_heading() {
        let seed = compose_seed("Body.", &entry("scout", Source::Agent), "check the bump");
        assert!(seed.contains("## Your task"), "{seed}");
        assert!(seed.trim_end().ends_with("check the bump"), "{seed}");
    }

    #[test]
    fn no_instruction_leaves_the_body_alone() {
        let seed = compose_seed("Body.", &entry("scout", Source::Agent), "   ");
        assert!(!seed.contains("## Your task"), "{seed}");
        assert_eq!(seed.trim(), "Body.");
    }

    #[test]
    fn declared_skills_become_the_equipped_block() {
        let mut e = entry("scout", Source::Agent);
        e.skills = vec!["repo-reference".to_string()];
        let seed = compose_seed("Body.", &e, "");
        assert!(seed.contains("Equipped for this run"), "{seed}");
        assert!(seed.contains("repo-reference"), "{seed}");
    }

    #[test]
    fn an_entry_with_no_skills_adds_no_equipped_block() {
        let seed = compose_seed("Body.", &entry("debug", Source::Skill), "");
        assert!(!seed.contains("Equipped for this run"), "{seed}");
    }

    #[test]
    fn a_worktree_skill_goes_through_the_dispatch_skill_envelope() {
        let e = entry("debug", Source::Skill);
        let (skill, seed) = worktree_route(&e, "composed body");
        assert_eq!(skill, Some("debug"));
        assert_eq!(seed, None, "a skill must not also override the seed");
    }

    #[test]
    fn a_worktree_agent_hands_over_its_own_seed_and_no_skill() {
        let e = entry("scout", Source::Agent);
        let (skill, seed) = worktree_route(&e, "composed body");
        assert_eq!(
            skill, None,
            "an agent must not be dispatched under a skill name"
        );
        assert_eq!(seed, Some("composed body"));
    }

    #[test]
    fn a_shadowed_skill_never_reaches_the_picker() {
        let mut shadowed = entry("scout", Source::Skill);
        shadowed.shadowed = true;
        let all = vec![entry("scout", Source::Agent), shadowed];
        let picked = rows(&all, None);
        let names: Vec<&str> = picked.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["scout"]);
    }

    #[test]
    fn a_row_carries_the_name_in_the_hidden_column() {
        let all = vec![entry("debug", Source::Skill)];
        let line = rows(&all, None)[0].line();
        assert_eq!(line.split(TAB).nth(1), Some("debug"));
    }

    #[test]
    fn required_arguments_are_flagged_in_the_picker_row() {
        let mut e = entry("today-agent", Source::Agent);
        e.arguments = vec![ArgumentDef {
            name: "date".into(),
            description: None,
            required: true,
        }];
        let all = vec![e];
        assert!(rows(&all, None)[0].display.contains("needs date"));
    }
}
