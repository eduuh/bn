//! The config deserialization layer: [`ConfigFile`], one parse of every config layer merged
//! together, and its accessors. Internal to the resolver — [`super::Config`] /
//! [`super::JobConfig`] consume it and expose the resolved values.

use std::collections::HashMap;
use std::path::Path;

/// One config layer as a raw table. `None` when the file is absent, unreadable or not valid TOML;
/// the last two are reported, because a typo that silently changes nothing is exactly the failure
/// a config resolver must not have.
fn read_layer(path: &Path) -> Option<toml::Table> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("bn: could not read {}: {error}", path.display());
            return None;
        }
    };
    match text.parse::<toml::Table>() {
        Ok(table) => Some(table),
        Err(e) => {
            eprintln!("bn: ignoring malformed {}: {e}", path.display());
            None
        }
    }
}

/// Merge `over` into `base`: tables merge key by key, anything else is replaced whole. An array is
/// one value, not a set to union — a layer that says `repos = ["bn"]` means exactly that.
pub(crate) fn merge_tables(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match value {
            toml::Value::Table(over_child) => match base.get_mut(&key) {
                Some(toml::Value::Table(base_child)) => merge_tables(base_child, over_child),
                _ => {
                    base.insert(key, toml::Value::Table(over_child));
                }
            },
            value => {
                base.insert(key, value);
            }
        }
    }
}

/// The settings bn reads, all optional: the shared `<notes_root>/config.toml` with this machine's
/// `<notes_root>/<machine>/config.toml` merged on top. `crate::config_migrate::KNOWN_SECTIONS`
/// lists these field names, and a test keeps the two equal.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct ConfigFile {
    work: Option<WorkSection>,
    jobs: Option<JobsSection>,
    review: Option<ReviewSection>,
    harness: Option<HarnessSection>,
    attention: Option<AttentionSection>,
    calib: Option<CalibSection>,
    capture: Option<CaptureSection>,
    wrap: Option<WrapSection>,
    standup: Option<StandupSection>,
    dxi: Option<DxiSection>,
    agents: Option<AgentsSection>,
    /// `[notify]` — the desktop-toast policy gate. Deserialized into the domain's own raw shape
    /// because the clamping and the `bar` vocabulary belong next to the policy that reads them.
    /// See [`crate::notify::policy`].
    notify: Option<crate::notify::policy::NotifyFileConfig>,
    staff: Option<StaffSection>,
    mcp: Option<McpSection>,
    managed_repos: Option<ManagedReposSection>,
    /// `[tasks]`: which backend holds the task list. See [`crate::task_store`].
    tasks: Option<TasksSection>,
    tools: Option<ToolsSection>,
    links: Option<LinksSection>,
    web: Option<WebSection>,
    log: Option<LogSection>,
    scout: Option<ScoutSection>,
    dispatch: Option<DispatchSection>,
    pipeline: Option<super::PipelineConfig>,
    orchestrate: Option<OrchestrateSection>,
    /// `[machine]`: what this machine should do less of. Absent, it is allowed everything.
    machine: Option<MachineProfile>,
    /// `[hosts.<alias>]`: ssh-host enrichment for `bn connect`. Keyed by
    /// the ssh-config `Host` alias; connection details stay in `~/.ssh/config`.
    pub(super) hosts: Option<HashMap<String, crate::hosts::HostExtra>>,
    /// `[nodes.<node_id>]` (#729): the declarative multi-node worker list — each node's execution
    /// target, capability labels, and RAM budget. Resolved to a [`crate::hosts::NodeRecord`]
    /// registry the orchestrate scheduler places work across. Empty/absent ⇒ single-node behavior.
    pub(super) nodes: Option<HashMap<String, crate::hosts::NodeConfig>>,
    /// `[repo.<name>]`: per-repo verify, pipeline, and node-capability placement policy. Keyed by
    /// the repo's bn/local name; matched case-insensitively on the trailing path segment at use.
    pub(super) repo: Option<HashMap<String, super::RepoConfig>>,
}

use super::config_sections::*;

impl ConfigFile {
    /// Read every config layer and merge them into one parse, a later layer winning key by key.
    /// `None` when no layer exists. A layer that cannot be read or parsed is reported and skipped,
    /// so resolution falls back to the defaults for whatever it would have set — and the defaults
    /// switch nothing off.
    pub(super) fn load() -> Option<ConfigFile> {
        let mut merged: Option<toml::Table> = None;
        let mut read = Vec::new();
        for path in super::dirs::config_layer_paths() {
            let Some(layer) = read_layer(&path) else {
                continue;
            };
            read.push(path);
            match merged.as_mut() {
                Some(base) => merge_tables(base, layer),
                None => merged = Some(layer),
            }
        }
        match toml::Value::Table(merged?).try_into::<ConfigFile>() {
            Ok(cf) => Some(cf),
            Err(e) => {
                let paths: Vec<String> = read.iter().map(|p| p.display().to_string()).collect();
                eprintln!("bn: ignoring malformed config ({}): {e}", paths.join(", "));
                None
            }
        }
    }

    pub(super) fn machine_profile(&self) -> Option<MachineProfile> {
        self.machine.clone()
    }

    /// `[tasks].provider` — the raw word, parsed by
    /// [`crate::task_store::TaskProviderKind::parse`].
    pub(super) fn tasks_provider(&self) -> Option<String> {
        self.tasks.as_ref().and_then(|t| t.provider.clone())
    }

    /// `[tasks].forge_url` — the Forge MCP endpoint, raw.
    pub(super) fn tasks_forge_url(&self) -> Option<String> {
        self.tasks.as_ref().and_then(|t| t.forge_url.clone())
    }

    pub(super) fn work_repos(&self) -> Option<Vec<String>> {
        self.work.as_ref().and_then(|w| w.work_repos.clone())
    }

    /// `[managed_repos].repos`: the clone URLs bn keeps checked out under the managed root.
    pub(super) fn managed_repos(&self) -> Option<Vec<String>> {
        self.managed_repos.as_ref().and_then(|m| m.repos.clone())
    }

    pub(super) fn tools_builtins(&self) -> Option<Vec<String>> {
        self.tools.as_ref().and_then(|t| t.builtins.clone())
    }

    /// `[links].tags` — the curated tag vocabulary for `bn links`. `None` when unset.
    pub(super) fn links_tags(&self) -> Option<Vec<String>> {
        self.links.as_ref().and_then(|l| l.tags.clone())
    }

    /// `[repo.<name>]` (#454): per-repo overrides. `None` when unset.
    pub(super) fn repos(&self) -> Option<HashMap<String, super::RepoConfig>> {
        self.repo.clone()
    }

    /// `[nodes.<node_id>]` (#729): the declarative multi-node worker list. `None` when unset.
    pub(super) fn nodes(&self) -> Option<HashMap<String, crate::hosts::NodeConfig>> {
        self.nodes.clone()
    }

    /// `[jobs.only_on]`. Empty map when unset.
    pub(super) fn job_only_on(&self) -> HashMap<String, Vec<String>> {
        self.jobs
            .as_ref()
            .and_then(|j| j.only_on.clone())
            .unwrap_or_default()
    }

    pub(super) fn weekly_anchor(&self) -> Option<String> {
        self.review.as_ref().and_then(|r| r.weekly_anchor.clone())
    }

    pub(super) fn sla_first_review_hours(&self) -> Option<f64> {
        self.review.as_ref().and_then(|r| r.sla_first_review_hours)
    }

    pub(super) fn review_harness(&self) -> Option<String> {
        self.review.as_ref().and_then(|r| r.harness.clone())
    }

    pub(super) fn disabled_harnesses(&self) -> Option<Vec<String>> {
        self.harness.as_ref().and_then(|h| h.disabled.clone())
    }

    pub(super) fn default_harness(&self) -> Option<String> {
        self.harness
            .as_ref()
            .and_then(|h| h.default.clone())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub(super) fn attention_max_age_hours(&self) -> Option<f64> {
        self.attention.as_ref().and_then(|a| a.max_age_hours)
    }

    /// The coarse `--est S|M|L` durations. A per-key override that parses to a positive duration
    /// wins; otherwise the built-in default for that size stands, so a partial or malformed
    /// `[calib]` degrades per-key rather than dropping the whole table.
    pub(super) fn coarse_sizes(&self) -> crate::calib::CoarseSizes {
        let d = crate::calib::CoarseSizes::default();
        let pick = |get: &dyn Fn(&CalibSection) -> Option<String>, fallback: i64| -> i64 {
            self.calib
                .as_ref()
                .and_then(get)
                .and_then(|s| crate::focus::parse_duration(&s))
                .filter(|&n| n > 0)
                .unwrap_or(fallback)
        };
        crate::calib::CoarseSizes {
            s: pick(&|c| c.s.clone(), d.s),
            m: pick(&|c| c.m.clone(), d.m),
            l: pick(&|c| c.l.clone(), d.l),
        }
    }

    pub(super) fn screenshot_dir(&self) -> Option<std::path::PathBuf> {
        self.capture
            .as_ref()
            .and_then(|c| c.screenshot_dir.clone())
            .map(std::path::PathBuf::from)
    }

    pub(super) fn gif_dir(&self) -> Option<std::path::PathBuf> {
        self.capture
            .as_ref()
            .and_then(|c| c.gif_dir.clone())
            .map(std::path::PathBuf::from)
    }

    pub(super) fn wrap_enabled(&self) -> Option<bool> {
        self.wrap.as_ref().and_then(|s| s.enabled)
    }

    pub(super) fn standup_enabled(&self) -> Option<bool> {
        self.standup.as_ref().and_then(|s| s.enabled)
    }

    pub(super) fn standup_fetch_delta(&self) -> Option<bool> {
        self.standup.as_ref().and_then(|s| s.fetch_delta)
    }

    pub(super) fn dxi_enabled(&self) -> Option<bool> {
        self.dxi.as_ref().and_then(|s| s.enabled)
    }

    pub(super) fn dispatch_plan_review(&self) -> Option<bool> {
        self.dispatch.as_ref().and_then(|s| s.plan_review)
    }

    pub(super) fn dispatch_plan_reviewers(&self) -> Option<usize> {
        self.dispatch.as_ref().and_then(|s| s.plan_reviewers)
    }

    pub(super) fn dispatch_plan_review_quorum(&self) -> Option<usize> {
        self.dispatch.as_ref().and_then(|s| s.plan_review_quorum)
    }

    pub(super) fn pipeline(&self) -> Option<super::PipelineConfig> {
        self.pipeline.clone()
    }

    pub(super) fn review_diff_gate(&self) -> Option<bool> {
        self.review.as_ref().and_then(|s| s.diff_gate)
    }

    pub(super) fn review_diff_gate_reviews(&self) -> Option<usize> {
        self.review.as_ref().and_then(|s| s.diff_gate_reviews)
    }

    fn orchestrate(&self) -> Option<&OrchestrateSection> {
        self.orchestrate.as_ref()
    }

    pub(super) fn orchestrate_stuck_idle_secs(&self) -> Option<i64> {
        self.orchestrate().and_then(|s| s.stuck_idle_secs)
    }

    pub(super) fn orchestrate_keep_warm(&self) -> Option<bool> {
        self.orchestrate().and_then(|s| s.keep_warm)
    }

    pub(super) fn orchestrate_low_water(&self) -> Option<usize> {
        self.orchestrate().and_then(|s| s.low_water)
    }

    pub(super) fn orchestrate_owned_repos(&self) -> Option<Vec<String>> {
        self.orchestrate().and_then(|s| s.own.clone())
    }

    pub(super) fn orchestrate_exclude_titles(&self) -> Option<Vec<String>> {
        self.orchestrate().and_then(|s| s.exclude_titles.clone())
    }

    pub(super) fn orchestrate_exclude_labels(&self) -> Option<Vec<String>> {
        self.orchestrate().and_then(|s| s.exclude_labels.clone())
    }

    pub(super) fn orchestrate_brainstorm_count(&self) -> Option<usize> {
        self.orchestrate().and_then(|s| s.brainstorm_count)
    }

    pub(super) fn orchestrate_brainstorm_interval_secs(&self) -> Option<i64> {
        self.orchestrate().and_then(|s| s.brainstorm_interval_secs)
    }

    pub(super) fn orchestrate_min_free_ram_mb(&self) -> Option<u64> {
        self.orchestrate().and_then(|s| s.min_free_ram_mb)
    }

    pub(super) fn orchestrate_max_loadavg(&self) -> Option<f64> {
        self.orchestrate().and_then(|s| s.max_loadavg)
    }

    pub(super) fn orchestrate_max_swap_pct(&self) -> Option<f64> {
        self.orchestrate().and_then(|s| s.max_swap_pct)
    }

    pub(super) fn orchestrate_tokens_per_hour(&self) -> Option<u64> {
        self.orchestrate().and_then(|s| s.tokens_per_hour)
    }

    pub(super) fn orchestrate_daily_cost_usd(&self) -> Option<f64> {
        self.orchestrate().and_then(|s| s.daily_cost_usd)
    }

    pub(super) fn orchestrate_daily_cost_usd_coding(&self) -> Option<f64> {
        self.orchestrate().and_then(|s| s.daily_cost_usd_coding)
    }

    pub(super) fn orchestrate_daily_cost_usd_research(&self) -> Option<f64> {
        self.orchestrate().and_then(|s| s.daily_cost_usd_research)
    }

    pub(super) fn orchestrate_remote_overflow(&self) -> Option<bool> {
        self.orchestrate().and_then(|s| s.remote_overflow)
    }

    /// `[agents].enabled` — the opt-in agents switched on.
    pub(super) fn agents_enabled(&self) -> Option<Vec<String>> {
        self.agents.as_ref().and_then(|s| s.enabled.clone())
    }

    /// `[notify]` — the toast policy gate.
    pub(super) fn notify(&self) -> Option<crate::notify::policy::NotifyFileConfig> {
        self.notify.clone()
    }

    /// `[staff].home` — the machine the agent staff runs on (#270). `None` (unset) leaves the staff
    /// running anywhere.
    pub(super) fn staff_home(&self) -> Option<String> {
        self.staff.as_ref().and_then(|s| s.home.clone())
    }

    pub(super) fn mcp_extras(&self) -> Option<Vec<String>> {
        self.mcp.as_ref().and_then(|s| s.extras.clone())
    }

    pub(super) fn web_port(&self) -> Option<u16> {
        self.web.as_ref().and_then(|s| s.port)
    }

    pub(super) fn web_bind(&self) -> Option<String> {
        self.web.as_ref().and_then(|s| s.bind.clone())
    }

    pub(super) fn web_fleet_poll_secs(&self) -> Option<u64> {
        self.web.as_ref().and_then(|s| s.fleet_poll_secs)
    }

    pub(super) fn web_tmux_mcp_autostart(&self) -> Option<bool> {
        self.web.as_ref().and_then(|s| s.tmux_mcp_autostart)
    }

    pub(super) fn web_tmux_mcp_badge(&self) -> Option<bool> {
        self.web.as_ref().and_then(|s| s.tmux_mcp_badge)
    }

    pub(super) fn log_enabled(&self) -> Option<bool> {
        self.log.as_ref().and_then(|s| s.enabled)
    }

    pub(super) fn log_level(&self) -> Option<String> {
        self.log.as_ref().and_then(|s| s.level.clone())
    }

    pub(super) fn log_dir(&self) -> Option<String> {
        self.log.as_ref().and_then(|s| s.dir.clone())
    }

    pub(super) fn log_retain_days(&self) -> Option<i64> {
        self.log.as_ref().and_then(|s| s.retain_days)
    }

    pub(super) fn scout_watch(&self) -> Option<Vec<String>> {
        self.scout.as_ref().and_then(|s| s.watch.clone())
    }

    pub(super) fn scout_tools(&self) -> Option<Vec<String>> {
        self.scout.as_ref().and_then(|s| s.tools.clone())
    }

    pub(super) fn scout_max_findings(&self) -> Option<usize> {
        self.scout.as_ref().and_then(|s| s.max_findings)
    }
}

#[cfg(test)]
mod layer_tests {
    use super::{merge_tables, ConfigFile};

    fn table(text: &str) -> toml::Table {
        text.parse().expect("valid TOML")
    }

    fn get<'a>(table: &'a toml::Table, section: &str, key: &str) -> Option<&'a toml::Value> {
        table.get(section).and_then(|s| s.get(key))
    }

    /// `[tasks] provider` resolves onto `Config`: absent is the file, `"forge"` is the Forge app,
    /// and an unknown word is reported and resolves to the file rather than failing the whole
    /// config.
    #[test]
    fn tasks_provider_resolves_file_forge_and_unknown() {
        use crate::task_store::TaskProviderKind;
        let resolve = |text: &str| {
            let file: ConfigFile = toml::from_str(text).expect("config parses");
            super::super::Config::resolve(Some(&file)).tasks_provider
        };
        assert_eq!(resolve(""), TaskProviderKind::File);
        assert_eq!(resolve("[tasks]\n"), TaskProviderKind::File);
        assert_eq!(
            resolve("[tasks]\nprovider = \"file\"\n"),
            TaskProviderKind::File
        );
        assert_eq!(
            resolve("[tasks]\nprovider = \"forge\"\n"),
            TaskProviderKind::ForgeApp
        );
        assert_eq!(
            resolve("[tasks]\nprovider = \"froge\"\n"),
            TaskProviderKind::File
        );
    }

    /// `[tasks] forge_url` defaults to production and a blank value is the default.
    #[test]
    fn tasks_forge_url_defaults_and_overrides() {
        let resolve = |text: &str| {
            let file: ConfigFile = toml::from_str(text).expect("config parses");
            super::super::Config::resolve(Some(&file)).tasks_forge_url
        };
        let default = crate::forge_app::DEFAULT_FORGE_URL;
        assert_eq!(resolve(""), default);
        assert_eq!(resolve("[tasks]\nforge_url = \"  \"\n"), default);
        assert_eq!(
            resolve("[tasks]\nforge_url = \"http://localhost:5000/mcp\"\n"),
            "http://localhost:5000/mcp"
        );
    }

    /// A later layer overrides only what it names: tables merge key by key, so a machine that sets
    /// `[web] port` keeps the shared `bind`, and sections it never mentions survive untouched.
    #[test]
    fn a_later_layer_wins_key_by_key() {
        let mut base =
            table("[web]\nport = 7770\nbind = \"127.0.0.1\"\n\n[review]\nharness = \"claude\"\n");
        merge_tables(&mut base, table("[web]\nport = 7791\n"));

        assert_eq!(
            get(&base, "web", "port").and_then(toml::Value::as_integer),
            Some(7791)
        );
        assert_eq!(
            get(&base, "web", "bind").and_then(toml::Value::as_str),
            Some("127.0.0.1")
        );
        assert_eq!(
            get(&base, "review", "harness").and_then(toml::Value::as_str),
            Some("claude")
        );
    }

    /// An array is one value: a later `repos = ["bn"]` replaces the list rather than adding to it,
    /// or a machine could never narrow what the shared file allows.
    #[test]
    fn an_array_is_replaced_not_unioned() {
        let mut base = table("[machine]\nrepos = [\"bn\", \"arena\"]\n");
        merge_tables(&mut base, table("[machine]\nrepos = [\"bn\"]\n"));

        assert_eq!(
            get(&base, "machine", "repos")
                .and_then(toml::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    /// A deserializer that answers one question — which field names a derived `Deserialize` struct
    /// asks for — and refuses everything else.
    struct FieldNames(Vec<&'static str>);

    impl<'de> serde::Deserializer<'de> for &mut FieldNames {
        type Error = serde::de::value::Error;

        fn deserialize_any<V: serde::de::Visitor<'de>>(
            self,
            _: V,
        ) -> Result<V::Value, Self::Error> {
            Err(serde::de::Error::custom("only struct field names are read"))
        }

        fn deserialize_struct<V: serde::de::Visitor<'de>>(
            self,
            _name: &'static str,
            fields: &'static [&'static str],
            _visitor: V,
        ) -> Result<V::Value, Self::Error> {
            self.0.extend_from_slice(fields);
            Err(serde::de::Error::custom("field names captured"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
            option unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier
            ignored_any
        }
    }

    /// Every top-level section bn parses, read off the derive itself.
    pub(crate) fn parsed_sections() -> Vec<&'static str> {
        let mut names = FieldNames(Vec::new());
        let _ = <ConfigFile as serde::Deserialize>::deserialize(&mut names);
        names.0.sort_unstable();
        names.0
    }

    /// The migration keeps what bn reads and drops everything else, so its list of sections must be
    /// exactly the sections bn parses: one it missed would be silently thrown away on migration.
    #[test]
    fn the_migration_knows_every_section_bn_parses() {
        let mut known = crate::config_migrate::KNOWN_SECTIONS.to_vec();
        known.sort_unstable();
        assert_eq!(parsed_sections(), known);
    }

    /// With no template to read, docs/config.md is where a setting is found, so every section bn
    /// parses needs an entry there: a new section added without one fails here.
    #[test]
    fn docs_config_documents_every_section_bn_parses() {
        let docs = include_str!("../../../../docs/config.md");
        let missing: Vec<&str> = parsed_sections()
            .into_iter()
            .filter(|section| {
                !docs.contains(&format!("`[{section}]`"))
                    && !docs.contains(&format!("`[{section}."))
            })
            .collect();
        assert!(
            missing.is_empty(),
            "docs/config.md does not document: {missing:?}"
        );
    }
}
