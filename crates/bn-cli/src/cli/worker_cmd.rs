//! `bn worker` subcommands, split out of `cli.rs` to keep it under its size ratchet.
use crate::*;

/// The `bn worker` subcommands (#719 — the attempt-registry slice of epic #707 M1).
#[derive(Subcommand, Debug)]
pub(crate) enum WorkerCmd {
    /// Report this worker's capabilities/labels, a capacity snapshot (RAM/swap/load/disk), its
    /// derived READY/PRESSURE admission status, and the durable attempt registry. `--json` (the
    /// global flag) emits the versioned machine payload; the plain form prints a human summary.
    Status,
    /// Close a running attempt: RUNNING → SUCCEEDED (default) or FAILED (`--fail`). Called by the
    /// spawn wrapper (`tmux ... 'agent-cmd ; bn worker finish <request_id>'`). Records the exit
    /// code so the outcome is retrievable by id. Resolves the request's most recent RUNNING
    /// attempt.
    Finish {
        /// The request whose running attempt is finishing.
        request_id: String,
        /// Mark the attempt FAILED instead of SUCCEEDED.
        #[arg(long)]
        fail: bool,
        /// The agent's exit status to record on the attempt.
        #[arg(long)]
        exit_code: Option<i64>,
    },
    /// Worker-authoritative atomic admission (#720, epic #707 M1). Opens the registry with
    /// `BEGIN IMMEDIATE`, re-reads `MemAvailable` inside the txn, self-heals dead reservations,
    /// enforces the memory guard (`estimate + 512MB`) and the single M1 slot, then records the
    /// `RUNNING` reservation BEFORE any worktree/tmux side effect. Prints the verdict
    /// (`accepted` / `already_exists` / `rejected_capacity`); exit 0 accepted or already_exists,
    /// 1 rejected, 2 internal error.
    Start {
        /// The dispatch request id — the idempotency/dedupe key `(repo, id)` maps onto.
        #[arg(long)]
        request_id: String,
        /// This attempt's unique id; a retried dispatch reuses the exact id → `already_exists`.
        #[arg(long)]
        attempt_id: String,
        /// A human/scheduler label for what the attempt runs (e.g. the issue number).
        #[arg(long)]
        task: String,
        /// Repository the worker-local dispatch should create the lane in.
        #[arg(long)]
        repo: String,
        /// Agent harness passed to the canonical worker-local dispatch.
        #[arg(long, default_value = "copilot")]
        harness: String,
        /// The agent's estimated memory ceiling (MB); admission needs `MemAvailable >= this + 512`
        /// and the launch is wrapped in a cgroup cap at this ceiling.
        #[arg(long)]
        memory_ceiling_mb: u64,
    },
    /// Internal monitor process kept inside the attempt's `tmux -L orch` liveness session.
    #[command(hide = true)]
    Run {
        #[arg(long)]
        attempt_id: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        memory_ceiling_mb: u64,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Sweep RUNNING reservations and clear the orphaned ones — a manual counterpart to the
    /// self-heal step of `start`. Run after a reboot to release attempts whose tmux session/pid is
    /// gone. Prints the cleared attempt ids.
    Reconcile,
}
