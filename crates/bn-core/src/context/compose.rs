//! Composing a profile: render its atoms in order, count, and fit to the budget.

use super::atoms::{render_atom, AtomInputs};
use super::manifest::Manifest;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Piece {
    pub atom: String,
    pub tokens: usize,
    #[serde(skip)]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Composed {
    pub profile: String,
    pub pieces: Vec<Piece>,
    pub total: usize,
    pub budget: Option<usize>,
    /// Tokens over budget (0 when within it or unbudgeted).
    pub overage: usize,
    /// Atoms that had nothing to say.
    pub empty: Vec<String>,
    /// Atoms that couldn't render, with why.
    pub missing: Vec<String>,
    /// Atoms dropped (from the end) to fit the budget.
    pub dropped: Vec<String>,
}

impl Composed {
    /// The composed text, in atom order.
    pub fn text(&self) -> String {
        self.pieces.iter().map(|p| p.text.as_str()).collect()
    }
}

/// Compose `profile`. `budget` overrides the profile's; `fit` drops atoms from the end until the
/// rest fits, rather than only warning.
pub fn compose(
    manifest: &Manifest,
    profile: &str,
    inputs: &AtomInputs<'_>,
    budget: Option<usize>,
    fit: bool,
) -> Result<Composed, String> {
    let p = manifest.profile(profile).ok_or_else(|| {
        format!(
            "no profile '{profile}' (have: {})",
            manifest.profile_names().join(", ")
        )
    })?;
    let mut out = Composed {
        profile: profile.to_string(),
        pieces: vec![],
        total: 0,
        budget: budget.or(p.budget),
        overage: 0,
        empty: vec![],
        missing: vec![],
        dropped: vec![],
    };
    for name in &p.atoms {
        let Some(atom) = manifest.atom(name) else {
            out.missing.push(format!("{name}: no such atom"));
            continue;
        };
        match render_atom(&atom, inputs) {
            Ok(Some(text)) => out.pieces.push(Piece {
                atom: name.clone(),
                tokens: crate::tokens::estimate(&text),
                text,
            }),
            Ok(None) => out.empty.push(name.clone()),
            Err(why) => out.missing.push(format!("{name}: {why}")),
        }
    }
    let total = |pieces: &[Piece]| pieces.iter().map(|p| p.tokens).sum::<usize>();
    if let (true, Some(b)) = (fit, out.budget) {
        while total(&out.pieces) > b {
            match out.pieces.pop() {
                Some(p) => out.dropped.insert(0, p.atom),
                None => break,
            }
        }
    }
    out.total = total(&out.pieces);
    out.overage = out.budget.map_or(0, |b| out.total.saturating_sub(b));
    Ok(out)
}
