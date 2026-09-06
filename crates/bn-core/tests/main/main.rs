//! Every bn-core integration test, in one binary.
//!
//! Each file under `main/` used to be its own `tests/*.rs`, and cargo builds one test binary per
//! such file — so 19 files meant 19 full links of bn-core, paid on every `cargo test`,
//! every `cargo clippy --all-targets`, and every CI run. Declaring them as modules of a single
//! target collapses that to one link without changing a single test.
//!
//! Adding a test file: drop it in `main/` and add a `mod` line below. It stays an integration
//! test — it sees only bn-core's public API, exactly as before.
//!
//! Safe because none of these tests mutate process-wide state. They set no environment variables
//! and never change the working directory, so sharing one process introduces no cross-test races.
//! A test that needs to mutate the environment does not belong here; give it its own
//! `tests/<name>.rs` target (and its own process) instead.

mod advise_rank;
mod agent_doc_dir;
mod agent_opt_in;
mod attn_staff_promotion;
mod events_concurrent_append;
mod next_fit;
mod queue_durations;
mod queue_fit;
mod queue_node_payload;
mod queue_recurrence;
mod queue_reservoir;
mod queue_runs;
mod queue_tick_policy;
mod queue_units;
mod queue_windows;
mod task_ledger;
