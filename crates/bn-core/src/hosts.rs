//! Host registry for native SSH (`bn connect`). `~/.ssh/config` is the
//! source of truth for connection details; the config.toml `[hosts]` table only adds what ssh
//! config can't express (where worktrees live on that box, the default agent harness, whether
//! `bn` is installed there). Resolution: enumerate `Host` stanzas (wildcards and `Match` blocks
//! skipped), left-join `[hosts]` by alias, filter out localhost.

use std::collections::HashMap;
use std::path::Path;

/// A `[hosts.<alias>]` entry from config.toml — the enrichment ssh config can't carry.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct HostExtra {
    /// Where worktrees live on that box (the remote's `worktree_dir`).
    pub repo_root: Option<String>,
    /// Default agent harness when dispatching there (`claude` / `copilot`).
    pub harness: Option<String>,
    /// Whether `bn` is installed on the target. `false` = connect works but dispatch/preview
    /// degrade; unset = unknown (probed at dispatch time).
    pub bn: Option<bool>,
}

/// A connectable host: an ssh-config alias plus its `[hosts]` enrichment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Host {
    /// The `Host` alias as typed after `ssh ` — the join key for `[hosts]`.
    pub alias: String,
    /// `HostName` from the stanza (what ssh actually dials), if set.
    pub hostname: Option<String>,
    /// `User` from the stanza, if set.
    pub user: Option<String>,
    /// `Port` from the stanza, if set and numeric.
    pub port: Option<u16>,
    /// Whether the alias has a `[hosts.<alias>]` entry — the trust registration for managed peers.
    pub registered: bool,
    pub extra: HostExtra,
}

impl Host {
    /// `user@hostname[:port]` — the connection target ssh resolves the alias to. Falls back to
    /// the alias itself when the stanza sets no `HostName`.
    pub fn target(&self) -> String {
        let name = self.hostname.as_deref().unwrap_or(&self.alias);
        let base = match &self.user {
            Some(u) => format!("{u}@{name}"),
            None => name.to_string(),
        };
        match self.port {
            Some(p) => format!("{base}:{p}"),
            None => base,
        }
    }
}

/// A node's lifecycle in the registry (#717). Only [`Ready`](NodeStatus::Ready) is eligible for
/// placement; every other state (and an unheard-from / freshly-stubbed node) is filtered out by
/// the pure scheduler before it can be chosen. `Unknown` is the default so a record with no live
/// heartbeat yet is never treated as placeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeStatus {
    /// Heartbeating and accepting work — the only placement-eligible state.
    Ready,
    /// Reachable but shedding load; no new work should be placed here.
    Draining,
    /// Not reachable / heartbeat lapsed.
    Offline,
    /// No heartbeat observed yet (freshly stubbed) — the default.
    #[default]
    Unknown,
}

/// A worker node in the execution registry (#717, M1 foundation) — the identity + capacity record
/// dispatch will place work against once [`crate::backlog::ExecutionTarget::SshHost`] targeting is
/// wired end-to-end. This is the **stub shape only**: the fields are persisted and the pure
/// scheduler filters on [`status`](NodeRecord::status), but no live capacity is fetched yet (that's
/// a later slice — #719). `ram_reserved_mb` is what the daemon has already committed to in-flight
/// agents on that node; `ram_total_mb - ram_reserved_mb` is the headroom a future capacity-aware
/// pick will weigh. `labels` (e.g. `wsl-box`, `fedora-box`) let a target select a *class* of node.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct NodeRecord {
    /// Stable node id — the `~/.ssh/config` alias the [`crate::backlog::ExecutionTarget::SshHost`]
    /// target names, so placement resolves a target to a concrete registry record.
    pub node_id: String,
    /// **Where this node executes** (#729): the [`crate::backlog::ExecutionTarget`] dispatch places
    /// a picked item onto — [`Local`](crate::backlog::ExecutionTarget::Local) for the box the daemon
    /// runs on (e.g. `wsl-box`), or [`SshHost`](crate::backlog::ExecutionTarget::SshHost) for a
    /// remote worker (e.g. `fedora-box` → `ssh:fedora`). Defaults to `Local` so a pre-#729 record
    /// (which carried no target) reads as the implicit local box.
    #[serde(
        default,
        skip_serializing_if = "crate::backlog::ExecutionTarget::is_local"
    )]
    pub target: crate::backlog::ExecutionTarget,
    /// Free-form capability labels (e.g. `os:linux`, `mt5-bridge`, `has-node`) placement matches a
    /// repo's required capabilities against (#728).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Total RAM on the node in MB (0 until a capacity probe fills it — stub).
    #[serde(default)]
    pub ram_total_mb: u64,
    /// RAM already committed to in-flight agents on the node in MB (0 = idle / unknown).
    #[serde(default)]
    pub ram_reserved_mb: u64,
    /// Repositories this node owns and may execute. Empty preserves the pre-ownership behavior:
    /// the node accepts any repo that passes its capability policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owned_repos: Vec<String>,
    /// Last worker-status capacity snapshot. `None` means the signal is unknown and resource
    /// guards fail open, matching local observation semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<crate::worker::capacity::Capacity>,
    /// Active worker attempts reported by the node. The current worker admission policy is
    /// single-slot, so a non-zero count makes the node busy for automatic placement.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub active_attempts: usize,
    /// Epoch secs of the last heartbeat (0 = never heard from).
    #[serde(default)]
    pub last_heartbeat: i64,
    /// The node's lifecycle state; only [`NodeStatus::Ready`] is placement-eligible.
    #[serde(default)]
    pub status: NodeStatus,
    /// How many agents the **scheduler** may run here at once.
    ///
    /// Distinct from `worker::registry::MAX_SLOTS`, which is the node's own admission guard and is
    /// deliberately 1 for an unmonitored box. This is the control plane's view: a 64 GB machine can
    /// hold three agents, and without a way to say so the scheduler would leave it mostly idle.
    /// Defaults to 1 — the pre-queue behaviour.
    #[serde(default = "one_slot")]
    pub slots: usize,
}

/// Serde default for [`NodeRecord::slots`].
fn one_slot() -> usize {
    1
}

/// How long a node may go without a heartbeat before it is no longer trusted, in seconds.
pub const DEFAULT_HEARTBEAT_TTL_SECS: i64 = 300;

/// Whether `node`'s heartbeat has lapsed as of `now`.
///
/// A node that heartbeated an hour ago is not Ready no matter what its record still says, and the
/// registry has no other way to notice: `last_heartbeat` was written and then never read, so a box
/// that died kept receiving work until someone looked. A node that has *never* heartbeated
/// (`last_heartbeat == 0`) is not stale — it is merely unproven, which `NodeStatus::Unknown`
/// already says, and treating it as stale would mean re-reporting the same fact under a worse name.
pub fn is_stale(node: &NodeRecord, now: i64, ttl_secs: i64) -> bool {
    node.last_heartbeat > 0 && now.saturating_sub(node.last_heartbeat) > ttl_secs
}

/// Fold a `bn worker status --json` payload into `node`.
///
/// Pure, and in bn-core rather than the CLI, because what it decides is a policy question: whether
/// a reachable node is *placeable*. The previous version marked any node that answered at all as
/// `Ready`, ignored the `PRESSURE` verdict the node had just computed about itself, dropped the
/// advertised `labels` on the floor — so a remote node's skills could never reach placement — and
/// never wrote `last_heartbeat`, which is why staleness could not be detected.
pub fn apply_snapshot(node: &mut NodeRecord, status: Option<&serde_json::Value>, now: i64) {
    let Some(status) = status else {
        node.status = NodeStatus::Offline;
        node.capacity = None;
        node.active_attempts = 0;
        return;
    };
    node.last_heartbeat = now;
    if let Some(labels) = status.pointer("/node/labels").and_then(|v| v.as_array()) {
        // The node's own advertisement wins over the config's, then the operator's declared labels
        // are merged back so a `[nodes.<id>].labels` entry can still add a tag the box cannot know
        // about itself (a physical attachment, a network zone).
        let mut merged: Vec<String> = labels
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        merged.append(&mut node.labels);
        merged.sort();
        merged.dedup();
        node.labels = merged;
    }
    node.capacity = status
        .get("capacity")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    node.active_attempts = status
        .get("attempts")
        .and_then(serde_json::Value::as_array)
        .map(|attempts| {
            attempts
                .iter()
                .filter(|a| {
                    matches!(
                        a.get("state").and_then(serde_json::Value::as_str),
                        Some("QUEUED" | "RUNNING")
                    )
                })
                .count()
        })
        .unwrap_or_default();
    // Honour the node's own verdict: a box reporting PRESSURE has told us it is shedding load, and
    // overriding that with Ready because it answered the phone is how you thrash a struggling
    // machine.
    node.status = match node.capacity.map(|c| c.status()) {
        Some(crate::worker::capacity::NodeStatus::Ready) | None => NodeStatus::Ready,
        Some(crate::worker::capacity::NodeStatus::Pressure) => NodeStatus::Draining,
        Some(crate::worker::capacity::NodeStatus::Dead) => NodeStatus::Offline,
    };
}

/// The synthesized registry record for the box this process runs on.
///
/// The local box has always been an **implicit** placement target with no record, which meant it
/// advertised no capabilities and could therefore never satisfy a required one. That was harmless
/// while requirements were exotic hardware tags nobody used; it is fatal once every entry can
/// require a skill, because on a single-machine setup — i.e. most of them — the registry is empty
/// and *every* constrained entry would be unplaceable forever.
///
/// Synthesizing a record instead makes the local box a first-class registry entry that competes on
/// its real advertised capabilities, and makes a "no capable node" verdict mean what it says.
///
/// Merged **over** any `[nodes.<machine>]` the operator declared, so explicit config still wins on
/// `owned_repos`, extra labels and the RAM budget.
pub fn local_record(
    cfg: &crate::resolver::Config,
    labels: Vec<String>,
    worktree_root: &std::path::Path,
    now: i64,
) -> NodeRecord {
    let machine = cfg.machine();
    let declared = cfg
        .nodes
        .iter()
        .find(|n| n.node_id.eq_ignore_ascii_case(machine));
    let mut labels = labels;
    if let Some(declared) = declared {
        labels.extend(declared.labels.iter().cloned());
        labels.sort();
        labels.dedup();
    }
    NodeRecord {
        node_id: machine.to_string(),
        target: crate::backlog::ExecutionTarget::Local,
        labels,
        ram_total_mb: declared.map(|n| n.ram_total_mb).unwrap_or_default(),
        ram_reserved_mb: declared.map(|n| n.ram_reserved_mb).unwrap_or_default(),
        owned_repos: declared.map(|n| n.owned_repos.clone()).unwrap_or_default(),
        capacity: Some(crate::worker::capacity::sample(worktree_root)),
        active_attempts: 0,
        last_heartbeat: now,
        // The box running this code is by definition reachable, so it needs no preflight to be
        // Ready — unlike a remote node, whose `Ready` must be earned by a successful probe.
        status: NodeStatus::Ready,
        slots: declared.map(|n| n.slots).filter(|s| *s > 0).unwrap_or(1),
    }
}

/// A `[nodes.<node_id>]` entry from config.toml — the declarative worker list (#729). This is the
/// *operator-declared* shape of a node (its execution target, owned repos, capability labels, and
/// RAM budget);
/// [`into_record`](NodeConfig::into_record) turns it into the runtime [`NodeRecord`] dispatch
/// places against. `status`/`last_heartbeat` are intentionally not configurable: they're liveness
/// state the daemon's preflight fills in (a freshly-loaded node starts [`NodeStatus::Unknown`],
/// never `Ready`, so it can't be placed on until a preflight confirms it).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct NodeConfig {
    /// The node's [`crate::backlog::ExecutionTarget`] label (`local`, `ssh:<alias>`,
    /// `github-runner`). Absent/blank ⇒ `local` (the box the daemon runs on).
    pub target: Option<String>,
    /// Capability labels (e.g. `os:linux`, `mt5-bridge`, `has-node`) placement matches a repo's
    /// required capabilities against (#728).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Total RAM on the node in MB (0 = unknown).
    #[serde(default)]
    pub ram_total_mb: u64,
    /// RAM the operator reserves as always-committed headroom on the node in MB (0 = none).
    #[serde(default)]
    pub ram_reserved_mb: u64,
    /// Repositories this node owns. Empty means unrestricted for backward compatibility.
    #[serde(default)]
    pub repos: Vec<String>,
    /// How many agents the scheduler may run here at once. Defaults to 1.
    #[serde(default = "one_slot")]
    pub slots: usize,
}

impl NodeConfig {
    /// Resolve this config entry into a runtime [`NodeRecord`] under `node_id`. The `target`
    /// string is parsed via [`crate::backlog::ExecutionTarget::parse`]; an unrecognized/blank
    /// value falls back to [`crate::backlog::ExecutionTarget::Local`]. `status` starts
    /// [`NodeStatus::Unknown`] — never placement-eligible until a live preflight promotes it.
    pub fn into_record(self, node_id: &str) -> NodeRecord {
        let target = self
            .target
            .as_deref()
            .and_then(crate::backlog::ExecutionTarget::parse)
            .unwrap_or_default();
        NodeRecord {
            node_id: node_id.to_string(),
            target,
            labels: self.labels,
            ram_total_mb: self.ram_total_mb,
            ram_reserved_mb: self.ram_reserved_mb,
            owned_repos: self.repos,
            capacity: None,
            active_attempts: 0,
            last_heartbeat: 0,
            status: NodeStatus::Unknown,
            slots: self.slots.max(1),
        }
    }
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Resolve the declarative `[nodes]` table (`node_id → `[`NodeConfig`]`) into the runtime registry,
/// sorted by `node_id` for a stable, deterministic first-fit placement order (#729). An empty map
/// yields an empty registry — the pre-#729 single-node behavior (placement then always lands
/// `Local`).
pub fn resolve_nodes(nodes: &HashMap<String, NodeConfig>) -> Vec<NodeRecord> {
    let mut records: Vec<NodeRecord> = nodes
        .iter()
        .map(|(id, cfg)| cfg.clone().into_record(id))
        .collect();
    records.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    records
}

/// The lifecycle state a node takes after a **BatchMode key-auth preflight** (#729). A node whose
/// `ssh -o BatchMode=yes` probe succeeded (reachable *and* key auth works — no password prompt) is
/// [`Ready`](NodeStatus::Ready) and placement-eligible; a probe that failed (unreachable, or
/// password-only auth the unattended daemon can't answer) marks it [`Offline`](NodeStatus::Offline)
/// so the scheduler never places on a node it can't actually reach. Pure so the mapping is
/// unit-testable; the CLI supplies the live probe result.
pub fn node_status_from_preflight(reachable_with_key_auth: bool) -> NodeStatus {
    if reachable_with_key_auth {
        NodeStatus::Ready
    } else {
        NodeStatus::Offline
    }
}

/// ssh's first-obtained-value semantics.
pub fn parse_ssh_config(text: &str) -> Vec<Host> {
    let mut hosts: Vec<Host> = Vec::new();
    // Indices into `hosts` for the stanza currently being filled; empty while inside a
    // `Match` block or a stanza whose aliases were all wildcards.
    let mut current: Vec<usize> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = split_directive(line) else {
            continue;
        };
        if key.eq_ignore_ascii_case("host") {
            current.clear();
            for alias in value.split_whitespace() {
                if alias.contains(['*', '?']) || alias.starts_with('!') {
                    continue;
                }
                current.push(hosts.len());
                hosts.push(Host {
                    alias: alias.to_string(),
                    ..Host::default()
                });
            }
            continue;
        }
        if key.eq_ignore_ascii_case("match") {
            current.clear();
            continue;
        }
        for &i in &current {
            let h = &mut hosts[i];
            if key.eq_ignore_ascii_case("hostname") {
                if h.hostname.is_none() {
                    h.hostname = Some(value.to_string());
                }
            } else if key.eq_ignore_ascii_case("user") {
                if h.user.is_none() {
                    h.user = Some(value.to_string());
                }
            } else if key.eq_ignore_ascii_case("port") && h.port.is_none() {
                h.port = value.parse().ok();
            }
        }
    }
    hosts
}

/// Split an ssh_config directive into `(keyword, value)`. Both `Key value` and `Key=value`
/// forms are accepted; returns `None` for a keyword with no value.
fn split_directive(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(|c: char| c.is_whitespace() || c == '=')?;
    let value = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    if key.is_empty() || value.is_empty() {
        None
    } else {
        Some((key, value.trim()))
    }
}

/// Left-join `[hosts]` extras onto the ssh-config hosts and drop localhost entries (connecting
/// to the machine you're on is never the point). ssh config is the left side: a host present
/// only there still appears (unregistered, default extras); a `[hosts]` entry with no matching
/// ssh stanza is dropped — connection details live in ssh config by decision.
pub fn merge(ssh_hosts: Vec<Host>, extras: &HashMap<String, HostExtra>) -> Vec<Host> {
    ssh_hosts
        .into_iter()
        .filter(|h| !is_localhost(h))
        .map(|mut h| {
            if let Some(e) = extras.get(&h.alias) {
                h.registered = true;
                h.extra = e.clone();
            }
            h
        })
        .collect()
}

fn is_localhost(h: &Host) -> bool {
    let localhost = |s: &str| s.eq_ignore_ascii_case("localhost") || s == "127.0.0.1" || s == "::1";
    localhost(&h.alias) || h.hostname.as_deref().is_some_and(localhost)
}

/// Production entry: parse `<ssh_dir>/config` (absent file ⇒ no ssh hosts) and merge the
/// `[hosts]` extras. Callers pass `~/.ssh` so tests can point elsewhere.
pub fn load(ssh_dir: &Path, extras: &HashMap<String, HostExtra>) -> Vec<Host> {
    let text = std::fs::read_to_string(ssh_dir.join("config")).unwrap_or_default();
    merge(parse_ssh_config(&text), extras)
}
