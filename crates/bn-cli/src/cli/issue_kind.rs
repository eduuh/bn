//! `bn issue file --kind`. Split out of [`super`] under the never-grow ratchet.

/// The problem kind for `bn issue file`, each mapping to a canonical label.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IssueKindArg {
    Bug,
    Friction,
    Blocker,
    Enhancement,
}
