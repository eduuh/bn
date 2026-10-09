use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum CmdTracking {
    /// Weekly auto-retro: mine the telemetry bn already collects (punch log, focus blocks, PR
    /// metrics, todo ages, daily goals) into a metric table + one heuristic suggestion. Anchored
    /// to the configured weekly window; read-only and network-free.
    /// This is the *objective* retro.
    Insights {
        /// Rolling window of N days instead of the configured weekly anchor.
        #[arg(long)]
        since: Option<i64>,
        /// Write the rendered retro to `<notes>/<machine>/insights/<YYYY-Www>.md` (the cron
        /// snapshot path) instead of printing it, and report where it landed.
        #[arg(long)]
        write: bool,
        /// Record the current scoreboard numbers as the fixed baseline anchor (#276) — the
        /// pre-staff comparison point every later run renders against. Overwrites any prior
        /// anchor and reports where it landed.
        #[arg(long)]
        set_baseline: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn insights_help_does_not_reference_removed_retro_command() {
        let mut cmd = Cli::command();
        cmd.build();
        let help = cmd
            .find_subcommand_mut("insights")
            .expect("insights command")
            .render_help()
            .to_string();
        assert!(help.contains("objective"));
        assert!(!help.contains("bn retro"));
    }
}
