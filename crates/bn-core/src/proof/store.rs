//! Reading and writing `<note_dir>/proof/`.

use super::model::{Judgement, NewProof, Proof, Verdict};
use super::{parse_pid, pid};
use crate::resolver::{Config, Resolved};
use crate::store::{self, FileLock};
use std::fmt;
use std::io::Write;
use std::path::PathBuf;

const VERDICTS: &str = "verdicts.jsonl";

/// Why a proof call failed.
#[derive(Debug)]
pub enum ProofError {
    Io(std::io::Error),
    NotFound(String),
    /// The actor may not do this (e.g. judge their own proof).
    Forbidden(String),
    Invalid(String),
}

impl fmt::Display for ProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProofError::Io(e) => write!(f, "proof store: {e}"),
            ProofError::NotFound(id) => write!(f, "no proof {id}"),
            ProofError::Forbidden(m) | ProofError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ProofError {}

impl From<std::io::Error> for ProofError {
    fn from(e: std::io::Error) -> Self {
        ProofError::Io(e)
    }
}

type Result<T> = std::result::Result<T, ProofError>;

/// `<note_dir>/proof`.
pub fn dir(cfg: &Config, ctx: &Resolved) -> PathBuf {
    cfg.note_dir(ctx).join("proof")
}

/// Everything in `proof/`: the registered proofs (by id), and any file nobody registered.
#[derive(Debug, Default)]
pub struct Listing {
    pub proofs: Vec<Proof>,
    /// Files in `proof/` that are neither a proof, an attachment, nor the verdict log. They never
    /// count toward readiness: evidence dropped in by hand is unverified by construction.
    pub unregistered: Vec<String>,
}

impl Listing {
    pub fn get(&self, id: &str) -> Option<&Proof> {
        let n = parse_pid(id)?;
        self.proofs.iter().find(|p| p.num() == n)
    }
}

fn parse_proof(file: &str, text: &str) -> Option<Proof> {
    let (front, body) = store::split_frontmatter(text)?;
    let mut p: Proof = serde_yaml::from_str(front).ok()?;
    p.body = body.trim().to_string();
    p.file = file.to_string();
    (parse_pid(&p.id).is_some()).then_some(p)
}

/// Read `proof/`.
pub fn list(cfg: &Config, ctx: &Resolved) -> Listing {
    let mut out = Listing::default();
    let Ok(entries) = std::fs::read_dir(dir(cfg, ctx)) else {
        return out;
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut other = Vec::new();
    for name in names {
        let parsed = name
            .ends_with(".md")
            .then(|| std::fs::read_to_string(dir(cfg, ctx).join(&name)).ok())
            .flatten()
            .and_then(|text| parse_proof(&name, &text));
        match parsed {
            Some(p) => out.proofs.push(p),
            None => other.push(name),
        }
    }
    let attached: Vec<&str> = out
        .proofs
        .iter()
        .filter_map(|p| p.attachment.as_deref())
        .collect();
    out.unregistered = other
        .into_iter()
        .filter(|n| n != VERDICTS && !n.starts_with('.') && !attached.contains(&n.as_str()))
        .collect();
    out.proofs.sort_by_key(Proof::num);
    out
}

/// The verdict log in order, plus how many lines could not be read (reported, never fatal).
pub fn verdicts(cfg: &Config, ctx: &Resolved) -> (Vec<Verdict>, usize) {
    let Ok(text) = std::fs::read_to_string(dir(cfg, ctx).join(VERDICTS)) else {
        return (vec![], 0);
    };
    let mut bad = 0;
    let rows = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let v = serde_json::from_str::<Verdict>(l).ok();
            bad += usize::from(v.is_none());
            v
        })
        .collect();
    (rows, bad)
}

/// Refuse `for` ids that aren't this note's `done-when` criteria.
fn check_criteria(cfg: &Config, ctx: &Resolved, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let note = store::read(&cfg.note_file(ctx))
        .map_err(|_| ProofError::Invalid("no branch note to attach proof to".into()))?;
    let known: Vec<&str> = note.front.done_when.iter().map(|w| w.id.as_str()).collect();
    match ids.iter().find(|id| !known.contains(&id.as_str())) {
        Some(bad) => Err(ProofError::Invalid(format!(
            "{bad} is not a done-when criterion on this branch (have: {})",
            if known.is_empty() {
                "none — add one with `bn add done-when …`".to_string()
            } else {
                known.join(", ")
            }
        ))),
        None => Ok(()),
    }
}

fn slug(title: &str) -> String {
    let s: String = title
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let s: String = s.chars().take(40).collect();
    if s.is_empty() {
        "proof".into()
    } else {
        s.trim_end_matches('-').to_string()
    }
}

fn write_proof(cfg: &Config, ctx: &Resolved, p: &Proof) -> Result<()> {
    let front = serde_yaml::to_string(p).map_err(|e| ProofError::Invalid(e.to_string()))?;
    let text = format!("---\n{front}---\n\n{}\n", p.body.trim());
    store::write_atomic(&dir(cfg, ctx).join(&p.file), text.as_bytes())?;
    Ok(())
}

fn refresh_summary(cfg: &Config, ctx: &Resolved) {
    let listing = list(cfg, ctx);
    let (verdicts, _) = verdicts(cfg, ctx);
    let md = super::render_proof_md(&ctx.repo, &ctx.branch, &listing, &verdicts);
    crate::util::best_effort(
        store::write_atomic(&cfg.note_dir(ctx).join("PROOF.md"), md.as_bytes()),
        "proof: refresh PROOF.md",
    );
}

/// Register a proof. The id is the next free `p<N>`; an attachment is copied in beside it.
pub fn add(cfg: &Config, ctx: &Resolved, new: &NewProof, now: &str) -> Result<Proof> {
    if new.title.trim().is_empty() {
        return Err(ProofError::Invalid("a proof needs a title".into()));
    }
    check_criteria(cfg, ctx, &new.for_)?;
    let d = dir(cfg, ctx);
    std::fs::create_dir_all(&d)?;
    let _lock = FileLock::acquire_required(&d.join(".proof"))?;
    let n = list(cfg, ctx)
        .proofs
        .iter()
        .map(Proof::num)
        .max()
        .unwrap_or(0)
        + 1;
    let stem = format!("{}-{}", pid(n), slug(&new.title));
    let mut bytes = None;
    let attachment = match (&new.file, &new.attach_bytes) {
        (Some(src), _) => {
            let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("bin");
            // `-file` keeps an attached `.md` from landing on the proof's own `{stem}.md`.
            let name = format!("{stem}-file.{ext}");
            bytes = Some(std::fs::copy(src, d.join(&name))?);
            Some(name)
        }
        (None, Some((data, ext))) => {
            let name = format!("{stem}-file.{ext}");
            store::write_atomic(&d.join(&name), data)?;
            bytes = Some(data.len() as u64);
            Some(name)
        }
        (None, None) => None,
    };
    let proof = Proof {
        id: pid(n),
        kind: new.kind,
        for_: new.for_.clone(),
        title: new.title.trim().to_string(),
        sha: new.sha.clone(),
        dirty: new.dirty,
        author: new.author.clone(),
        created: now.to_string(),
        source: new.source.clone(),
        cmd: new.cmd.clone(),
        exit: new.exit,
        attachment,
        bytes,
        truncated: new.truncated,
        body: new.body.clone(),
        file: format!("{stem}.md"),
    };
    write_proof(cfg, ctx, &proof)?;
    drop(_lock);
    refresh_summary(cfg, ctx);
    Ok(proof)
}

/// Record `judge`'s verdict on proof `id`. The author may only withdraw; everyone else may only
/// accept or reject.
#[allow(clippy::too_many_arguments)]
pub fn judge(
    cfg: &Config,
    ctx: &Resolved,
    id: &str,
    judge: &str,
    verdict: Judgement,
    reason: &str,
    head: Option<&str>,
    now: &str,
) -> Result<Verdict> {
    if reason.trim().is_empty() {
        return Err(ProofError::Invalid("a verdict needs a reason".into()));
    }
    let listing = list(cfg, ctx);
    let proof = listing
        .get(id)
        .ok_or_else(|| ProofError::NotFound(id.to_string()))?;
    let own = proof.author == judge;
    match verdict {
        Judgement::Withdraw if !own => {
            return Err(ProofError::Forbidden(format!(
                "only its author ({}) can withdraw {}",
                proof.author, proof.id
            )))
        }
        Judgement::Accept | Judgement::Reject if own => {
            return Err(ProofError::Forbidden(format!(
                "{judge} wrote {}; someone else has to judge it",
                proof.id
            )))
        }
        _ => {}
    }
    let v = Verdict {
        proof: proof.id.clone(),
        judge: judge.to_string(),
        verdict,
        reason: reason.trim().to_string(),
        sha: head.map(str::to_string),
        ts: now.to_string(),
    };
    let d = dir(cfg, ctx);
    let _lock = FileLock::acquire_required(&d.join(".proof"))?;
    let line = serde_json::to_string(&v).map_err(|e| ProofError::Invalid(e.to_string()))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(d.join(VERDICTS))?;
    writeln!(f, "{line}")?;
    drop(_lock);
    refresh_summary(cfg, ctx);
    Ok(v)
}

/// Add criteria to proof `id`'s `for` list. Author only.
pub fn link(
    cfg: &Config,
    ctx: &Resolved,
    id: &str,
    criteria: &[String],
    actor: &str,
) -> Result<Proof> {
    check_criteria(cfg, ctx, criteria)?;
    let d = dir(cfg, ctx);
    let _lock = FileLock::acquire_required(&d.join(".proof"))?;
    let listing = list(cfg, ctx);
    let mut proof = listing
        .get(id)
        .cloned()
        .ok_or_else(|| ProofError::NotFound(id.to_string()))?;
    if proof.author != actor {
        return Err(ProofError::Forbidden(format!(
            "only its author ({}) can link {}",
            proof.author, proof.id
        )));
    }
    for c in criteria {
        if !proof.for_.contains(c) {
            proof.for_.push(c.clone());
        }
    }
    write_proof(cfg, ctx, &proof)?;
    drop(_lock);
    refresh_summary(cfg, ctx);
    Ok(proof)
}
