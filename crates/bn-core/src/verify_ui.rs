//! bn-web UI gate policy (#505) — pure, no I/O.
//!
//! The Rust `bn verify`/`bn gate` machinery gates only Rust (rustfmt + clippy + `cargo test`); a
//! bn-web PR could merge with a broken SPA (a `tsc` error, a dep the agent forgot to add to
//! `package.json`, a red vitest). This module is the *decision* half: given the branch's
//! changed-file set, does the gate additionally need to build + test the SPA? — and what commands
//! that gate runs. The CLI supplies the (git-computed) changed files and executes the steps; all
//! the branching that's worth testing lives here, unit-tested in isolation.

/// Repo-relative prefix of the bn-web SPA sources (trailing slash). A change to any file under here
/// — and *only* here — triggers the UI gate. Note this is the `ui/` subtree specifically: the sibling
/// Rust sources under `crates/bn-web/src/` are gated by the ordinary Rust suite, not npm.
pub const UI_DIR: &str = "crates/bn-web/ui/";

/// The `--prefix` argument handed to `npm` (the SPA package root, repo-relative). The gate runs npm
/// from the repo root with this prefix, exactly as the issue specifies
/// (`npm --prefix crates/bn-web/ui …`).
pub const UI_PREFIX: &str = "crates/bn-web/ui";

/// One step of the UI gate: a human `label` and the `npm` argument vector to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiStep {
    pub label: &'static str,
    pub args: Vec<&'static str>,
}

/// Does this one changed path require the UI gate? True only for hand-editable SPA sources under
/// [`UI_DIR`], excluding the build output (`dist/`) and installed deps (`node_modules/`) — both
/// gitignored, so this is a belt-and-suspenders guard against a stray committed artifact needlessly
/// forcing an expensive `npm ci`.
pub fn is_ui_source(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(UI_DIR) else {
        return false;
    };
    !rest.starts_with("dist/") && !rest.starts_with("node_modules/")
}

/// Should the UI gate run, given the branch's changed files? True iff at least one is an
/// [`is_ui_source`] path. A pure-Rust (or docs/jobs) change returns `false`, so it never pays the
/// npm cost — the "skip when unchanged" requirement.
pub fn ui_gate_needed<I, S>(changed: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    changed.into_iter().any(|p| is_ui_source(p.as_ref()))
}

/// The ordered commands the UI gate runs when [`ui_gate_needed`]: a clean install (proves
/// `package.json` + lockfile are complete — the #505 near-miss), then the build (`tsc && vite
/// build`, catching type errors), then vitest (`vitest run`, non-watch). Playwright e2e is
/// deliberately omitted — too slow for an inline gate (left to CI / opt-in per #505). Callers run
/// them in order and stop at the first non-zero exit.
pub fn ui_gate_steps() -> Vec<UiStep> {
    vec![
        UiStep {
            label: "npm ci",
            args: vec!["--prefix", UI_PREFIX, "ci"],
        },
        UiStep {
            label: "npm run build (tsc + vite)",
            args: vec!["--prefix", UI_PREFIX, "run", "build"],
        },
        UiStep {
            label: "npm run test (vitest)",
            args: vec!["--prefix", UI_PREFIX, "run", "test"],
        },
    ]
}


