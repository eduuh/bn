//! The coordination DB: one SQLite connection in WAL mode, schema, and migration.

use rusqlite::Connection;
use std::fmt;
use std::path::Path;

/// Bumped when the schema changes; [`CoordDb::migrate`] walks an older DB forward.
pub const SCHEMA_VERSION: u32 = 2;

/// Why a coordination call failed.
#[derive(Debug)]
pub enum CoordError {
    /// An underlying SQLite failure.
    Sqlite(rusqlite::Error),
    /// No watchdog with this id.
    NotFound(String),
    /// A malformed argument (bad seat, empty note, …).
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, CoordError>;

impl fmt::Display for CoordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoordError::Sqlite(e) => write!(f, "coord db error: {e}"),
            CoordError::NotFound(id) => write!(f, "no {id}"),
            CoordError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for CoordError {}

impl From<rusqlite::Error> for CoordError {
    fn from(e: rusqlite::Error) -> Self {
        CoordError::Sqlite(e)
    }
}

/// The coordination store.
pub struct CoordDb {
    pub(super) conn: Connection,
}

impl CoordDb {
    /// Open (creating if absent) the DB at `path`, creating its parent directory.
    pub fn open(path: &Path) -> Result<CoordDb> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CoordError::Invalid(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        CoordDb::init(Connection::open(path)?)
    }

    /// Open the default DB at [`super::db_path`].
    pub fn open_default() -> Result<CoordDb> {
        CoordDb::open(&super::db_path())
    }

    /// An ephemeral in-memory DB, for tests.
    pub fn open_in_memory() -> Result<CoordDb> {
        CoordDb::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<CoordDb> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // `BEGIN IMMEDIATE` then waits on a peer's write lock instead of failing SQLITE_BUSY, so two
        // ticks racing for one watchdog's slot get exactly one winner.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let db = CoordDb { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Create the schema if absent and stamp its version. Idempotent.
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        // v2: watchdogs. `IF NOT EXISTS` makes this the migration too — a v1 DB gains the tables.
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS watchdogs (
                 id            INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind          TEXT NOT NULL,
                 target        TEXT NOT NULL,
                 text          TEXT,
                 spec_json     TEXT NOT NULL DEFAULT '{}',
                 interval_secs INTEGER,
                 next_due_at   INTEGER NOT NULL,
                 expires_at    INTEGER,
                 max_fires     INTEGER,
                 fires         INTEGER NOT NULL DEFAULT 0,
                 qitem_id      INTEGER, -- unused; left from the retired queue rows
                 state         TEXT NOT NULL DEFAULT 'active',
                 created_by    TEXT NOT NULL,
                 created_at    INTEGER NOT NULL,
                 stopped_at    INTEGER,
                 stop_reason   TEXT,
                 last_eval_at  INTEGER,
                 defer_count   INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS watchdog_evals (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 watchdog_id INTEGER NOT NULL REFERENCES watchdogs(id),
                 ts          INTEGER NOT NULL,
                 outcome     TEXT NOT NULL,
                 detail      TEXT
             );
             CREATE INDEX IF NOT EXISTS wd_due ON watchdogs(state, next_due_at);
             CREATE INDEX IF NOT EXISTS wde_wd ON watchdog_evals(watchdog_id, id);",
        )?;
        self.conn.execute(
            "INSERT INTO schema_meta(key, value) VALUES ('version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// The schema version stamped in the DB.
    pub fn schema_version(&self) -> Result<u32> {
        let v: String = self.conn.query_row(
            "SELECT value FROM schema_meta WHERE key = 'version'",
            [],
            |r| r.get(0),
        )?;
        Ok(v.parse().unwrap_or(0))
    }
}
