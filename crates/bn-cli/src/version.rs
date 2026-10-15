use bn_core::render::Envelope;

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

pub(crate) fn render(json: bool) -> String {
    if json {
        Envelope::success("version", serde_json::json!({ "version": line() })).to_json()
    } else {
        line()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_render_is_the_version_line() {
        assert_eq!(render(false), line());
    }

    #[test]
    fn json_render_uses_the_standard_envelope() {
        let rendered = render(true);
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid json");
        assert_eq!(
            parsed,
            serde_json::json!({
                "v": bn_core::render::ENVELOPE_VERSION,
                "ok": true,
                "command": "version",
                "data": { "version": line() },
                "error": serde_json::Value::Null,
            })
        );
    }
}
