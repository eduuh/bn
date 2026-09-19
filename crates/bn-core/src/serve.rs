//! Pure `bn serve` / bn-web policy (#499): the honest connection-state classifier the dashboard
//! reads, and the managed-service status classifier the `bn serve status` command renders. No IO —
//! callers (the UI health probe, the CLI service probe) feed observations in and get a verdict out,
//! so the escalation rule lives in one tested place. The UI mirrors `classify_conn` in TypeScript
//! (`crates/bn-web/ui/src/lib/conn.ts`); keep [`DOWN_AFTER_FAILURES`] in sync there.

use serde::Serialize;

/// How often (seconds) the dashboard should poll `GET /api/health` *while the SSE stream is down*,
/// to tell a transient blip (server up, stream re-handshaking) apart from a real outage. Mirrored
/// as `HEALTH_PROBE_MS` in the UI.
pub const HEALTH_PROBE_SECS: u64 = 3;

/// Grace before an ongoing reconnect is called an honest outage: this many *consecutive* failed
/// health probes flip the state from `Reconnecting` to `Down`, so a one-off gap never cries wolf
/// but a genuinely dead server stops pretending it's "reconnecting…" forever.
pub const DOWN_AFTER_FAILURES: u32 = 3;

/// The dashboard's honest connection state. `Live` while the SSE stream is open; when it drops we
/// probe `/api/health` and either stay `Reconnecting` (server still answering — a transient blip)
/// or escalate to `Down` (probes failing past the grace — surface it, stop the silent spinner).
/// Serialises to the exact UI strings (`open` / `lost` / `down`) so both ends speak one vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnState {
    /// The live SSE stream is open. UI: `open`.
    #[serde(rename = "open")]
    Live,
    /// The stream dropped but the server still answers health probes (within grace) — a blip.
    /// UI: `lost`.
    #[serde(rename = "lost")]
    Reconnecting,
    /// Health probes have failed past the grace — the server is genuinely down. UI: `down`.
    Down,
}

impl ConnState {
    /// The UI/serde token (`open` / `lost` / `down`).
    pub fn as_str(self) -> &'static str {
        match self {
            ConnState::Live => "open",
            ConnState::Reconnecting => "lost",
            ConnState::Down => "down",
        }
    }
}

/// The managed bn-web service's status, as `bn serve status` reports it. Derived from two cheap
/// probes: whether a managed service is *installed* (launchd plist / systemd unit on disk) and
/// whether *something is listening* on the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeState {
    /// Something is answering on the port — the dashboard is up (managed or a bare foreground run).
    Running,
    /// A managed service is installed but nothing is listening — crashed / mid-restart / wedged.
    /// The honest "server down" the issue asks for, from the operator's side.
    Down,
    /// No managed service and nothing listening — the dashboard isn't running.
    NotInstalled,
}

impl ServeState {
    /// A one-word status token for `--json` and logs (`running` / `down` / `not_installed`).
    pub fn as_str(self) -> &'static str {
        match self {
            ServeState::Running => "running",
            ServeState::Down => "down",
            ServeState::NotInstalled => "not_installed",
        }
    }

    /// A short human sentence for the plain-text `bn serve status` line.
    pub fn summary(self) -> &'static str {
        match self {
            ServeState::Running => "running — the dashboard is up",
            ServeState::Down => "DOWN — a managed service is installed but nothing is listening (crashed/restarting)",
            ServeState::NotInstalled => "not running — no managed service and nothing is listening",
        }
    }
}

/// Classify the managed service from the two probes. A live port always reads `Running` (even a
/// bare `bn serve` foreground counts); an installed-but-silent service is honestly `Down`; neither
/// is `NotInstalled`. Pure.
pub fn service_state(installed: bool, listening: bool) -> ServeState {
    match (installed, listening) {
        (_, true) => ServeState::Running,
        (true, false) => ServeState::Down,
        (false, false) => ServeState::NotInstalled,
    }
}
