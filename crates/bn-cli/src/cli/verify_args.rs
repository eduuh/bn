//! Verification arguments: common gate flags plus verify-only preflight.
//! Split out of [`super`] under the never-grow ratchet.
use clap::Args;

#[derive(Args, Debug)]
pub(crate) struct VerifyCommandArgs {
    #[command(flatten)]
    pub(crate) gate: VerifyArgs,
    /// Preview the resolved gate without running checks or recording progress or proof.
    #[arg(long, conflicts_with = "for_")]
    pub(crate) dry_run: bool,
}

#[derive(Args, Debug)]
pub(crate) struct VerifyArgs {
    /// Job to run instead of the aggregate default gate.
    #[arg(long)]
    pub(crate) job: Option<String>,
    /// The done-when criteria (`w<N>`, comma-separated) this run is evidence for. The run is kept
    /// as proof either way; this links it (`bn proof link` can do it later).
    #[arg(long = "for", value_delimiter = ',')]
    pub(crate) for_: Vec<String>,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[test]
    fn preview_is_verify_only_and_cannot_claim_proof() {
        assert!(crate::Cli::try_parse_from(["bn", "verify", "--dry-run"]).is_ok());
        assert!(crate::Cli::try_parse_from(["bn", "verify", "--dry-run", "--job", "test"]).is_ok());
        assert!(crate::Cli::try_parse_from(["bn", "verify", "--dry-run", "--for", "w1"]).is_err());
        assert!(crate::Cli::try_parse_from(["bn", "gate", "--dry-run"]).is_err());
        assert!(crate::Cli::try_parse_from(["bn", "gate", "--for", "w1"]).is_ok());
    }
}
