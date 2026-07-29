//! cwd → (repo, branch) → note path. Port of `resolve_note_context`
//! (config/scripts/tmux-lib.zsh:151) + `notes_dir_for` / work-repos classification (bn:101-116).
//!
//! Two derivation cases, kept distinct exactly as the oracle does:
//!   A. cwd under `$WORKTREE_DIR/` → repo = first path segment (from the *directory*),
//!      branch from git.
//!   B. otherwise → repo = basename(git toplevel), with a `.bare`-sibling override that
//!      uses the parent dir name; branch from git.
//! Then: repo lowercased, branch slashes→dashes; empty repo|branch ⇒ failure.

mod config;
mod config_file;
mod context;
mod dirs;
mod jobs;
mod paths;
mod tools_config;
mod util;

pub use config::{
    Config, ContextError, LogConfig, PipelineConfig, PipelineModel, PipelineModelConfig,
    RepoBranch, RepoConfig, RepoPlacementPolicy, Resolved, WebConfig,
};
pub use context::{repo_root, resolve, resolve_hint, resolve_target};
pub use dirs::{
    agents_dir, bin_dir, bn_home, config_dir, config_file_path, jobs_dir, log_dir,
    managed_repos_dir, references_dir, state_dir, web_token,
};
pub use jobs::{
    agent_dirs, job_dirs, job_script, list_jobs, mcp_extras, quarter_review_cron_override,
    JobConfig, JobRef, JobScope,
};
pub use tools_config::ToolsConfig;

#[cfg(test)]
mod tests {
    use super::config_file::ConfigFile;
    use super::context::repo_from_common_dir;
    use super::util::{parse_weekday, parse_work_repos, split_list};
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn cfg(work_repos: Vec<&str>) -> Config {
        Config {
            personal_root: PathBuf::from("/p"),
            work_root: PathBuf::from("/w"),
            worktree_dir: PathBuf::from("/home/u/projects/worktree"),
            work_repos: work_repos.into_iter().map(String::from).collect(),
            hostname: "host".into(),
            ..Default::default()
        }
    }

    #[test]
    fn job_config_defaults_are_all_off() {
        // No file → the autopilot jobs are opt-in: every flag off, sensible default times.
        let jc = JobConfig::resolve(None);
        assert!(!jc.wrap_enabled);
        assert!(!jc.standup_enabled);
        assert!(!jc.standup_fetch_delta);
        assert!(!jc.dxi_enabled);
        assert_eq!(jc.wrap_time, "17:00");
        assert_eq!(jc.standup_time, "09:00");
        // Night shift: off, opt-in per repo (empty allowlist), sensible cap + window.
        assert!(!jc.night_enabled);
        assert_eq!(jc.night_max_dispatch, 3);
        assert_eq!(jc.night_window, "23:00-06:00");
        assert!(jc.night_repos.is_empty());
    }

    #[test]
    fn job_config_reads_sections_from_file() {
        let toml = r#"
            [wrap]
            enabled = true
            time = "16:30"

            [standup]
            enabled = true
            fetch_delta = true

            [dxi]
            enabled = true

            [night]
            enabled = true
            max_dispatch = 5
            window = "22:00-05:00"
            repos = ["bn", "arena"]
        "#;
        let cf: ConfigFile = toml::from_str(toml).unwrap();
        let jc = JobConfig::resolve(Some(&cf));
        assert!(jc.wrap_enabled);
        assert_eq!(jc.wrap_time, "16:30");
        assert!(jc.standup_enabled);
        assert!(jc.standup_fetch_delta);
        assert_eq!(jc.standup_time, "09:00"); // unset → default
        assert!(jc.dxi_enabled);
        assert!(jc.night_enabled);
        assert_eq!(jc.night_max_dispatch, 5);
        assert_eq!(jc.night_window, "22:00-05:00");
        assert_eq!(jc.night_repos, vec!["bn".to_string(), "arena".to_string()]);
    }

    #[test]
    fn tools_config_defaults_to_no_builtins() {
        let _g = crate::test_support::env_lock();
        let _builtins = crate::test_support::EnvGuard::remove("BN_TOOLS_BUILTINS");
        assert!(ToolsConfig::resolve(None).builtins.is_empty());
    }

    #[test]
    fn tools_config_reads_builtins_from_file_and_env() {
        let _g = crate::test_support::env_lock();
        let _builtins = crate::test_support::EnvGuard::remove("BN_TOOLS_BUILTINS");
        let cf: ConfigFile = toml::from_str("[tools]\nbuiltins = [\"n8n\"]\n").unwrap();
        assert_eq!(ToolsConfig::resolve(Some(&cf)).builtins, vec!["n8n"]);

        let _env = crate::test_support::EnvGuard::set("BN_TOOLS_BUILTINS", "n8n other");
        assert_eq!(
            ToolsConfig::resolve(Some(&cf)).builtins,
            vec!["n8n".to_string(), "other".to_string()]
        );
    }

    #[test]
    fn sentinel_allowlist_defaults_to_the_night_allowlist() {
        // Guard the env so a developer's real BN_* vars can't perturb the resolution.
        let _g = crate::test_support::env_lock();
        let _s = crate::test_support::EnvGuard::remove("BN_SENTINEL_REPOS");
        let _n = crate::test_support::EnvGuard::remove("BN_NIGHT_REPOS");
        let _a = crate::test_support::EnvGuard::remove("BN_SENTINEL_TODO_AGE_DAYS");

        // No [sentinel] → inherits [night] repos, default todo age.
        let cf: ConfigFile = toml::from_str("[night]\nrepos = [\"bn\", \"arena\"]\n").unwrap();
        let jc = JobConfig::resolve(Some(&cf));
        assert_eq!(
            jc.sentinel_repos,
            vec!["bn".to_string(), "arena".to_string()]
        );
        assert_eq!(jc.sentinel_todo_age_days, 30);

        // Explicit [sentinel] overrides the inheritance.
        let cf: ConfigFile = toml::from_str(
            "[night]\nrepos = [\"bn\"]\n[sentinel]\nrepos = [\"arena\"]\ntodo_age_days = 14\n",
        )
        .unwrap();
        let jc = JobConfig::resolve(Some(&cf));
        assert_eq!(jc.sentinel_repos, vec!["arena".to_string()]);
        assert_eq!(jc.sentinel_todo_age_days, 14);

        // Nothing set anywhere → empty (a no-op patrol).
        assert!(JobConfig::resolve(None).sentinel_repos.is_empty());
    }

    #[test]
    fn job_config_reads_quarter_section_and_defaults() {
        // Defaults: opt-in off, weekly window, no explicit harness.
        let d = JobConfig::resolve(None);
        assert!(!d.quarter_review_enabled);
        assert_eq!(d.quarter_review_window_days, 7);
        assert_eq!(d.quarter_harness, None);
        // File section is read.
        let cf: ConfigFile = toml::from_str(
            "[quarter]\nenabled = true\nreview_window_days = 14\nharness = \"claude\"\n",
        )
        .unwrap();
        let jc = JobConfig::resolve(Some(&cf));
        assert!(jc.quarter_review_enabled);
        assert_eq!(jc.quarter_review_window_days, 14);
        assert_eq!(jc.quarter_harness.as_deref(), Some("claude"));
    }

    #[test]
    fn quarter_review_cron_override_reads_env() {
        let _g = crate::test_support::env_lock();
        let _cron =
            crate::test_support::EnvGuard::set("BN_QUARTER_REVIEW_CRON", "Fri *-*-* 16:00:00");
        assert_eq!(
            quarter_review_cron_override().as_deref(),
            Some("Fri *-*-* 16:00:00")
        );
    }

    #[test]
    fn mcp_extras_parse_from_config_section() {
        let cf: ConfigFile =
            toml::from_str("[mcp]\nextras = [\"sequential-thinking\", \"time\"]\n").unwrap();
        assert_eq!(
            cf.mcp_extras(),
            Some(vec!["sequential-thinking".to_string(), "time".to_string()])
        );
        // Absent section → None (bn only).
        let empty: ConfigFile = toml::from_str("worktree_dir = \"/x\"\n").unwrap();
        assert_eq!(empty.mcp_extras(), None);
    }

    #[test]
    fn cron_global_layout_resolves_to_pane_flag() {
        // "pane" → shared-window layout for global crons; anything else (or unset) → window-per-job.
        let cf: ConfigFile = toml::from_str("[jobs]\ncron_global_layout = \"pane\"\n").unwrap();
        assert_eq!(cf.cron_global_layout().as_deref(), Some("pane"));
        assert!(Config::resolve(Some(&cf)).jobs_cron_global_pane);

        let cf: ConfigFile = toml::from_str("[jobs]\ncron_global_layout = \"window\"\n").unwrap();
        assert!(!Config::resolve(Some(&cf)).jobs_cron_global_pane);

        // Unset → default window-per-job.
        let cf: ConfigFile = toml::from_str("[jobs]\ntmux_window = true\n").unwrap();
        assert!(!Config::resolve(Some(&cf)).jobs_cron_global_pane);
    }

    #[test]
    fn cockpit_session_resolves_from_config_and_default() {
        // `[jobs].session` re-homes the whole cron/fleet cockpit.
        let cf: ConfigFile = toml::from_str("[jobs]\nsession = \"fleet\"\n").unwrap();
        assert_eq!(Config::resolve(Some(&cf)).cockpit_session, "fleet");

        // Unset [jobs].session → default "cron".
        let cf: ConfigFile = toml::from_str("[jobs]\ntmux_window = true\n").unwrap();
        assert_eq!(Config::resolve(Some(&cf)).cockpit_session, "cron");

        // No config file at all → default "cron".
        assert_eq!(Config::resolve(None).cockpit_session, "cron");
    }

    #[test]
    fn staff_home_resolves_from_config_and_env() {
        // `[staff].home` from config.toml.
        let cf: ConfigFile = toml::from_str("[staff]\nhome = \"boxB\"\n").unwrap();
        assert_eq!(cf.staff_home().as_deref(), Some("boxB"));

        let _g = crate::test_support::env_lock();
        // No env → the config value stands.
        let _off = crate::test_support::EnvGuard::remove("BN_STAFF_HOME");
        assert_eq!(
            Config::resolve(Some(&cf)).staff_home.as_deref(),
            Some("boxB")
        );

        // $BN_STAFF_HOME overrides the file.
        let _home = crate::test_support::EnvGuard::set("BN_STAFF_HOME", "boxA");
        assert_eq!(
            Config::resolve(Some(&cf)).staff_home.as_deref(),
            Some("boxA")
        );

        // Unset everywhere → None (the run-anywhere default).
        drop(_home);
        assert_eq!(Config::resolve(None).staff_home, None);
    }

    #[test]
    fn staff_home_blank_is_none() {
        let _g = crate::test_support::env_lock();
        let _off = crate::test_support::EnvGuard::remove("BN_STAFF_HOME");
        // A whitespace-only value never gates the staff off.
        let cf: ConfigFile = toml::from_str("[staff]\nhome = \"   \"\n").unwrap();
        assert_eq!(Config::resolve(Some(&cf)).staff_home, None);
    }

    #[test]
    fn orchestrate_caps_resolve_from_config_file() {
        // #377: resource/budget caps parse from `[orchestrate]`; unset → all off (unchanged daemon).
        let cf: ConfigFile = toml::from_str(
            "[orchestrate]\n\
             min_free_ram_mb = 3000\n\
             max_loadavg = 8.5\n\
             max_swap_pct = 60.0\n\
             tokens_per_hour = 2000000\n\
             daily_cost_usd = 25.0\n\
             daily_cost_usd_coding = 20.0\n\
             daily_cost_usd_research = 5.0\n\
             remote_overflow = true\n",
        )
        .unwrap();
        assert_eq!(cf.orchestrate_min_free_ram_mb(), Some(3000));
        assert_eq!(cf.orchestrate_max_loadavg(), Some(8.5));
        assert_eq!(cf.orchestrate_max_swap_pct(), Some(60.0));
        assert_eq!(cf.orchestrate_tokens_per_hour(), Some(2_000_000));
        assert_eq!(cf.orchestrate_daily_cost_usd(), Some(25.0));
        assert_eq!(cf.orchestrate_daily_cost_usd_coding(), Some(20.0));
        assert_eq!(cf.orchestrate_daily_cost_usd_research(), Some(5.0));
        assert_eq!(cf.orchestrate_remote_overflow(), Some(true));

        // Nothing configured → every guard off, so the scheduler behaves as fixed-concurrency.
        let bare: ConfigFile = toml::from_str("").unwrap();
        assert_eq!(bare.orchestrate_min_free_ram_mb(), None);
        assert_eq!(bare.orchestrate_max_loadavg(), None);
        assert_eq!(bare.orchestrate_max_swap_pct(), None);
        assert_eq!(bare.orchestrate_tokens_per_hour(), None);
        assert_eq!(bare.orchestrate_daily_cost_usd(), None);
        assert_eq!(bare.orchestrate_daily_cost_usd_coding(), None);
        assert_eq!(bare.orchestrate_daily_cost_usd_research(), None);
        assert_eq!(bare.orchestrate_remote_overflow(), None);
    }

    #[test]
    fn orchestrate_keep_warm_resolves_from_config_file() {
        let cf: ConfigFile = toml::from_str(
            "[orchestrate]\n\
             keep_warm = true\n\
             low_water = 4\n\
             own = [\"bn\", \"wira360\"]\n\
             exclude_titles = [\"gap(\", \"audit:\"]\n\
             exclude_labels = [\"blocked\", \"hold\"]\n\
             brainstorm_count = 2\n\
             brainstorm_interval_secs = 1800\n",
        )
        .unwrap();
        let cfg = Config::resolve(Some(&cf));
        assert!(cfg.orchestrate_keep_warm);
        assert_eq!(cfg.orchestrate_low_water, 4);
        assert_eq!(cfg.orchestrate_owned_repos, vec!["bn", "wira360"]);
        assert_eq!(cfg.orchestrate_exclude_titles, vec!["gap(", "audit:"]);
        assert_eq!(cfg.orchestrate_exclude_labels, vec!["blocked", "hold"]);
        assert_eq!(cfg.orchestrate_brainstorm_count, 2);
        assert_eq!(cfg.orchestrate_brainstorm_interval_secs, 1_800);

        let default = Config::resolve(None);
        assert!(!default.orchestrate_keep_warm);
        assert_eq!(
            default.orchestrate_low_water,
            crate::orchestrate::DEFAULT_KEEP_WARM_LOW_WATER
        );
    }

    #[test]
    fn orchestrate_agent_resolves_from_config_and_env() {
        // #382: `[orchestrate].agent` from config.toml, env override, blank → None.
        let cf: ConfigFile = toml::from_str("[orchestrate]\nagent = \"claude\"\n").unwrap();
        assert_eq!(cf.orchestrate_agent().as_deref(), Some("claude"));

        let _g = crate::test_support::env_lock();
        // No env → the config value stands.
        let _off = crate::test_support::EnvGuard::remove("BN_ORCHESTRATE_AGENT");
        assert_eq!(
            Config::resolve(Some(&cf)).orchestrate_agent.as_deref(),
            Some("claude")
        );

        // $BN_ORCHESTRATE_AGENT overrides the file.
        let _env = crate::test_support::EnvGuard::set("BN_ORCHESTRATE_AGENT", "copilot");
        assert_eq!(
            Config::resolve(Some(&cf)).orchestrate_agent.as_deref(),
            Some("copilot")
        );
        drop(_env);

        // Unset everywhere → None (the daemon then falls back to copilot).
        assert_eq!(Config::resolve(None).orchestrate_agent, None);

        // A whitespace-only value → None rather than a blank harness.
        let blank: ConfigFile = toml::from_str("[orchestrate]\nagent = \"   \"\n").unwrap();
        assert_eq!(Config::resolve(Some(&blank)).orchestrate_agent, None);
    }

    #[test]
    fn orchestrate_stuck_idle_threshold_resolves_from_config_and_env() {
        let cf: ConfigFile = toml::from_str("[orchestrate]\nstuck_idle_secs = 900\n").unwrap();
        let _g = crate::test_support::env_lock();
        let _off = crate::test_support::EnvGuard::remove("BN_ORCH_STUCK_IDLE_SECS");
        assert_eq!(Config::resolve(Some(&cf)).orchestrate_stuck_idle_secs, 900);

        let _env = crate::test_support::EnvGuard::set("BN_ORCH_STUCK_IDLE_SECS", "120");
        assert_eq!(Config::resolve(Some(&cf)).orchestrate_stuck_idle_secs, 120);
    }

    #[test]
    fn review_diff_gate_resolves_from_config_and_env() {
        // #607: `[review].diff_gate` + `diff_gate_reviews` from config.toml, env override, defaults.
        let cf: ConfigFile =
            toml::from_str("[review]\ndiff_gate = true\ndiff_gate_reviews = 3\n").unwrap();
        assert_eq!(cf.review_diff_gate(), Some(true));
        assert_eq!(cf.review_diff_gate_reviews(), Some(3));

        let _g = crate::test_support::env_lock();
        let _off1 = crate::test_support::EnvGuard::remove("BN_REVIEW_DIFF_GATE");
        let _off2 = crate::test_support::EnvGuard::remove("BN_REVIEW_DIFF_GATE_REVIEWS");
        let c = Config::resolve(Some(&cf));
        assert!(c.review_diff_gate);
        assert_eq!(c.review_diff_gate_reviews, 3);

        // Unset everywhere → off, default required count.
        let c = Config::resolve(None);
        assert!(!c.review_diff_gate);
        assert_eq!(
            c.review_diff_gate_reviews,
            crate::review_gate::DEFAULT_REQUIRED_REVIEWS
        );

        // Env overrides the file; a zero/blank count falls back to the default (a 0-review gate
        // would be a no-op).
        let _e1 = crate::test_support::EnvGuard::set("BN_REVIEW_DIFF_GATE", "1");
        let _e2 = crate::test_support::EnvGuard::set("BN_REVIEW_DIFF_GATE_REVIEWS", "0");
        let c = Config::resolve(None);
        assert!(c.review_diff_gate);
        assert_eq!(
            c.review_diff_gate_reviews,
            crate::review_gate::DEFAULT_REQUIRED_REVIEWS
        );
    }

    #[test]
    fn calib_coarse_sizes_override_and_default() {
        // No config → the built-in 30m/90m/3h defaults.
        assert_eq!(
            Config::resolve(None).coarse_sizes,
            crate::calib::CoarseSizes::default()
        );

        // A full `[calib]` table overrides all three, parsed via the duration grammar.
        let cf: ConfigFile =
            toml::from_str("[calib]\ns = \"15m\"\nm = \"1h\"\nl = \"2h30m\"\n").unwrap();
        assert_eq!(
            Config::resolve(Some(&cf)).coarse_sizes,
            crate::calib::CoarseSizes {
                s: 15 * 60,
                m: 3600,
                l: 150 * 60,
            }
        );

        // Partial/garbage degrades per-key: only `m` is valid, so `s`/`l` keep their defaults.
        let d = crate::calib::CoarseSizes::default();
        let cf: ConfigFile = toml::from_str("[calib]\nm = \"45m\"\nl = \"nonsense\"\n").unwrap();
        assert_eq!(
            Config::resolve(Some(&cf)).coarse_sizes,
            crate::calib::CoarseSizes {
                s: d.s,
                m: 45 * 60,
                l: d.l,
            }
        );
    }

    #[test]
    fn repo_from_common_dir_maps_every_layout() {
        // bare worktree layout: the shared `<repo>.git` → strip the suffix.
        assert_eq!(
            repo_from_common_dir(Path::new("/home/u/projects/bare/arena.git")).as_deref(),
            Some("arena")
        );
        // case is preserved here (normalize() lowercases later).
        assert_eq!(
            repo_from_common_dir(Path::new("/home/u/projects/bare/BigMono.git")).as_deref(),
            Some("BigMono")
        );
        // in-place `.bare` layout → the enclosing repo dir name.
        assert_eq!(
            repo_from_common_dir(Path::new("/home/u/projects/arena/.bare")).as_deref(),
            Some("arena")
        );
        // a STANDALONE clone's plain `.git` → None: git must NOT override the directory, so a
        // sub-repo nested inside another repo's worktree (e.g. worktree/BigMono/WebApp) keeps the
        // umbrella grouping the directory expresses rather than splitting into its own repo.
        assert_eq!(
            repo_from_common_dir(Path::new("/home/u/projects/worktree/BigMono/WebApp/.git")),
            None
        );
    }

    #[test]
    fn resolve_target_normalizes_explicit_pair() {
        let c = cfg(vec![]);
        // Explicit repo+branch is normalized (repo lowercased, branch slashes → dashes)
        // without touching the filesystem, regardless of the passed dir.
        let r = resolve_target(
            &c,
            Path::new("/nowhere"),
            Some("MyRepo".into()),
            Some("feature/x".into()),
        )
        .unwrap();
        assert_eq!(r.repo, "myrepo");
        assert_eq!(r.branch, "feature-x");
        // A partial pair is rejected before directory resolution, so callers never
        // accidentally fall back to cwd when they typo one half of an explicit target.
        assert_eq!(
            resolve_target(&c, Path::new("/nowhere"), Some("r".into()), None),
            Err(ContextError::PartialTarget)
        );
        assert_eq!(
            resolve_target(&c, Path::new("/nowhere"), None, Some("main".into())),
            Err(ContextError::PartialTarget)
        );
    }

    #[test]
    fn job_only_on_gates_by_machine() {
        let mut c = cfg(vec![]);
        c.hostname = "my-laptop".into();
        c.job_only_on = HashMap::from([(
            "commit-notes".to_string(),
            vec!["my-laptop".to_string(), "work-pc".to_string()],
        )]);
        // Listed job: allowed only on a named machine.
        assert!(c.job_allowed_here("commit-notes"));
        c.hostname = "RandomBox".into();
        assert!(!c.job_allowed_here("commit-notes"));
        // Unlisted job: runs everywhere.
        assert!(c.job_allowed_here("refresh-all"));
    }

    #[test]
    fn parse_weekday_names_and_numbers() {
        assert_eq!(parse_weekday("sunday"), Some(0));
        assert_eq!(parse_weekday("Mon"), Some(1));
        assert_eq!(parse_weekday("FRI"), Some(5));
        assert_eq!(parse_weekday("6"), Some(6));
        assert_eq!(parse_weekday("7"), None);
        assert_eq!(parse_weekday("xy"), None); // too short / unknown
    }

    #[test]
    fn split_list_splits_on_commas_spaces_newlines() {
        assert_eq!(
            split_list("a, b\nc\td  ,, e"),
            vec!["a", "b", "c", "d", "e"]
        );
        assert!(split_list("   ").is_empty());
    }

    #[test]
    fn config_file_parses_managed_repos() {
        let cf: ConfigFile = toml::from_str(
            "[managed_repos]\nrepos = [\"git@github.com:you/dotfiles.git\", \"https://x/y.git\"]\n",
        )
        .unwrap();
        assert_eq!(
            cf.managed_repos().unwrap(),
            vec!["git@github.com:you/dotfiles.git", "https://x/y.git"]
        );
        // Absent table → None (Config::resolve then defaults to an empty list).
        let empty: ConfigFile = toml::from_str("worktree_dir = \"/x\"\n").unwrap();
        assert!(empty.managed_repos().is_none());
    }

    #[test]
    fn config_file_parses_hosts_table() {
        let cf: ConfigFile = toml::from_str(
            "[hosts.workstation]\nrepo_root = \"~/projects/worktree\"\nharness = \"claude\"\nbn = true\n\n[hosts.pi]\nbn = false\n",
        )
        .unwrap();
        let hosts = cf.hosts.unwrap();
        let ws = &hosts["workstation"];
        assert_eq!(ws.repo_root.as_deref(), Some("~/projects/worktree"));
        assert_eq!(ws.harness.as_deref(), Some("claude"));
        assert_eq!(ws.bn, Some(true));
        // Partial entries deserialize with the rest defaulted.
        let pi = &hosts["pi"];
        assert_eq!(pi.bn, Some(false));
        assert_eq!(pi.harness, None);
        // Absent table → None (Config::resolve then defaults to an empty map).
        let empty: ConfigFile = toml::from_str("worktree_dir = \"/x\"\n").unwrap();
        assert!(empty.hosts.is_none());
    }

    #[test]
    fn config_file_parses_repo_table() {
        // `[repo.<name>].verify` (#454) — the repo's native verify command.
        let cf: ConfigFile =
            toml::from_str("[repo.arena]\nverify = \"npm run validate\"\n\n[repo.other]\n")
                .unwrap();
        let repos = cf.repos().unwrap();
        assert_eq!(repos["arena"].verify.as_deref(), Some("npm run validate"));
        // A table with no `verify` key deserializes with the field defaulted to None.
        assert_eq!(repos["other"].verify, None);
        // Absent table → None (Config::resolve then defaults to an empty map).
        let empty: ConfigFile = toml::from_str("worktree_dir = \"/x\"\n").unwrap();
        assert!(empty.repos().is_none());
    }

    #[test]
    fn config_file_parses_global_and_repo_pipeline_models() {
        let cf: ConfigFile = toml::from_str(
            r#"
[pipeline.models.plan]
model = "claude-fable-5"
effort = "max"
harness = "claude"

[repo.arena.pipeline.models.execute]
model = "claude-sonnet-4.6"
agent = "copilot"
"#,
        )
        .unwrap();
        let global = cf.pipeline().unwrap();
        assert_eq!(
            global.models[&crate::pipeline::stage::Stage::Plan],
            PipelineModelConfig {
                model: Some("claude-fable-5".into()),
                effort: Some("max".into()),
                harness: Some("claude".into()),
            }
        );
        let repos = cf.repos().unwrap();
        let execute = &repos["arena"].pipeline.as_ref().unwrap().models
            [&crate::pipeline::stage::Stage::Execute];
        assert_eq!(execute.model.as_deref(), Some("claude-sonnet-4.6"));
        assert_eq!(execute.effort, None);
        // `agent` is an accepted alias for `harness`.
        assert_eq!(execute.harness.as_deref(), Some("copilot"));
    }

    #[test]
    fn pipeline_model_layers_defaults_global_and_repo_overrides() {
        use crate::pipeline::stage::Stage;

        let mut c = cfg(vec![]);
        assert_eq!(
            c.pipeline_model("bn", Stage::Investigate),
            PipelineModel {
                model: "gpt-5.4".into(),
                effort: "high".into(),
                harness: None,
            }
        );
        assert_eq!(
            c.pipeline_model("bn", Stage::Plan),
            PipelineModel {
                model: "claude-opus-4.8".into(),
                effort: "max".into(),
                harness: None,
            }
        );
        assert_eq!(
            c.pipeline_model("bn", Stage::Critique),
            PipelineModel {
                model: "claude-opus-4.8".into(),
                effort: "max".into(),
                harness: None,
            }
        );
        assert_eq!(
            c.pipeline_model("bn", Stage::Execute),
            PipelineModel {
                model: "gpt-5.4-mini".into(),
                effort: "medium".into(),
                harness: None,
            }
        );

        c.pipeline.models.insert(
            Stage::Execute,
            PipelineModelConfig {
                model: Some("gpt-5.6-sol".into()),
                effort: Some("high".into()),
                harness: None,
            },
        );
        c.repos.insert(
            "Arena".into(),
            RepoConfig {
                pipeline: Some(PipelineConfig {
                    models: HashMap::from([(
                        Stage::Execute,
                        PipelineModelConfig {
                            model: None,
                            effort: Some("max".into()),
                            harness: None,
                        },
                    )]),
                }),
                ..Default::default()
            },
        );
        assert_eq!(
            c.pipeline_model("owner/arena", Stage::Execute),
            PipelineModel {
                model: "gpt-5.6-sol".into(),
                effort: "max".into(),
                harness: None,
            }
        );
    }

    #[test]
    fn pipeline_model_layers_harness() {
        use crate::pipeline::stage::Stage;

        let mut c = cfg(vec![]);
        // Global stage harness applies...
        c.pipeline.models.insert(
            Stage::Execute,
            PipelineModelConfig {
                model: Some("claude-opus-4.8".into()),
                effort: Some("max".into()),
                harness: Some("copilot".into()),
            },
        );
        assert_eq!(
            c.pipeline_model("bn", Stage::Execute).harness.as_deref(),
            Some("copilot")
        );
        // ...and a repo override wins over the global harness.
        c.repos.insert(
            "Arena".into(),
            RepoConfig {
                pipeline: Some(PipelineConfig {
                    models: HashMap::from([(
                        Stage::Execute,
                        PipelineModelConfig {
                            model: None,
                            effort: None,
                            harness: Some("claude".into()),
                        },
                    )]),
                }),
                ..Default::default()
            },
        );
        let resolved = c.pipeline_model("owner/arena", Stage::Execute);
        assert_eq!(resolved.harness.as_deref(), Some("claude"));
        // Model/effort still inherit from the global entry the repo left unset.
        assert_eq!(resolved.model, "claude-opus-4.8");
        assert_eq!(resolved.effort, "max");
    }

    #[test]
    fn pipeline_model_blank_harness_is_ignored() {
        use crate::pipeline::stage::Stage;

        let mut c = cfg(vec![]);
        c.pipeline.models.insert(
            Stage::Plan,
            PipelineModelConfig {
                model: None,
                effort: None,
                harness: Some("   ".into()),
            },
        );
        assert_eq!(c.pipeline_model("bn", Stage::Plan).harness, None);
    }

    #[test]
    fn pipeline_model_defaults_harness_none() {
        use crate::pipeline::stage::Stage;

        let c = cfg(vec![]);
        for stage in [
            Stage::Investigate,
            Stage::Plan,
            Stage::Execute,
            Stage::Critique,
        ] {
            assert_eq!(c.pipeline_model("bn", stage).harness, None, "{stage:?}");
        }
    }

    #[test]
    fn repo_verify_resolves_by_name_case_slug_and_falls_back() {
        let mut c = cfg(vec![]);
        c.repos = HashMap::from([
            (
                "arena".to_string(),
                RepoConfig {
                    verify: Some("npm run validate".to_string()),
                    ..Default::default()
                },
            ),
            // A key with surrounding whitespace-only value must NOT count as configured.
            (
                "blank".to_string(),
                RepoConfig {
                    verify: Some("   ".to_string()),
                    ..Default::default()
                },
            ),
        ]);
        // Exact bn repo name.
        assert_eq!(c.repo_verify("arena"), Some("npm run validate"));
        // resolver lowercases repo names, but a `[repo.MyRepo]`-style key still resolves.
        assert_eq!(c.repo_verify("ARENA"), Some("npm run validate"));
        // A gh `owner/repo` slug resolves on the trailing segment.
        assert_eq!(c.repo_verify("m365-core/arena"), Some("npm run validate"));
        // Whitespace-only command is treated as unset.
        assert_eq!(c.repo_verify("blank"), None);
        // Unconfigured repo (e.g. the bn repo) ⇒ None, so callers fall back to the bn test job.
        assert_eq!(c.repo_verify("bn"), None);
    }

    #[test]
    fn repo_placement_policy_resolves_config_and_mt5_safety_default() {
        let mut c = cfg(vec![]);
        c.repos = HashMap::from([
            (
                "owner/arena".to_string(),
                RepoConfig {
                    requires: vec![
                        " Has-Node ".to_string(),
                        "has-node".to_string(),
                        String::new(),
                    ],
                    excludes: vec![" GPU ".to_string()],
                    ..Default::default()
                },
            ),
            (
                "MT5-DATA-API".to_string(),
                RepoConfig {
                    requires: vec!["has-node".to_string()],
                    ..Default::default()
                },
            ),
        ]);

        assert_eq!(
            c.repo_placement_policy("owner/arena"),
            RepoPlacementPolicy {
                requires: vec!["has-node".to_string()],
                excludes: vec!["gpu".to_string()],
            }
        );
        assert_eq!(
            c.repo_placement_policy("/work/mt5-data-api").requires,
            vec!["has-node".to_string(), "mt5-bridge".to_string()]
        );
        assert_eq!(
            c.repo_placement_policy("MT5-DATA-API").requires,
            vec!["has-node".to_string(), "mt5-bridge".to_string()]
        );
        assert_eq!(
            c.repo_placement_policy("bn"),
            RepoPlacementPolicy::default()
        );
    }

    #[test]
    fn managed_repos_dir_from_home_then_override() {
        let _g = crate::test_support::env_lock();
        let managed = crate::test_support::EnvGuard::remove("BN_MANAGED_REPOS_DIR");
        let _home = crate::test_support::EnvGuard::set("BN_HOME", "/tmp/bnhome");
        assert_eq!(managed_repos_dir(), PathBuf::from("/tmp/bnhome/repos"));
        managed.set_value("/custom/repos");
        assert_eq!(managed_repos_dir(), PathBuf::from("/custom/repos"));
    }

    #[test]
    fn global_jobs_resolve_only_sh_files() {
        let _g = crate::test_support::env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("refresh-all.sh"), "#!/bin/sh\ntrue\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "not a job\n").unwrap();
        let _jobs = crate::test_support::EnvGuard::set("BN_JOBS_DIR", dir.path());

        let c = cfg(vec![]);
        // only `.sh` files; README.md and unknown names don't resolve
        let (scope, path) = job_script(&c, None, "refresh-all").unwrap();
        assert_eq!(scope, JobScope::Global);
        assert!(path.ends_with("refresh-all.sh"));
        assert!(job_script(&c, None, "README").is_none());
        let jobs = list_jobs(&c, None);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "refresh-all");
    }

    #[test]
    fn global_jobs_resolve_work_then_personal_then_shipped() {
        let _g = crate::test_support::env_lock();
        let work = tempfile::tempdir().unwrap();
        let personal = tempfile::tempdir().unwrap();
        let shipped = tempfile::tempdir().unwrap();
        // shipped tier is BN_JOBS_DIR itself; point it at a `jobs/` dir so `mk` is uniform.
        let _jobs = crate::test_support::EnvGuard::set("BN_JOBS_DIR", shipped.path().join("jobs"));

        // create `<root>/jobs/<name>.sh`
        let mk = |root: &std::path::Path, name: &str| {
            let jobs = root.join("jobs");
            std::fs::create_dir_all(&jobs).unwrap();
            std::fs::write(jobs.join(format!("{name}.sh")), "#!/bin/sh\ntrue\n").unwrap();
        };

        let c = Config {
            personal_root: personal.path().to_path_buf(),
            work_root: work.path().to_path_buf(),
            worktree_dir: PathBuf::from("/none"),
            hostname: "host".into(),
            ..Default::default()
        };

        // shipped-only job still resolves (fallback tier intact)
        mk(shipped.path(), "ensure-tmux");
        let (scope, path) = job_script(&c, None, "ensure-tmux").unwrap();
        assert_eq!(scope, JobScope::Global);
        assert!(path.starts_with(shipped.path()));

        // personal-only job resolves from the private personal tier
        mk(personal.path(), "personal-job");
        assert!(job_script(&c, None, "personal-job")
            .unwrap()
            .1
            .starts_with(personal.path()));

        // same name in all three tiers → work wins (highest precedence)
        mk(work.path(), "pr-queue");
        mk(personal.path(), "pr-queue");
        mk(shipped.path(), "pr-queue");
        assert!(
            job_script(&c, None, "pr-queue")
                .unwrap()
                .1
                .starts_with(work.path()),
            "work tier shadows personal + shipped"
        );

        // personal shadows shipped when work has no copy
        mk(personal.path(), "commit-notes");
        mk(shipped.path(), "commit-notes");
        assert!(job_script(&c, None, "commit-notes")
            .unwrap()
            .1
            .starts_with(personal.path()));
    }

    #[test]
    fn branch_job_shadows_repo_and_global() {
        let _g = crate::test_support::env_lock();
        let global = tempfile::tempdir().unwrap();
        let _jobs = crate::test_support::EnvGuard::set("BN_JOBS_DIR", global.path());

        let notes = tempfile::tempdir().unwrap();
        let c = Config {
            personal_root: notes.path().to_path_buf(),
            work_root: notes.path().to_path_buf(),
            worktree_dir: PathBuf::from("/none"),
            hostname: "host".into(),
            ..Default::default()
        };
        let ctx = Resolved {
            repo: "bn".into(),
            branch: "feat-x".into(),
        };

        // same job name at branch and repo tiers; branch must win
        let branch_jobs = notes
            .path()
            .join("host")
            .join("bn")
            .join("feat-x")
            .join("jobs");
        let repo_jobs = notes
            .path()
            .join("host")
            .join("bn")
            .join("main")
            .join("jobs");
        std::fs::create_dir_all(&branch_jobs).unwrap();
        std::fs::create_dir_all(&repo_jobs).unwrap();
        std::fs::write(branch_jobs.join("check.sh"), "#!/bin/sh\ntrue\n").unwrap();
        std::fs::write(repo_jobs.join("check.sh"), "#!/bin/sh\ntrue\n").unwrap();
        std::fs::write(repo_jobs.join("build.sh"), "#!/bin/sh\ntrue\n").unwrap();

        let (scope, _) = job_script(&c, Some(&ctx), "check").unwrap();
        assert_eq!(scope, JobScope::Branch, "branch tier shadows repo");
        let (scope, _) = job_script(&c, Some(&ctx), "build").unwrap();
        assert_eq!(scope, JobScope::Repo);

        let jobs = list_jobs(&c, Some(&ctx));
        // branch: check; repo: build, check; global: none
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].scope, JobScope::Branch);
    }

    #[test]
    fn work_repos_parse_strips_comments_and_whitespace() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        // comment line, inline comment, intra-line whitespace removed, blank skipped.
        writeln!(f, "# header\nbn\nmy repo # note\n\n  spaced  ").unwrap();
        let got = parse_work_repos(f.path());
        assert_eq!(got, vec!["bn", "myrepo", "spaced"]);
    }

    #[test]
    fn missing_work_repos_file_is_empty() {
        assert!(parse_work_repos(Path::new("/no/such/file")).is_empty());
    }

    #[test]
    fn notes_dir_for_classifies_work_exact_case() {
        let c = cfg(vec!["acme"]);
        assert_eq!(c.notes_dir_for("acme"), Path::new("/w"));
        assert_eq!(c.notes_dir_for("bn"), Path::new("/p"));
        // case-sensitive: uppercase entry would never match a lowercased repo.
        assert_eq!(c.notes_dir_for("ACME"), Path::new("/p"));
    }

    #[test]
    fn work_only_hides_non_work_repos_and_personal_root() {
        let mut c = cfg(vec!["acme"]);
        // Personal machine (default): everything is visible, both roots scanned.
        assert!(c.is_visible_repo("acme"));
        assert!(c.is_visible_repo("personal-side-project"));

        c.work_only = true;
        // Work machine: only work repos surface…
        assert!(c.is_visible_repo("acme"));
        assert!(c.is_visible_repo("m365/acme")); // gh owner/repo basename matches
        assert!(!c.is_visible_repo("personal-side-project"));
        // …and the personal root is never even scanned (only /w exists in this cfg anyway).
        assert_eq!(c.each_notes_root(), Vec::<&Path>::new()); // neither /p nor /w exists on disk
    }

    #[test]
    fn config_file_supplies_roots_and_repos() {
        let cf: ConfigFile = toml::from_str(
            r#"
            worktree_dir = "/wt"
            [personal]
            notes_root = "/p/notes"
            [work]
            notes_root = "/w/notes"
            work_repos = ["acme", "internal"]
            "#,
        )
        .unwrap();

        let _g = crate::test_support::env_lock();
        // Clear the env layer so the file values are what we observe.
        let _notes = crate::test_support::EnvGuard::remove("BN_NOTES_DIR");
        let _notes_work = crate::test_support::EnvGuard::remove("BN_NOTES_DIR_WORK");
        let _worktree = crate::test_support::EnvGuard::remove("BN_WORKTREE_DIR");
        let _work_repos = crate::test_support::EnvGuard::remove("BN_WORK_REPOS_FILE");
        let c = Config::resolve(Some(&cf));
        assert_eq!(c.personal_root, PathBuf::from("/p/notes"));
        assert_eq!(c.work_root, PathBuf::from("/w/notes"));
        assert_eq!(c.worktree_dir, PathBuf::from("/wt"));
        assert_eq!(
            c.work_repos,
            vec!["acme".to_string(), "internal".to_string()]
        );
    }

    #[test]
    fn env_overrides_config_file() {
        let cf: ConfigFile = toml::from_str(
            r#"
            worktree_dir = "/wt-file"
            [personal]
            notes_root = "/p-file"
            "#,
        )
        .unwrap();

        let _g = crate::test_support::env_lock();
        let _notes = crate::test_support::EnvGuard::set("BN_NOTES_DIR", "/p-env");
        let _worktree = crate::test_support::EnvGuard::set("BN_WORKTREE_DIR", "/wt-env");
        let c = Config::resolve(Some(&cf));
        // BN_NOTES_DIR collapses personal+work and outranks the file's [personal].notes_root.
        assert_eq!(c.personal_root, PathBuf::from("/p-env"));
        assert_eq!(c.worktree_dir, PathBuf::from("/wt-env"));
    }

    #[test]
    fn web_config_defaults_file_and_env() {
        let _g = crate::test_support::env_lock();
        let _port = crate::test_support::EnvGuard::remove("BN_WEB_PORT");
        let _bind = crate::test_support::EnvGuard::remove("BN_WEB_BIND");
        let _poll = crate::test_support::EnvGuard::remove("BN_WEB_FLEET_POLL_SECS");
        let _autostart = crate::test_support::EnvGuard::remove("BN_WEB_TMUX_MCP_AUTOSTART");
        let _badge = crate::test_support::EnvGuard::remove("BN_WEB_TMUX_MCP_BADGE");

        // Nothing configured → loopback / 7770 / 2s, with tmux integration enabled.
        let d = Config::resolve(None);
        assert_eq!(d.web.port, 7770);
        assert_eq!(d.web.bind, "127.0.0.1");
        assert_eq!(d.web.fleet_poll_secs, 2);
        assert!(d.web.tmux_mcp_autostart);
        assert!(d.web.tmux_mcp_badge);

        // config.toml [web] values apply.
        let cf: ConfigFile = toml::from_str(
            r#"
            [web]
            port = 8080
            bind = "0.0.0.0"
            fleet_poll_secs = 5
            tmux_mcp_autostart = false
            tmux_mcp_badge = false
            "#,
        )
        .unwrap();
        let c = Config::resolve(Some(&cf));
        assert_eq!(c.web.port, 8080);
        assert_eq!(c.web.bind, "0.0.0.0");
        assert_eq!(c.web.fleet_poll_secs, 5);
        assert!(!c.web.tmux_mcp_autostart);
        assert!(!c.web.tmux_mcp_badge);

        // Env overrides the file, per-key.
        let _penv = crate::test_support::EnvGuard::set("BN_WEB_PORT", "9999");
        let _aenv = crate::test_support::EnvGuard::set("BN_WEB_TMUX_MCP_AUTOSTART", "true");
        let _benv = crate::test_support::EnvGuard::set("BN_WEB_TMUX_MCP_BADGE", "1");
        let e = Config::resolve(Some(&cf));
        assert_eq!(e.web.port, 9999);
        assert_eq!(e.web.bind, "0.0.0.0", "bind still from the file");
        assert!(e.web.tmux_mcp_autostart);
        assert!(e.web.tmux_mcp_badge);
    }

    #[test]
    fn dispatch_plan_review_defaults_file_and_env() {
        let _g = crate::test_support::env_lock();
        let _env = crate::test_support::EnvGuard::remove("BN_DISPATCH_PLAN_REVIEW");
        let _reviewers = crate::test_support::EnvGuard::remove("BN_DISPATCH_PLAN_REVIEWERS");
        let _quorum = crate::test_support::EnvGuard::remove("BN_DISPATCH_PLAN_REVIEW_QUORUM");

        // Nothing configured → the gate is off by default.
        let defaults = Config::resolve(None);
        assert!(!defaults.dispatch_plan_review);
        assert_eq!(defaults.dispatch_plan_reviewers, 3);
        assert_eq!(defaults.dispatch_plan_review_quorum, 2);

        // Config can tune both K and its approval threshold.
        let cf: ConfigFile = toml::from_str(
            "[dispatch]\nplan_review = true\nplan_reviewers = 4\nplan_review_quorum = 3\n",
        )
        .unwrap();
        let configured = Config::resolve(Some(&cf));
        assert!(configured.dispatch_plan_review);
        assert_eq!(configured.dispatch_plan_reviewers, 4);
        assert_eq!(configured.dispatch_plan_review_quorum, 3);

        // Env overrides the file (force off even when the file turns it on).
        let _e = crate::test_support::EnvGuard::set("BN_DISPATCH_PLAN_REVIEW", "0");
        let _r = crate::test_support::EnvGuard::set("BN_DISPATCH_PLAN_REVIEWERS", "2");
        let _q = crate::test_support::EnvGuard::set("BN_DISPATCH_PLAN_REVIEW_QUORUM", "9");
        let env = Config::resolve(Some(&cf));
        assert!(!env.dispatch_plan_review);
        assert_eq!(env.dispatch_plan_reviewers, 2);
        assert_eq!(env.dispatch_plan_review_quorum, 2, "quorum clamps to K");
    }

    #[test]
    fn note_dir_path_formula() {
        let c = cfg(vec!["acme"]);
        let ctx = Resolved {
            repo: "bn".into(),
            branch: "main".into(),
        };
        assert_eq!(c.note_dir(&ctx), PathBuf::from("/p/host/bn/main"));
        assert_eq!(c.note_file(&ctx), PathBuf::from("/p/host/bn/main/note.md"));
        let wctx = Resolved {
            repo: "acme".into(),
            branch: "feature-x".into(),
        };
        assert_eq!(c.note_dir(&wctx), PathBuf::from("/w/host/acme/feature-x"));
    }

    #[test]
    fn inbox_file_is_machine_scoped() {
        let c = cfg(vec![]);
        assert_eq!(c.inbox_file(), PathBuf::from("/p/host/inbox.md"));
    }

    // ---- resolve_hint (git-free capture context) ----

    /// A hint config whose worktree_dir won't accidentally prefix-match the tempdir.
    fn hint_cfg() -> Config {
        let mut c = cfg(vec![]);
        c.worktree_dir = PathBuf::from("/nonexistent-worktree-arena");
        c
    }

    fn write_file(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn hint_plain_clone_reads_head_without_git() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("MyRepo");
        write_file(&top.join(".git/HEAD"), "ref: refs/heads/feature/x\n");
        let sub = top.join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        // Resolves from a subdirectory, normalizes repo case and branch slashes.
        let r = resolve_hint(&hint_cfg(), &sub).unwrap();
        assert_eq!(r.repo, "myrepo");
        assert_eq!(r.branch, "feature-x");
    }

    #[test]
    fn hint_bare_backed_worktree_names_repo_from_common_dir() {
        let dir = tempfile::tempdir().unwrap();
        let gitdir = dir.path().join("arena.git/worktrees/wt1");
        write_file(&gitdir.join("HEAD"), "ref: refs/heads/main\n");
        let wt = dir.path().join("elsewhere/wt1");
        write_file(&wt.join(".git"), &format!("gitdir: {}\n", gitdir.display()));
        let r = resolve_hint(&hint_cfg(), &wt).unwrap();
        // Repo comes from the bare common dir, not the worktree's directory name.
        assert_eq!(r.repo, "arena");
        assert_eq!(r.branch, "main");
    }

    #[test]
    fn hint_detached_head_or_non_repo_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_hint(&hint_cfg(), dir.path()), None);
        let top = dir.path().join("repo");
        write_file(
            &top.join(".git/HEAD"),
            "b6589fc6ab0dc82cf12099d1c2d40ab994e8410c\n",
        );
        assert_eq!(resolve_hint(&hint_cfg(), &top), None);
    }

    #[test]
    fn repo_branch_aggregate_borrows_and_round_trips() {
        let r = Resolved {
            repo: "bn".to_string(),
            branch: "feat/x".to_string(),
        };
        let rb = r.rb();
        assert_eq!(rb.repo, "bn");
        assert_eq!(rb.branch, "feat/x");
        // Directly constructed view equals the one borrowed from `Resolved`.
        assert_eq!(rb, RepoBranch::new("bn", "feat/x"));
    }
}
