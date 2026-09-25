//! `bn on` — user-scriptable reactions to the event log (see [`bn_core::reactor`]).
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum OnCmd {
    /// Add a rule: when an event of KIND lands, run this command.
    ///
    /// The command is argv, run directly — no shell — so nothing is word-split or glob-expanded
    /// behind your back; wrap it in `sh -c '…'` when you want shell syntax. It receives the event
    /// as `BN_EVENT_KIND`, `BN_EVENT_TS` and one `BN_EVENT_<FIELD>` per field, and the whole event
    /// as JSON on stdin.
    ///
    /// A new rule starts from now: it never fires on events already in the log.
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Add {
        /// The event kind to match (e.g. `pr_merged`), or `*` for every kind.
        kind: String,
        /// Only fire when the event carries this field value. Repeatable; all must match.
        #[arg(long = "field", value_name = "K=V")]
        fields: Vec<String>,
        /// The command and its arguments.
        #[arg(required = true)]
        run: Vec<String>,
    },
    /// Remove a rule by id.
    Rm { id: String },
    /// Stop a rule firing without deleting it (`bn on enable` puts it back).
    Disable { id: String },
    /// Re-enable a disabled rule.
    Enable { id: String },
    /// Run one reactor pass: fire every rule matched by events since the last pass.
    ///
    /// This is what the `reactor` cron calls. Safe to run by hand — the cursor means an event is
    /// considered once, so a manual pass does not double-fire what the cron already handled.
    Tick {
        /// Show what would fire and change nothing — no commands, no cursor move.
        #[arg(long)]
        dry_run: bool,
        /// Consider the whole log instead of only events since the cursor. For testing a rule you
        /// just wrote; it will re-fire anything that already fired.
        #[arg(long)]
        replay: bool,
    },
    /// Show which rules a synthetic event would fire, without running anything and without
    /// touching the log or the cursor — the way to check a rule before trusting it.
    Test {
        /// The event kind to simulate.
        kind: String,
        /// A field on the simulated event. Repeatable.
        #[arg(long = "field", value_name = "K=V")]
        fields: Vec<String>,
    },
}
