//! Durable worker-local attempt registry (#719) — the liveness channel for dispatched work; git
//! stays audit-only. Backed by SQLite in WAL mode so a write survives a crash and is the recovery
//! point (persist BEFORE any process/worktree creation — that admission logic is slice #720, but
//! the schema is designed for it here).
//!
//! **State machine (epic #707 v4):** `QUEUED → RUNNING → SUCCEEDED | FAILED`. Only these four are
//! *stored*. `LOST` is *derived* at reconcile — a `RUNNING` row whose tmux session is gone and
//! that never wrote a finish (see [`DerivedState`] / `derived_state`); it is never a stored
//! state, so there is no 7-state machine.
//!
//! **Idempotency:** `attempt_id` is the primary key. A retried dispatch reuses the exact
//! `attempt_id`, so a second [`Registry::enqueue`] of an admitted request returns
//! [`EnqueueOutcome::AlreadyExists`] — no second worktree/PR. The command result + exit status are
//! stored on the row ([`Registry::finish`]) so a lost ack can be answered by id without re-running.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::path::Path;

/// Slots this worker admits concurrently in M1 (epic #707): a single agent at a time. A larger
/// cap is a later-milestone knob; the admission logic is written against this constant so raising
/// it is a one-line change.
pub const MAX_SLOTS: usize = 1;

/// Headroom (MB) kept free above an attempt's memory estimate — the overcommit guard band. The
/// admission gate requires `MemAvailable >= estimate + MEM_RESERVE_MB` (epic #707 v4).
pub const MEM_RESERVE_MB: u64 = 512;

/// The registry schema version, surfaced in `bn worker status --json` and stored in `schema_meta`.
/// Bump it (and add a migration in [`Registry::migrate`]) on any incompatible schema change.
pub const SCHEMA_VERSION: u32 = 1;

/// A stored attempt state. Exactly four, per epic #707 v4 — `LOST` is derived, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AttemptState {
    /// Recorded by the dispatch client BEFORE ack — the durable recovery point.
    Queued,
    /// Locked by the worker: the agent has been (or is being) spawned. Runtime identity (pid,
    /// tmux session) is persisted here.
    Running,
    /// Terminal: the attempt finished successfully.
    Succeeded,
    /// Terminal: the attempt finished with a failure.
    Failed,
}

impl AttemptState {
    fn as_str(self) -> &'static str {
        match self {
            AttemptState::Queued => "QUEUED",
            AttemptState::Running => "RUNNING",
            AttemptState::Succeeded => "SUCCEEDED",
            AttemptState::Failed => "FAILED",
        }
    }

    fn parse(s: &str) -> Option<AttemptState> {
        match s {
            "QUEUED" => Some(AttemptState::Queued),
            "RUNNING" => Some(AttemptState::Running),
            "SUCCEEDED" => Some(AttemptState::Succeeded),
            "FAILED" => Some(AttemptState::Failed),
            _ => None,
        }
    }
}

impl std::fmt::Display for AttemptState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The reconcile-time view of an attempt: the four stored states plus the derived `LOST`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DerivedState {
    Queued,
    Running,
    Succeeded,
    Failed,
    /// A `RUNNING` row whose worker/tmux session is no longer alive and which never wrote a
    /// finish. Derived only — never persisted.
    Lost,
}

/// The immutable identity of a new attempt, recorded at `QUEUED` (before ack). `node` is the
/// worker the attempt is bound to; `task` is a human/scheduler label for what it runs.
#[derive(Debug, Clone)]
pub struct NewAttempt {
    pub attempt_id: String,
    pub request_id: String,
    pub task: String,
    pub node: String,
}

/// A full attempt row as stored.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attempt {
    pub attempt_id: String,
    pub request_id: String,
    pub task: String,
    pub node: String,
    pub state: AttemptState,
    /// The `tmux -L orch` session name, set at `RUNNING` (the liveness domain for LOST derivation).
    pub tmux_session: Option<String>,
    /// The agent process id, set at `RUNNING`.
    pub pid: Option<i64>,
    /// The stored command result (stdout/summary), set at finish — retrievable by id so a lost ack
    /// never forces re-execution.
    pub result: Option<String>,
    /// The exit status, set at finish.
    pub exit_code: Option<i64>,
    /// When the row was first inserted (ISO-8601).
    pub created_ts: String,
    /// When the row last transitioned (ISO-8601).
    pub updated_ts: String,
}

/// The result of an [`Registry::enqueue`]: a genuinely new attempt was admitted, or the
/// `attempt_id` already existed (an idempotent retry — no new work).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// A new `QUEUED` row was inserted.
    Admitted,
    /// An attempt with this `attempt_id` already existed; nothing changed.
    AlreadyExists,
}

/// A worker-authoritative admission request (#720). Carries the immutable attempt identity plus
/// the memory estimate the overcommit guard checks against and the tmux session name that becomes
/// this attempt's liveness domain once it is `RUNNING`.
#[derive(Debug, Clone)]
pub struct AdmissionRequest {
    pub attempt_id: String,
    pub request_id: String,
    pub task: String,
    pub node: String,
    /// The agent's estimated memory ceiling (MB); admission requires
    /// `MemAvailable >= this + MEM_RESERVE_MB`.
    pub memory_estimate_mb: u64,
    /// The `tmux -L orch` session the spawned agent will run under — persisted at admission so a
    /// later self-heal/reconcile can probe this attempt's liveness.
    pub tmux_session: String,
}

/// The verdict of [`Registry::admit`], mapping 1:1 to the M1 decision table (issue #720).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionOutcome {
    /// A fresh `RUNNING` reservation was recorded (record-before-ack); the caller may now create
    /// the worktree + tmux session and spawn the agent.
    Accepted,
    /// This `attempt_id` was already recorded (live or terminal) — an idempotent retry. No second
    /// worktree/PR; the stored result is retrievable by id.
    AlreadyExists,
    /// The node cannot take the attempt right now: either the single slot is held by a live
    /// attempt, or `MemAvailable` is below the estimate + reserve.
    RejectedCapacity { reason: String },
}

/// Registry errors, kept dependency-free (no `thiserror`).
#[derive(Debug)]
pub enum RegistryError {
    /// An underlying SQLite failure.
    Sqlite(rusqlite::Error),
    /// The referenced `attempt_id` (or request) does not exist.
    NotFound(String),
    /// A state transition that the machine forbids (e.g. finishing a `QUEUED` attempt).
    IllegalTransition {
        attempt_id: String,
        from: AttemptState,
        to: AttemptState,
    },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Sqlite(e) => write!(f, "registry sqlite error: {e}"),
            RegistryError::NotFound(id) => write!(f, "attempt not found: {id}"),
            RegistryError::IllegalTransition {
                attempt_id,
                from,
                to,
            } => write!(
                f,
                "illegal transition {from} → {to} for attempt {attempt_id}"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<rusqlite::Error> for RegistryError {
    fn from(e: rusqlite::Error) -> Self {
        RegistryError::Sqlite(e)
    }
}

pub type Result<T> = std::result::Result<T, RegistryError>;

/// The durable attempt registry, wrapping one SQLite connection (WAL).
pub struct Registry {
    conn: Connection,
}

impl Registry {
    /// Open (creating if absent) the registry at `path`, in WAL mode, and ensure the schema. The
    /// parent directory is created if needed.
    pub fn open(path: &Path) -> Result<Registry> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                RegistryError::Sqlite(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                    Some(format!("create {}: {e}", parent.display())),
                ))
            })?;
        }
        let conn = Connection::open(path)?;
        Registry::init(conn)
    }

    /// Open an ephemeral in-memory registry — used by tests and by callers that only need the pure
    /// state-machine semantics.
    pub fn open_in_memory() -> Result<Registry> {
        Registry::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Registry> {
        // WAL: durable across a crash and lets a status reader proceed without blocking a writer.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Make `BEGIN IMMEDIATE` block on a peer's write lock (up to 5s) instead of failing with
        // SQLITE_BUSY — this is what serializes concurrent `admit` calls into exactly-one-accepted.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut reg = Registry { conn };
        reg.migrate()?;
        Ok(reg)
    }

    /// Create tables if absent and stamp the schema version. Future incompatible changes read the
    /// stored version here and migrate forward.
    fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS attempts (
                 attempt_id   TEXT PRIMARY KEY,
                 request_id   TEXT NOT NULL,
                 task         TEXT NOT NULL,
                 node         TEXT NOT NULL,
                 state        TEXT NOT NULL,
                 tmux_session TEXT,
                 pid          INTEGER,
                 result       TEXT,
                 exit_code    INTEGER,
                 created_ts   TEXT NOT NULL,
                 updated_ts   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_attempts_request ON attempts(request_id);
             CREATE INDEX IF NOT EXISTS idx_attempts_state ON attempts(state);",
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO schema_meta(key, value) VALUES ('version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// The schema version stored in the DB (matches [`SCHEMA_VERSION`] for a freshly-created one).
    pub fn schema_version(&self) -> Result<u32> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(SCHEMA_VERSION))
    }

    /// Insert a new `QUEUED` attempt (the durable recovery point). Idempotent on `attempt_id`: a
    /// retry of an already-recorded attempt returns [`EnqueueOutcome::AlreadyExists`] and changes
    /// nothing, so a lost ack never spawns a second worktree/PR.
    pub fn enqueue(&self, a: &NewAttempt) -> Result<EnqueueOutcome> {
        let now = now_iso();
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO attempts
                 (attempt_id, request_id, task, node, state, created_ts, updated_ts)
             VALUES (?1, ?2, ?3, ?4, 'QUEUED', ?5, ?5)",
            rusqlite::params![a.attempt_id, a.request_id, a.task, a.node, now],
        )?;
        Ok(if changed == 1 {
            EnqueueOutcome::Admitted
        } else {
            EnqueueOutcome::AlreadyExists
        })
    }

    /// Finish a `RUNNING` attempt into `SUCCEEDED`/`FAILED`, storing the command result + exit code
    /// remotely so the outcome is retrievable by id. Rejects any non-`RUNNING` source state.
    pub fn finish(
        &self,
        attempt_id: &str,
        success: bool,
        exit_code: Option<i64>,
        result: Option<&str>,
    ) -> Result<()> {
        let to = if success {
            AttemptState::Succeeded
        } else {
            AttemptState::Failed
        };
        // Atomic guard (see `mark_running`): only a still-`RUNNING` row transitions to a terminal
        // state, in one statement, so a lost ack can't race a reconcile into a double finish.
        let changed = self.conn.execute(
            "UPDATE attempts
             SET state = ?2, exit_code = ?3, result = ?4, updated_ts = ?5
             WHERE attempt_id = ?1 AND state = 'RUNNING'",
            rusqlite::params![attempt_id, to.as_str(), exit_code, result, now_iso()],
        )?;
        if changed == 0 {
            let current = self.require_state(attempt_id)?;
            return Err(RegistryError::IllegalTransition {
                attempt_id: attempt_id.to_string(),
                from: current,
                to,
            });
        }
        Ok(())
    }

    /// Fetch one attempt by id.
    pub fn get(&self, attempt_id: &str) -> Result<Option<Attempt>> {
        self.conn
            .query_row(
                &format!("{SELECT_ATTEMPT} WHERE attempt_id = ?1"),
                [attempt_id],
                row_to_attempt,
            )
            .optional()
            .map_err(Into::into)
    }

    /// All attempts, newest-created first.
    pub fn list(&self) -> Result<Vec<Attempt>> {
        let mut stmt = self.conn.prepare(&format!(
            "{SELECT_ATTEMPT} ORDER BY created_ts DESC, attempt_id"
        ))?;
        let rows = stmt.query_map([], row_to_attempt)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// All attempts for one `request_id`, newest-created first (a request can have >1 attempt).
    pub fn by_request(&self, request_id: &str) -> Result<Vec<Attempt>> {
        let mut stmt = self.conn.prepare(&format!(
            "{SELECT_ATTEMPT} WHERE request_id = ?1 ORDER BY created_ts DESC, attempt_id"
        ))?;
        let rows = stmt.query_map([request_id], row_to_attempt)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Worker-authoritative atomic admission (#720). Runs the entire check-then-persist sequence
    /// inside a `BEGIN IMMEDIATE` transaction so a concurrent `admit` cannot double-admit: the
    /// second txn blocks on the write lock and, on wake, sees the first's committed reservation.
    ///
    /// Sequence (matching the M1 decision table):
    /// 1. **Idempotency** — if this `attempt_id` already exists (live or terminal),
    ///    [`AdmissionOutcome::AlreadyExists`]; a lost ack never spawns a second worktree/PR.
    /// 2. **Self-heal** — every `RUNNING` row is liveness-probed via `is_alive`; a dead one is
    ///    cleared to `FAILED` (freeing its slot) before the capacity decision.
    /// 3. **Memory gate** — `read_mem_available_mb` is called *inside* the txn (a fresh
    ///    `/proc/meminfo` read); reject if `MemAvailable < estimate + MEM_RESERVE_MB`. `None`
    ///    (unmeasurable) fails open, matching bn's other guards.
    /// 4. **Slot cap** — reject if the live `RUNNING` count is already at [`MAX_SLOTS`].
    /// 5. **Record-before-ack** — insert the reservation as `RUNNING` (with its tmux session) and
    ///    commit; the caller then creates the worktree/tmux and spawns the agent.
    ///
    /// `is_alive(session, pid)` is the caller's liveness probe (e.g. `tmux -L orch has-session` /
    /// `kill -0`). `read_mem_available_mb()` returns `MemAvailable` in MB (or `None`). Both are
    /// injected so the policy is unit-testable without a Linux host or tmux.
    pub fn admit(
        &mut self,
        req: &AdmissionRequest,
        read_mem_available_mb: &dyn Fn() -> Option<u64>,
        is_alive: &dyn Fn(Option<&str>, Option<i64>) -> bool,
    ) -> Result<AdmissionOutcome> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        // (1) Idempotency on attempt_id: any existing record (live or terminal) short-circuits.
        let existing: Option<String> = tx
            .query_row(
                "SELECT state FROM attempts WHERE attempt_id = ?1",
                [&req.attempt_id],
                |r| r.get(0),
            )
            .optional()?;
        if existing.is_some() {
            tx.commit()?;
            return Ok(AdmissionOutcome::AlreadyExists);
        }

        // (2) Self-heal: clear dead RUNNING rows so their slot is reclaimable.
        let running = {
            let mut stmt = tx.prepare(&format!("{SELECT_ATTEMPT} WHERE state = 'RUNNING'"))?;
            let rows = stmt.query_map([], row_to_attempt)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut live_slots = 0usize;
        for a in &running {
            if is_alive(a.tmux_session.as_deref(), a.pid) {
                live_slots += 1;
            } else {
                tx.execute(
                    "UPDATE attempts
                     SET state = 'FAILED', result = 'self-heal: session dead', updated_ts = ?2
                     WHERE attempt_id = ?1",
                    rusqlite::params![a.attempt_id, now_iso()],
                )?;
            }
        }

        // (3) Memory gate — re-read MemAvailable inside the txn (hard overcommit protection).
        let need = req.memory_estimate_mb + MEM_RESERVE_MB;
        if let Some(avail) = read_mem_available_mb() {
            if avail < need {
                tx.commit()?;
                return Ok(AdmissionOutcome::RejectedCapacity {
                    reason: format!("insufficient memory: {avail}MB available < {need}MB required"),
                });
            }
        }

        // (4) Slot cap.
        if live_slots >= MAX_SLOTS {
            tx.commit()?;
            return Ok(AdmissionOutcome::RejectedCapacity {
                reason: format!("no free slot: {live_slots}/{MAX_SLOTS} in use"),
            });
        }

        // (5) Record-before-ack: persist the RUNNING reservation, then commit.
        let now = now_iso();
        tx.execute(
            "INSERT INTO attempts
                 (attempt_id, request_id, task, node, state, tmux_session, created_ts, updated_ts)
             VALUES (?1, ?2, ?3, ?4, 'RUNNING', ?5, ?6, ?6)",
            rusqlite::params![
                req.attempt_id,
                req.request_id,
                req.task,
                req.node,
                req.tmux_session,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(AdmissionOutcome::Accepted)
    }

    /// Sweep every `RUNNING` attempt and clear the dead ones (a manual counterpart to the
    /// self-heal step of [`admit`]). Returns the `attempt_id`s that were marked `FAILED`. Called by
    /// `bn worker reconcile` after a reboot to release orphaned reservations.
    pub fn reconcile(
        &mut self,
        is_alive: &dyn Fn(Option<&str>, Option<i64>) -> bool,
    ) -> Result<Vec<String>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running = {
            let mut stmt = tx.prepare(&format!("{SELECT_ATTEMPT} WHERE state = 'RUNNING'"))?;
            let rows = stmt.query_map([], row_to_attempt)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut cleared = Vec::new();
        for a in &running {
            if !is_alive(a.tmux_session.as_deref(), a.pid) {
                tx.execute(
                    "UPDATE attempts
                     SET state = 'FAILED', result = 'reconcile: session dead', updated_ts = ?2
                     WHERE attempt_id = ?1",
                    rusqlite::params![a.attempt_id, now_iso()],
                )?;
                cleared.push(a.attempt_id.clone());
            }
        }
        tx.commit()?;
        Ok(cleared)
    }

    fn require_state(&self, attempt_id: &str) -> Result<AttemptState> {
        let s: Option<String> = self
            .conn
            .query_row(
                "SELECT state FROM attempts WHERE attempt_id = ?1",
                [attempt_id],
                |r| r.get(0),
            )
            .optional()?;
        match s.as_deref().and_then(AttemptState::parse) {
            Some(state) => Ok(state),
            None => Err(RegistryError::NotFound(attempt_id.to_string())),
        }
    }
}

const SELECT_ATTEMPT: &str =
    "SELECT attempt_id, request_id, task, node, state, tmux_session, pid, \
     result, exit_code, created_ts, updated_ts FROM attempts";

fn row_to_attempt(r: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
    let state_str: String = r.get(4)?;
    Ok(Attempt {
        attempt_id: r.get(0)?,
        request_id: r.get(1)?,
        task: r.get(2)?,
        node: r.get(3)?,
        state: AttemptState::parse(&state_str).unwrap_or(AttemptState::Failed),
        tmux_session: r.get(5)?,
        pid: r.get(6)?,
        result: r.get(7)?,
        exit_code: r.get(8)?,
        created_ts: r.get(9)?,
        updated_ts: r.get(10)?,
    })
}

/// Current timestamp as ISO-8601. Honors `$BN_NOW` (epoch seconds) for deterministic tests, like
/// the rest of bn's time helpers ([`crate::util::now_stamp`]).
fn now_iso() -> String {
    if let Ok(s) = std::env::var("BN_NOW") {
        if let Ok(n) = s.trim().parse::<i64>() {
            if let Some(dt) = chrono::DateTime::from_timestamp(n, 0) {
                return dt.to_rfc3339();
            }
        }
    }
    chrono::Utc::now().to_rfc3339()
}
