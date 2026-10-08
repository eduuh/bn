//! Command grouping: the top-level surface agents and people read, kept small without touching
//! a single handler. A grouped name (`bn note close`, `bn tmux status`, `bn cat --brief`) is
//! rewritten to the command that already implements it (`bn close`, `bn tmux-status`,
//! `bn brief`) before clap parses argv, so both spellings run the same code. The old names stay
//! callable but hidden from `bn --help`; tmux bindings, cron jobs and older prompts keep working.
//!
//! Hiding and the new group entries are applied to the clap `Command` at runtime (see
//! [`command`]) rather than as derive attributes, so the `cli.rs` enums stay as they are.
use crate::Cli;
use clap::CommandFactory;
use std::ffi::OsString;

/// `(group, sub, legacy argv)`: `bn <group> <sub> …` runs `bn <legacy…> …`.
pub(crate) const ROUTES: &[(&str, &str, &[&str])] = &[
    ("note", "path", &["note"]),
    ("note", "close", &["close"]),
    ("note", "reopen", &["reopen"]),
    ("note", "rename", &["rename"]),
    ("note", "archive", &["archive"]),
    ("note", "prune", &["prune"]),
    ("note", "stale", &["stale"]),
    ("note", "clean", &["clean"]),
    ("note", "migrate", &["migrate"]),
    ("note", "migrate-layout", &["migrate-layout"]),
    ("note", "reset", &["reset"]),
    ("note", "files", &["files"]),
    ("note", "shot", &["shot"]),
    ("note", "why", &["why"]),
    ("note", "log", &["log-progress"]),
    ("add", "notes", &["notes"]),
    ("add", "tc", &["tc"]),
    ("review", "plan", &["review"]),
    ("review", "diff", &["review-gate"]),
    ("review", "pr", &["review-pr"]),
    ("review", "queue", &["reviews"]),
    ("review", "critic", &["critic"]),
    ("pipeline", "promote", &["promote"]),
    ("wt", "repos", &["repos"]),
    ("task", "q", &["q"]),
    ("agent", "registry", &["sessions"]),
    ("agent", "resume", &["resume"]),
    ("agent", "switch", &["switch"]),
    ("agent", "handoff", &["handoff"]),
    ("agent", "compact", &["compact"]),
    ("agent", "context", &["context"]),
    ("attn", "notify", &["notify"]),
    ("attn", "feed", &["feed"]),
    ("attn", "findings", &["findings"]),
    ("forge", "agent", &["forge-agent"]),
    ("machine", "connect", &["connect"]),
    ("machine", "tunnels", &["tunnels"]),
    ("fleet", "control", &["control"]),
    ("fleet", "worker", &["worker"]),
    ("fleet", "watchdog", &["watchdog"]),
    ("fleet", "on", &["on"]),
    ("fleet", "watch", &["watch"]),
    ("fleet", "startup", &["startup"]),
    ("setup", "harness", &["harness"]),
    ("setup", "trust-folder", &["trust-folder"]),
    ("setup", "shell-init", &["shell-init"]),
    ("setup", "mcp", &["mcp"]),
    ("setup", "completion", &["completion"]),
    ("setup", "hooks", &["hooks"]),
    ("stats", "insights", &["insights"]),
    ("stats", "metrics", &["metrics"]),
    ("stats", "cost", &["cost"]),
    ("stats", "learnings", &["learnings"]),
    ("stats", "event", &["event"]),
    ("tmux", "status", &["tmux-status"]),
    ("tmux", "git", &["tmux-git"]),
    ("tmux", "goal", &["tmux-goal"]),
    ("tmux", "attn-count", &["tmux-attn-count"]),
    ("tmux", "done-badge", &["tmux-done-badge"]),
    ("tmux", "sync", &["tmux-sync"]),
    ("tmux", "agent", &["tmux-agent"]),
    ("tmux", "cockpit", &["tmux-cockpit"]),
    ("tmux", "help", &["tmux-help"]),
    ("tmux", "keys", &["tmux-keys"]),
    ("tmux", "note", &["tmux-note"]),
    ("tmux", "prune", &["tmux-prune"]),
    ("tmux", "reconcile", &["tmux-reconcile"]),
    ("tmux", "snapshot-filter", &["tmux-snapshot-filter"]),
    ("tmux", "glyphs", &["glyphs"]),
];

/// `(command, flag, legacy)`: `bn <command> … <flag> …` runs `bn <legacy> …` with the flag dropped.
pub(crate) const FLAG_ROUTES: &[(&str, &str, &str)] = &[
    ("cat", "--brief", "brief"),
    ("list", "--todos", "todo"),
    ("list", "--summary", "summary"),
    ("list", "--worktrees", "worktrees"),
    ("list", "--repos", "managed-repos"),
    ("status", "--diff", "diff"),
    ("verify", "--rebased", "gate"),
    ("pr", "--wi", "link"),
    ("add", "--url", "capture"),
    ("next", "--stale", "doctor"),
];

/// Groups that exist only as a routing prefix (no command of their own), with their summary.
const NEW_GROUPS: &[(&str, &str)] = &[
    (
        "tmux",
        "tmux status-bar segments, popups and session housekeeping",
    ),
    (
        "stats",
        "Reports over the event log: insights, metrics, cost, learnings",
    ),
    (
        "setup",
        "One-time wiring: harnesses, folder trust, shell wrappers, MCP, completions, git hooks",
    ),
    (
        "fleet",
        "Long-lived coordination: controllers, workers, watchdogs, event rules",
    ),
];

/// Commands kept callable but off `bn --help` because only a hook, job or maintainer runs them.
/// `gap ingest` is how the meetings cron fills the cache `bn next --fit` reads.
const HIDDEN: &[&str] = &["release", "open", "gap"];

/// Routes that exist for hooks and jobs and stay out of group help.
const QUIET_ROUTES: &[(&str, &str)] = &[
    ("note", "log"),
    ("stats", "event"),
    ("fleet", "watch"),
    ("fleet", "startup"),
];

/// The clap command with grouped and hidden names hidden and the new groups listed.
/// Use this instead of `Cli::command()` wherever the surface is shown (help, completions).
pub(crate) fn command() -> clap::Command {
    let mut cmd = Cli::command();
    let legacy = ROUTES
        .iter()
        .filter(|(group, _, to)| to[0] != *group)
        .map(|(_, _, to)| to[0])
        .chain(FLAG_ROUTES.iter().map(|(_, _, to)| *to))
        .chain(HIDDEN.iter().copied());
    for name in legacy {
        cmd = cmd.mut_subcommand(name, |s| s.hide(true));
    }
    for (name, about) in NEW_GROUPS {
        cmd = cmd.subcommand(
            clap::Command::new(*name)
                .about(*about)
                .disable_help_flag(true)
                .arg(
                    clap::Arg::new("args")
                        .num_args(0..)
                        .trailing_var_arg(true)
                        .allow_hyphen_values(true),
                ),
        );
    }
    cmd
}

/// What `main` should do with argv.
pub(crate) enum Plan {
    /// Parse these args (possibly rewritten).
    Parse(Vec<OsString>),
    /// Print this group help and exit with the code.
    Help(String, i32),
}

/// Rewrite a grouped spelling to its legacy command, or answer group help.
pub(crate) fn plan(args: Vec<OsString>) -> Plan {
    let s: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let Some(first) = s.get(1).map(String::as_str) else {
        return Plan::Parse(args);
    };
    let second = s.get(2).map(String::as_str);
    let is_help = |a: Option<&str>| matches!(a, Some("--help" | "-h" | "help"));

    if let Some(sub) = second {
        if let Some((_, _, to)) = ROUTES.iter().find(|(g, n, _)| *g == first && *n == sub) {
            let mut out = vec![args[0].clone()];
            out.extend(to.iter().map(OsString::from));
            out.extend(args[3..].iter().cloned());
            return Plan::Parse(out);
        }
    }
    if let Some((_, flag, to)) = FLAG_ROUTES
        .iter()
        .find(|(c, f, _)| *c == first && s[2..].iter().any(|a| a == f))
    {
        let mut out = vec![args[0].clone(), OsString::from(to)];
        out.extend(
            args[2..]
                .iter()
                .filter(|a| a.to_str() != Some(flag))
                .cloned(),
        );
        return Plan::Parse(out);
    }
    if let Some((name, about)) = NEW_GROUPS.iter().find(|(g, _)| *g == first) {
        let code = if second.is_none() || is_help(second) {
            0
        } else {
            2
        };
        let mut text = format!("{about}\n\nUsage: bn {name} <COMMAND> [ARGS]...\n\nCommands:\n");
        text.push_str(&route_list(name));
        if let Some(bad) = second.filter(|_| code != 0) {
            text = format!("error: unknown command `bn {name} {bad}`\n\n{text}");
        }
        return Plan::Help(text, code);
    }
    if is_help(second)
        && s.len() == 3
        && ROUTES
            .iter()
            .any(|(g, _, to)| *g == first && to[0] != first)
    {
        let mut cmd = command();
        cmd.build();
        let mut text = cmd
            .find_subcommand_mut(first)
            .map(|c| c.render_help().to_string())
            .unwrap_or_default();
        text.push_str("\nAlso under this command:\n");
        text.push_str(&route_list(first));
        return Plan::Help(text, 0);
    }
    Plan::Parse(args)
}

/// One aligned `  <sub>  <about>` line per visible route of `group`.
fn route_list(group: &str) -> String {
    let cmd = Cli::command();
    let rows: Vec<(&str, String)> = ROUTES
        .iter()
        .filter(|(g, n, _)| *g == group && !QUIET_ROUTES.contains(&(*g, *n)))
        .map(|(_, n, to)| {
            let about = cmd
                .find_subcommand(to[0])
                .and_then(|c| c.get_about())
                .map(|a| a.to_string())
                .unwrap_or_default();
            let first = about.split(". ").next().unwrap_or("").trim_end_matches('.');
            let short: String = first.chars().take(100).collect();
            let short = if short.len() < first.len() {
                short + "…"
            } else {
                short
            };
            (*n, short)
        })
        .collect();
    let width = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    rows.iter()
        .map(|(n, a)| format!("  {n:width$}  {a}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<OsString> {
        std::iter::once("bn")
            .chain(s.split_whitespace())
            .map(OsString::from)
            .collect()
    }
    fn parsed(s: &str) -> Vec<String> {
        match plan(argv(s)) {
            Plan::Parse(a) => a
                .iter()
                .skip(1)
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            Plan::Help(t, _) => panic!("`{s}` answered help:\n{t}"),
        }
    }

    fn help_text(args: &[&str]) -> String {
        let err = Cli::command().try_get_matches_from(args).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        err.to_string()
    }

    #[test]
    fn grouped_names_rewrite_to_the_legacy_command() {
        assert_eq!(
            parsed("note close --achievement x"),
            ["close", "--achievement", "x"]
        );
        assert_eq!(parsed("tmux status /tmp"), ["tmux-status", "/tmp"]);
        assert_eq!(parsed("note path"), ["note"]);
        assert_eq!(parsed("review plan submit"), ["review", "submit"]);
        assert_eq!(parsed("agent registry"), ["sessions"]);
    }

    #[test]
    fn flag_routes_drop_the_flag_wherever_it_sits() {
        assert_eq!(parsed("cat --brief"), ["brief"]);
        assert_eq!(parsed("list --json --todos"), ["todo", "--json"]);
        assert_eq!(
            parsed("verify --job lint --rebased"),
            ["gate", "--job", "lint"]
        );
    }

    #[test]
    fn legacy_and_ungrouped_spellings_pass_through_unchanged() {
        for s in [
            "close",
            "tmux-status /tmp",
            "agent sessions",
            "review submit",
            "add todo x",
            "note",
        ] {
            assert_eq!(parsed(s), s.split_whitespace().collect::<Vec<_>>());
        }
    }

    #[test]
    fn new_groups_answer_help_and_reject_unknown_subcommands() {
        assert!(matches!(plan(argv("tmux")), Plan::Help(t, 0) if t.contains("cockpit")));
        assert!(matches!(plan(argv("stats --help")), Plan::Help(t, 0) if t.contains("insights")));
        assert!(matches!(plan(argv("fleet nope")), Plan::Help(t, 2) if t.contains("unknown")));
    }

    #[test]
    fn existing_groups_list_their_routed_commands_in_help() {
        let Plan::Help(t, 0) = plan(argv("note --help")) else {
            panic!("no help")
        };
        assert!(t.contains("close") && t.contains("migrate-layout") && !t.contains("  log "));
    }

    #[test]
    fn help_text_does_not_reference_removed_retro_commands() {
        let insights = help_text(&["bn", "insights", "--help"]);
        assert!(!insights.contains("bn weekly"));
        assert!(!insights.contains("bn retro"));

        let skills = help_text(&["bn", "skills", "--help"]);
        assert!(!skills.contains("bn retro"));
        assert!(skills.contains("bn skills find"));
    }

    /// Every route must land on a real command that clap accepts, and must not shadow a real
    /// subcommand of its group (that would silently change what an existing spelling runs).
    #[test]
    fn every_route_targets_a_real_command_and_shadows_nothing() {
        let cmd = Cli::command();
        for (group, sub, to) in ROUTES {
            let target = cmd.find_subcommand(to[0]);
            assert!(target.is_some(), "bn {group} {sub} -> unknown `{}`", to[0]);
            if let Some(g) = cmd.find_subcommand(group) {
                assert!(
                    g.find_subcommand(sub).is_none(),
                    "bn {group} {sub} shadows a real subcommand"
                );
            }
            let mut probe: Vec<&str> = vec!["bn"];
            probe.extend(to.iter());
            probe.push("--help");
            let err = cmd.clone().try_get_matches_from(&probe).unwrap_err();
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp,
                "`{probe:?}` does not parse"
            );
        }
        for (c, flag, to) in FLAG_ROUTES {
            let c = cmd.find_subcommand(c).expect("flag route command");
            assert!(
                c.get_arguments()
                    .all(|a| a.get_long() != flag.strip_prefix("--")),
                "{flag} exists"
            );
            assert!(cmd.find_subcommand(to).is_some(), "unknown `{to}`");
        }
        for name in HIDDEN {
            assert!(cmd.find_subcommand(name).is_some(), "unknown `{name}`");
        }
    }
}
