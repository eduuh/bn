// `BN_VERSION` comes from build.rs: the release workflow's computed `vX.Y.Z` when it built
// this binary, else Cargo.toml's workspace version for a local build. See build.rs.
pub(crate) const VERSION: &str = concat!(
    env!("BN_VERSION"),
    " (git ",
    env!("BN_GIT_SHA"),
    ", built ",
    env!("BN_BUILD_DATE"),
    ", ",
    env!("BN_BUILD_TARGET"),
    ")"
);

pub(crate) fn line() -> String {
    format!("bn {VERSION}")
}
