pub(crate) const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
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
