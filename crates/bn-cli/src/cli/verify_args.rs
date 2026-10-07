//! The flags `bn verify` and `bn gate` share. Split out of [`super`] under the never-grow ratchet,
//! so a new gate flag lands here instead of twice in `cli.rs`.
use clap::Args;

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
