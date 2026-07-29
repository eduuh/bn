//! Durable worker-local attempt registry (#719) — the liveness channel for dispatched work; git
//! stays audit-only. Backed by SQLite in WAL mode so a write survives a crash and is the recovery
//! point (persist BEFORE any process/worktree creation — that admission logic is slice #720, but
//! the schema is designed for it here).
//!
//! **State machine (epic #707 v4):** `QUEUED → RUNNING → SUCCEEDED | FAILED`. Only these four are
//! *stored*. `LOST` is *derived* at reconcile — a `RUNNING` row whose tmux session is gone and
//! that never wrote a finish (see [`DerivedState`] / [`derived_state`]); it is never a stored
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

/// Derive the reconcile-time state of an attempt from its stored state plus a liveness verdict.
/// `alive` is the caller's probe result (e.g. `tmux -L orch has-session` / `kill -0 <pid>`); it is
/// only consulted for `RUNNING` rows. Pure so the LOST-derivation policy is unit-testable without
/// tmux or a live process.
pub fn derived_state(state: AttemptState, alive: bool) -> DerivedState {
    match state {
        AttemptState::Queued => DerivedState::Queued,
        AttemptState::Running if !alive => DerivedState::Lost,
        AttemptState::Running => DerivedState::Running,
        AttemptState::Succeeded => DerivedState::Succeeded,
        AttemptState::Failed => DerivedState::Failed,
    }
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

    /// Lock a `QUEUED` attempt into `RUNNING`, persisting its runtime identity (tmux session, pid).
    /// Rejects any non-`QUEUED` source state as an illegal transition.
    pub fn mark_running(
        &self,
        attempt_id: &str,
        tmux_session: Option<&str>,
        pid: Option<i64>,
    ) -> Result<()> {
        // Atomic guard: the promotion only fires while the row is still `QUEUED`, so two racing
        // callers can never both win — the `AND state = 'QUEUED'` predicate and the row write
        // happen under one SQLite statement. A separate read-then-write (as this once was) could
        // let both readers see `QUEUED` and both promote, spawning a second worktree/PR.
        let changed = self.conn.execute(
            "UPDATE attempts
             SET state = 'RUNNING', tmux_session = ?2, pid = ?3, updated_ts = ?4
             WHERE attempt_id = ?1 AND state = 'QUEUED'",
            rusqlite::params![attempt_id, tmux_session, pid, now_iso()],
        )?;
        if changed == 0 {
            // The row was absent or no longer `QUEUED`. Re-read to return the precise error the
            // callers already distinguish (`NotFound` vs `IllegalTransition`).
            let current = self.require_state(attempt_id)?;
            return Err(RegistryError::IllegalTransition {
                attempt_id: attempt_id.to_string(),
                from: current,
                to: AttemptState::Running,
            });
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn new_attempt(id: &str, req: &str) -> NewAttempt {
        NewAttempt {
            attempt_id: id.to_string(),
            request_id: req.to_string(),
            task: "issue-719".to_string(),
            node: "worker-a".to_string(),
        }
    }

    #[test]
    fn fresh_registry_reports_schema_version() {
        let reg = Registry::open_in_memory().unwrap();
        assert_eq!(reg.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn empty_registry_lists_nothing() {
        let reg = Registry::open_in_memory().unwrap();
        assert!(reg.list().unwrap().is_empty());
    }

    #[test]
    fn round_trip_full_lifecycle() {
        let reg = Registry::open_in_memory().unwrap();
        assert_eq!(
            reg.enqueue(&new_attempt("a1", "r1")).unwrap(),
            EnqueueOutcome::Admitted
        );
        let a = reg.get("a1").unwrap().unwrap();
        assert_eq!(a.state, AttemptState::Queued);
        assert_eq!(a.request_id, "r1");
        assert_eq!(a.task, "issue-719");
        assert_eq!(a.node, "worker-a");

        reg.mark_running("a1", Some("agent-r1"), Some(4242))
            .unwrap();
        let a = reg.get("a1").unwrap().unwrap();
        assert_eq!(a.state, AttemptState::Running);
        assert_eq!(a.tmux_session.as_deref(), Some("agent-r1"));
        assert_eq!(a.pid, Some(4242));

        reg.finish("a1", true, Some(0), Some("all green")).unwrap();
        let a = reg.get("a1").unwrap().unwrap();
        assert_eq!(a.state, AttemptState::Succeeded);
        assert_eq!(a.exit_code, Some(0));
        assert_eq!(a.result.as_deref(), Some("all green"));
    }

    #[test]
    fn enqueue_is_idempotent_on_attempt_id() {
        let reg = Registry::open_in_memory().unwrap();
        assert_eq!(
            reg.enqueue(&new_attempt("a1", "r1")).unwrap(),
            EnqueueOutcome::Admitted
        );
        // A retry with the same attempt_id (even mid-flight) never inserts a second row.
        reg.mark_running("a1", None, None).unwrap();
        assert_eq!(
            reg.enqueue(&new_attempt("a1", "r1")).unwrap(),
            EnqueueOutcome::AlreadyExists
        );
        assert_eq!(reg.list().unwrap().len(), 1);
        // The retry did not clobber the RUNNING state back to QUEUED.
        assert_eq!(reg.get("a1").unwrap().unwrap().state, AttemptState::Running);
    }

    #[test]
    fn illegal_transition_finish_before_running() {
        let reg = Registry::open_in_memory().unwrap();
        reg.enqueue(&new_attempt("a1", "r1")).unwrap();
        let err = reg.finish("a1", true, Some(0), None).unwrap_err();
        match err {
            RegistryError::IllegalTransition { from, to, .. } => {
                assert_eq!(from, AttemptState::Queued);
                assert_eq!(to, AttemptState::Succeeded);
            }
            other => panic!("expected IllegalTransition, got {other:?}"),
        }
    }

    #[test]
    fn illegal_transition_running_from_terminal() {
        let reg = Registry::open_in_memory().unwrap();
        reg.enqueue(&new_attempt("a1", "r1")).unwrap();
        reg.mark_running("a1", None, None).unwrap();
        reg.finish("a1", false, Some(1), None).unwrap();
        let err = reg.mark_running("a1", None, None).unwrap_err();
        assert!(matches!(err, RegistryError::IllegalTransition { .. }));
    }

    #[test]
    fn transition_on_missing_attempt_is_not_found() {
        let reg = Registry::open_in_memory().unwrap();
        assert!(matches!(
            reg.mark_running("ghost", None, None).unwrap_err(),
            RegistryError::NotFound(_)
        ));
    }

    #[test]
    fn concurrent_mark_running_promotes_exactly_once() {
        // The exactly-once guarantee under a real race: many workers open their own connection to
        // one file-backed registry and all try to promote the same QUEUED attempt at once. The
        // atomic `WHERE state = 'QUEUED'` UPDATE must let exactly one win; the read-then-write this
        // replaced could let two both observe QUEUED and both spawn a worktree/PR.
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reg.sqlite");
        Registry::open(&path)
            .unwrap()
            .enqueue(&new_attempt("a1", "r1"))
            .unwrap();

        // Spawn every racer before joining any — the barrier only releases once all RACERS have
        // reached it, so all threads must be live first.
        const RACERS: usize = 16;
        let barrier = Arc::new(Barrier::new(RACERS));
        let mut handles = Vec::with_capacity(RACERS);
        for i in 0..RACERS {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let reg = Registry::open(&path).unwrap();
                barrier.wait(); // release all racers together to maximise contention
                reg.mark_running("a1", Some(&format!("agent-{i}")), Some(i as i64))
            }));
        }
        let mut results = Vec::with_capacity(RACERS);
        for h in handles {
            results.push(h.join().unwrap());
        }

        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "exactly one racer may promote QUEUED -> RUNNING"
        );
        for r in &results {
            if let Err(e) = r {
                assert!(
                    matches!(e, RegistryError::IllegalTransition { .. }),
                    "a loser must see an illegal transition, not {e:?}"
                );
            }
        }
        assert_eq!(
            Registry::open(&path).unwrap().get("a1").unwrap().unwrap().state,
            AttemptState::Running
        );
    }

    #[test]
    fn lost_is_derived_only_for_dead_running_rows() {
        // QUEUED / terminal states ignore the liveness probe.
        assert_eq!(
            derived_state(AttemptState::Queued, false),
            DerivedState::Queued
        );
        assert_eq!(
            derived_state(AttemptState::Succeeded, false),
            DerivedState::Succeeded
        );
        assert_eq!(
            derived_state(AttemptState::Failed, false),
            DerivedState::Failed
        );
        // A live RUNNING row stays RUNNING; a dead one is LOST.
        assert_eq!(
            derived_state(AttemptState::Running, true),
            DerivedState::Running
        );
        assert_eq!(
            derived_state(AttemptState::Running, false),
            DerivedState::Lost
        );
    }

    #[test]
    fn by_request_groups_attempts() {
        let reg = Registry::open_in_memory().unwrap();
        reg.enqueue(&new_attempt("a1", "r1")).unwrap();
        reg.enqueue(&new_attempt("a2", "r1")).unwrap();
        reg.enqueue(&new_attempt("b1", "r2")).unwrap();
        assert_eq!(reg.by_request("r1").unwrap().len(), 2);
        assert_eq!(reg.by_request("r2").unwrap().len(), 1);
        assert!(reg.by_request("nope").unwrap().is_empty());
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.db");
        {
            let reg = Registry::open(&path).unwrap();
            reg.enqueue(&new_attempt("a1", "r1")).unwrap();
            reg.mark_running("a1", Some("agent-r1"), Some(7)).unwrap();
        }
        let reg = Registry::open(&path).unwrap();
        let a = reg.get("a1").unwrap().unwrap();
        assert_eq!(a.state, AttemptState::Running);
        assert_eq!(a.pid, Some(7));
        assert_eq!(reg.schema_version().unwrap(), SCHEMA_VERSION);
    }

    // ---- admission (#720) --------------------------------------------------

    fn admission(id: &str, req: &str, mem: u64) -> AdmissionRequest {
        AdmissionRequest {
            attempt_id: id.to_string(),
            request_id: req.to_string(),
            task: "issue-720".to_string(),
            node: "worker-a".to_string(),
            memory_estimate_mb: mem,
            tmux_session: format!("agent-{req}"),
        }
    }

    const ALIVE: &dyn Fn(Option<&str>, Option<i64>) -> bool = &|_, _| true;
    const DEAD: &dyn Fn(Option<&str>, Option<i64>) -> bool = &|_, _| false;
    const PLENTY: &dyn Fn() -> Option<u64> = &|| Some(64_000);

    #[test]
    fn admit_accepts_into_a_free_slot_and_records_running() {
        let mut reg = Registry::open_in_memory().unwrap();
        let out = reg
            .admit(&admission("a1", "r1", 1000), PLENTY, ALIVE)
            .unwrap();
        assert_eq!(out, AdmissionOutcome::Accepted);
        let a = reg.get("a1").unwrap().unwrap();
        assert_eq!(a.state, AttemptState::Running);
        assert_eq!(a.tmux_session.as_deref(), Some("agent-r1"));
    }

    #[test]
    fn admit_is_idempotent_on_attempt_id() {
        let mut reg = Registry::open_in_memory().unwrap();
        assert_eq!(
            reg.admit(&admission("a1", "r1", 1000), PLENTY, ALIVE)
                .unwrap(),
            AdmissionOutcome::Accepted
        );
        // Same attempt_id retried → already_exists, no second row, slot unchanged.
        assert_eq!(
            reg.admit(&admission("a1", "r1", 1000), PLENTY, ALIVE)
                .unwrap(),
            AdmissionOutcome::AlreadyExists
        );
        assert_eq!(reg.list().unwrap().len(), 1);
    }

    #[test]
    fn admit_rejects_when_single_slot_is_held_by_a_live_attempt() {
        let mut reg = Registry::open_in_memory().unwrap();
        reg.admit(&admission("a1", "r1", 1000), PLENTY, ALIVE)
            .unwrap();
        // A different attempt/request while the slot is live → rejected_capacity.
        let out = reg
            .admit(&admission("a2", "r2", 1000), PLENTY, ALIVE)
            .unwrap();
        assert!(matches!(out, AdmissionOutcome::RejectedCapacity { .. }));
    }

    #[test]
    fn admit_rejects_when_memory_below_estimate_plus_reserve() {
        let mut reg = Registry::open_in_memory().unwrap();
        // Ceiling 1000 + 512 reserve = 1512 required; only 1000 available → reject.
        let mem: &dyn Fn() -> Option<u64> = &|| Some(1000);
        let out = reg.admit(&admission("a1", "r1", 1000), mem, ALIVE).unwrap();
        assert!(matches!(out, AdmissionOutcome::RejectedCapacity { .. }));
        assert!(reg.get("a1").unwrap().is_none());
    }

    #[test]
    fn admit_fails_open_when_memory_unmeasurable() {
        let mut reg = Registry::open_in_memory().unwrap();
        let unknown: &dyn Fn() -> Option<u64> = &|| None;
        assert_eq!(
            reg.admit(&admission("a1", "r1", 999_999), unknown, ALIVE)
                .unwrap(),
            AdmissionOutcome::Accepted
        );
    }

    #[test]
    fn admit_self_heals_a_dead_reservation_then_accepts() {
        let mut reg = Registry::open_in_memory().unwrap();
        reg.admit(&admission("a1", "r1", 1000), PLENTY, ALIVE)
            .unwrap();
        // The incumbent is now dead: a fresh admit probes it dead, clears it, and takes the slot.
        let out = reg
            .admit(&admission("a2", "r2", 1000), PLENTY, DEAD)
            .unwrap();
        assert_eq!(out, AdmissionOutcome::Accepted);
        assert_eq!(reg.get("a1").unwrap().unwrap().state, AttemptState::Failed);
        assert_eq!(reg.get("a2").unwrap().unwrap().state, AttemptState::Running);
    }

    #[test]
    fn admit_permits_readmission_under_same_request_after_terminal() {
        let mut reg = Registry::open_in_memory().unwrap();
        reg.admit(&admission("a1", "r1", 1000), PLENTY, ALIVE)
            .unwrap();
        reg.finish("a1", true, Some(0), None).unwrap();
        // Terminal row frees the slot; a fresh attempt_id under the same request re-admits.
        let out = reg
            .admit(&admission("a2", "r1", 1000), PLENTY, ALIVE)
            .unwrap();
        assert_eq!(out, AdmissionOutcome::Accepted);
    }

    #[test]
    fn reconcile_clears_only_dead_running_rows() {
        let mut reg = Registry::open_in_memory().unwrap();
        reg.admit(&admission("a1", "r1", 1000), PLENTY, ALIVE)
            .unwrap();
        let cleared = reg.reconcile(DEAD).unwrap();
        assert_eq!(cleared, vec!["a1".to_string()]);
        assert_eq!(reg.get("a1").unwrap().unwrap().state, AttemptState::Failed);
        // A second sweep finds nothing live to clear.
        assert!(reg.reconcile(ALIVE).unwrap().is_empty());
    }

    #[test]
    fn concurrent_admit_of_same_request_admits_exactly_one() {
        use std::sync::{Arc, Barrier};
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("registry.db"));
        // Pre-create so both threads open an existing DB.
        Registry::open(&path).unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        // Two genuinely-different attempts racing for the single slot of the same request.
        for attempt in ["a1", "a2"] {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut reg = Registry::open(&path).unwrap();
                barrier.wait();
                let alive: &dyn Fn(Option<&str>, Option<i64>) -> bool = &|_, _| true;
                let plenty: &dyn Fn() -> Option<u64> = &|| Some(64_000);
                reg.admit(&admission(attempt, "r1", 1000), plenty, alive)
                    .unwrap()
            }));
        }
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let accepted = outcomes
            .iter()
            .filter(|o| **o == AdmissionOutcome::Accepted)
            .count();
        assert_eq!(
            accepted, 1,
            "exactly one attempt must be accepted: {outcomes:?}"
        );
        assert!(outcomes
            .iter()
            .any(|o| matches!(o, AdmissionOutcome::RejectedCapacity { .. })));
    }
}
