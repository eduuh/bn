//! The atoms and how each renders.

use crate::proof::ItemState;
use crate::references::{self, Scope};
use crate::resolver::{Config, Resolved};
use serde::Deserialize;
use std::path::Path;

/// One piece of context.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Atom {
    /// My coding standards, by name (the dispatch `--ref` selection, or all of them).
    GlobalRefs,
    /// This repo's reference notes, by name.
    RefIndex,
    /// The branch's standing directives.
    Directives,
    Goal,
    /// Done-when readiness.
    Proof,
    /// `RECAP.md`.
    Recap,
    /// The handoff orientation.
    Handoff,
    /// The seat's open queue rows.
    Queue,
    /// One reference, or one section of it: `name`, `name#section`, `global:name#section`.
    Ref {
        #[serde(rename = "ref")]
        r: String,
    },
    /// A skill's rendered body.
    Skill {
        name: String,
    },
    /// A repo file, or one `## ` section of it: `docs/ARCH.md#Overview`.
    File {
        path: String,
    },
}

impl Atom {
    /// The built-in atom a bare name stands for (`directives`, `ref-index`, …).
    pub fn builtin(name: &str) -> Option<Atom> {
        Some(match name {
            "global-refs" => Atom::GlobalRefs,
            "ref-index" => Atom::RefIndex,
            "directives" => Atom::Directives,
            "goal" => Atom::Goal,
            "proof" => Atom::Proof,
            "recap" => Atom::Recap,
            "handoff" => Atom::Handoff,
            "queue" => Atom::Queue,
            _ => return None,
        })
    }
}

/// What rendering needs.
pub struct AtomInputs<'a> {
    pub cfg: &'a Config,
    pub ctx: &'a Resolved,
    /// The repo checkout, for `skill` and `file` atoms.
    pub root: Option<&'a Path>,
    /// The dispatch `--ref` selection for `global-refs` (empty: every standard).
    pub global_refs: &'a [String],
}

/// Render `atom`. `Ok(None)` when it has nothing to say (no directives, no recap); `Err` when a
/// named thing is missing (an unknown reference, section, skill or file).
pub fn render_atom(atom: &Atom, i: &AtomInputs<'_>) -> Result<Option<String>, String> {
    Ok(match atom {
        Atom::GlobalRefs => global_refs(i.global_refs),
        Atom::RefIndex => ref_index(&i.ctx.repo),
        Atom::Directives => directives(i.cfg, i.ctx),
        Atom::Goal => crate::store::read(&i.cfg.note_file(i.ctx))
            .ok()
            .and_then(|n| crate::render::goal_text(&n.body))
            .map(|g| format!("\n\nGoal for this branch: {g}\n")),
        Atom::Proof => proof(i.cfg, i.ctx),
        Atom::Recap => std::fs::read_to_string(crate::compact::recap_path(i.cfg, i.ctx))
            .ok()
            .filter(|t| !t.trim().is_empty())
            .map(|t| format!("\n\n{}\n", t.trim_end())),
        Atom::Handoff => crate::handoff::read(i.cfg, i.ctx)
            .map(|h| format!("\n\n{}", crate::handoff::render_orientation(&h))),
        Atom::Queue => queue(i.ctx),
        Atom::Ref { r } => Some(reference(&i.ctx.repo, r)?),
        Atom::Skill { name } => {
            let root = i.root.ok_or("a skill atom needs a repo checkout")?;
            let body = crate::skills::render_required(i.cfg, root, name, None)
                .map_err(|e| format!("skill {name}: {e}"))?;
            Some(format!("\n\n{}\n", body.trim_end()))
        }
        Atom::File { path } => Some(file(i.root, path)?),
    })
}

/// The coding-standards pointer `bn dispatch` has always seeded (#623): `refs` is the `--ref`
/// selection (comma lists allowed, `-` for none); empty means every standard.
pub fn global_refs(refs: &[String]) -> Option<String> {
    let mut tokens: Vec<String> = Vec::new();
    for r in refs {
        for part in r.split(',') {
            let t = part.trim();
            if t == "-" {
                return None;
            }
            if !t.is_empty() {
                tokens.push(t.to_string());
            }
        }
    }
    let names: Vec<String> = if tokens.is_empty() {
        references::names()
    } else {
        let mut out: Vec<String> = Vec::new();
        for t in &tokens {
            if let Some(r) = references::resolve(t) {
                if !out.contains(&r.name) {
                    out.push(r.name);
                }
            }
        }
        out
    };
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "\n\nApply these references (my coding standards) when writing code, and check your \
         changes against them — run `bn ref <name>` for the full text of each: {}.",
        names.join(", ")
    ))
}

/// This repo's reference notes, by name only — an index a few dozen notes long costs a couple of
/// hundred tokens, so every agent can afford to know what exists.
pub fn ref_index(repo: &str) -> Option<String> {
    let names = references::names_in(&Scope::repo(repo));
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "\n\nThis repo's reference notes — what past sessions discovered about it (how it builds, \
         where the seams are, which failures mislead). Read the one that bears on your task with \
         `bn ref <name> --repo`, and when you learn something durable, record it with \
         `bn ref append <name> --repo --section gotchas \"<fact> (src: …, <date>)\"`: {}.",
        names.join(", ")
    ))
}

/// The branch note's standing directives.
pub fn directives(cfg: &Config, ctx: &Resolved) -> Option<String> {
    let note = crate::store::read(&cfg.note_file(ctx)).ok()?;
    let items: Vec<String> = note
        .front
        .directives
        .iter()
        .map(|d| d.text.clone())
        .collect();
    if items.is_empty() {
        return None;
    }
    let list = items
        .iter()
        .map(|d| format!("\n- {d}"))
        .collect::<Vec<_>>()
        .join("");
    Some(format!(
        "\n\nSTANDING INSTRUCTIONS for this branch — follow these for everything you do here. They \
         were recorded with `bn agent instruct` and live in this branch's note under \
         `## Directives`, so they apply to every session on this branch, not just this one:{list}\n"
    ))
}

fn proof(cfg: &Config, ctx: &Resolved) -> Option<String> {
    let (r, _) = crate::proof::readiness_now(cfg, ctx, None);
    if r.items.is_empty() {
        return None;
    }
    let lines: Vec<String> = r
        .items
        .iter()
        .map(|i| {
            let s = match &i.state {
                ItemState::Accepted { proof, .. } => format!("accepted ({proof})"),
                ItemState::Stale { proof, .. } => format!("stale ({proof})"),
                ItemState::Attached { proofs } => {
                    format!("awaiting a verdict ({})", proofs.join(", "))
                }
                ItemState::Rejected { proof, .. } => format!("rejected ({proof})"),
                ItemState::Unproven => "unproven".into(),
            };
            format!("\n- {} {} — {s}", i.id, i.text)
        })
        .collect();
    Some(format!(
        "\n\nDone-when readiness (from recorded evidence, at HEAD):{}\n",
        lines.concat()
    ))
}

fn queue(ctx: &Resolved) -> Option<String> {
    let seat = crate::coord::normalize_seat(&format!("{}:{}", ctx.repo, ctx.branch)).ok()?;
    let rows = crate::coord::CoordDb::open_default()
        .ok()?
        .list(&crate::coord::ListFilter {
            seat: Some(seat),
            ..Default::default()
        })
        .ok()?;
    if rows.is_empty() {
        return None;
    }
    let lines: Vec<String> = rows
        .iter()
        .map(|q| {
            format!(
                "\n- {} {} from {}: {}",
                q.id,
                q.state,
                q.source,
                q.body.lines().next().unwrap_or("")
            )
        })
        .collect();
    Some(format!(
        "\n\nQueue rows for this branch (claim before starting: `bn queue claim <q>`):{}\n",
        lines.concat()
    ))
}

fn reference(repo: &str, spec: &str) -> Result<String, String> {
    let (scope, rest) = match spec.strip_prefix("global:") {
        Some(rest) => (Scope::Global, rest),
        None => (Scope::repo(repo), spec),
    };
    let (name, section) = references::split_ref(rest);
    let (r, body) = references::read_in(&scope, name).ok_or(format!("no reference '{name}'"))?;
    let text = match section {
        Some(s) => {
            references::section_of(&body, s).ok_or(format!("{} has no section '{s}'", r.name))?
        }
        None => body,
    };
    Ok(format!("\n\n{}\n", text.trim_end()))
}

fn file(root: Option<&Path>, spec: &str) -> Result<String, String> {
    let root = root.ok_or("a file atom needs a repo checkout")?;
    let (path, section) = references::split_ref(spec);
    if path.split(['/', '\\']).any(|seg| seg == "..") || Path::new(path).is_absolute() {
        return Err(format!("{path}: file atoms stay inside the repo"));
    }
    let body = std::fs::read_to_string(root.join(path)).map_err(|e| format!("{path}: {e}"))?;
    let text = match section {
        Some(s) => {
            references::section_of(&body, s).ok_or(format!("{path} has no section '{s}'"))?
        }
        None => body,
    };
    Ok(format!("\n\n{}\n", text.trim_end()))
}
