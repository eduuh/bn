//! `<repo>/.bn/context.toml`: custom atoms and profiles.

use super::atoms::Atom;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// A named, ordered list of atoms with a token budget.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Profile {
    pub atoms: Vec<String>,
    #[serde(default)]
    pub budget: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub atoms: BTreeMap<String, Atom>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

/// The profiles every repo has. `ship` and `scout` are what `bn dispatch` seeds an agent with;
/// `compact-restore` is what a session reads coming back from a compaction.
pub const BUILTIN_PROFILES: &[(&str, &[&str], usize)] = &[
    ("ship", &["global-refs", "ref-index", "directives"], 4000),
    ("scout", &["global-refs", "ref-index", "directives"], 4000),
    (
        "compact-restore",
        &["directives", "recap", "proof", "handoff"],
        8000,
    ),
    (
        "orient",
        &["directives", "goal", "proof", "ref-index"],
        4000,
    ),
];

impl Manifest {
    /// A profile by name: the manifest's, else a built-in.
    pub fn profile(&self, name: &str) -> Option<Profile> {
        self.profiles.get(name).cloned().or_else(|| {
            BUILTIN_PROFILES
                .iter()
                .find(|(n, _, _)| *n == name)
                .map(|(_, atoms, budget)| Profile {
                    atoms: atoms.iter().map(|a| a.to_string()).collect(),
                    budget: Some(*budget),
                })
        })
    }

    /// Every profile name, built-ins included, sorted.
    pub fn profile_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.profiles.keys().cloned().collect();
        for (n, _, _) in BUILTIN_PROFILES {
            if !names.iter().any(|x| x == n) {
                names.push(n.to_string());
            }
        }
        names.sort();
        names
    }

    /// An atom by name: a declared one, else a built-in kind of the same name.
    pub fn atom(&self, name: &str) -> Option<Atom> {
        self.atoms
            .get(name)
            .cloned()
            .or_else(|| Atom::builtin(name))
    }
}

/// Load `<root>/.bn/context.toml`, overlaid by `.bn/context.local.toml` (its atoms and profiles
/// replace same-named ones). Missing files are an empty manifest; a malformed one is an error.
pub fn load(root: Option<&Path>) -> Result<Manifest, String> {
    let mut m = Manifest::default();
    let Some(root) = root else {
        return Ok(m);
    };
    for file in ["context.toml", "context.local.toml"] {
        let path = root.join(".bn").join(file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let layer: Manifest =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        m.atoms.extend(layer.atoms);
        m.profiles.extend(layer.profiles);
    }
    Ok(m)
}
