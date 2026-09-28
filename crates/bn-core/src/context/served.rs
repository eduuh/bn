//! `context-served.jsonl`: what context was handed to a seat, and separately, whether the seat
//! said it read it. Serving a pack does not prove an agent read it — delivery and comprehension
//! are different facts, so they are different lines.

use crate::resolver::{Config, Resolved};
use serde::{Deserialize, Serialize};
use std::io::Write;

/// One line of the log: a serve, or an acknowledgement of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Line {
    Served {
        ts: i64,
        /// The profile or piece served (`ship`, `ref:build-and-run#gotchas`).
        what: String,
        atoms: Vec<String>,
        digest: String,
        tokens: usize,
        /// `dispatch`, `cli`, `mcp`.
        consumer: String,
    },
    Acked {
        ts: i64,
        digest: String,
        by: String,
    },
}

/// A serve, with its acknowledgement if one came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Served {
    pub ts: i64,
    pub what: String,
    pub atoms: Vec<String>,
    pub digest: String,
    pub tokens: usize,
    pub consumer: String,
    pub acked_by: Option<String>,
    pub acked_at: Option<i64>,
}

fn path(cfg: &Config, ctx: &Resolved) -> std::path::PathBuf {
    cfg.note_dir(ctx).join("context-served.jsonl")
}

fn append(cfg: &Config, ctx: &Resolved, line: &Line) -> std::io::Result<()> {
    std::fs::create_dir_all(cfg.note_dir(ctx))?;
    let json = serde_json::to_string(line).map_err(std::io::Error::other)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path(cfg, ctx))?;
    writeln!(f, "{json}")
}

/// A short, stable fingerprint of `text` (FNV-1a, 48 bits in hex) — what an acknowledgement
/// names, so it says *which* context was read.
pub fn digest(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:012x}", h & 0xffff_ffff_ffff)
}

/// Log that `text` (from `what`, built of `atoms`) was served to `consumer`. Returns its digest.
pub fn record(
    cfg: &Config,
    ctx: &Resolved,
    what: &str,
    atoms: &[String],
    text: &str,
    consumer: &str,
) -> std::io::Result<String> {
    let d = digest(text);
    append(
        cfg,
        ctx,
        &Line::Served {
            ts: crate::util::now_epoch_secs(),
            what: what.to_string(),
            atoms: atoms.to_vec(),
            digest: d.clone(),
            tokens: crate::tokens::estimate(text),
            consumer: consumer.to_string(),
        },
    )?;
    Ok(d)
}

/// Log that `by` read the context with `digest`. Refuses a digest that was never served.
pub fn ack(cfg: &Config, ctx: &Resolved, digest: &str, by: &str) -> Result<(), String> {
    if !list(cfg, ctx).iter().any(|s| s.digest == digest) {
        return Err(format!("no served context with digest {digest}"));
    }
    append(
        cfg,
        ctx,
        &Line::Acked {
            ts: crate::util::now_epoch_secs(),
            digest: digest.to_string(),
            by: by.to_string(),
        },
    )
    .map_err(|e| e.to_string())
}

/// Every serve, oldest first, each with its acknowledgement (the first one) if any.
pub fn list(cfg: &Config, ctx: &Resolved) -> Vec<Served> {
    let lines: Vec<Line> = std::fs::read_to_string(path(cfg, ctx))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let mut out: Vec<Served> = Vec::new();
    for line in &lines {
        match line {
            Line::Served {
                ts,
                what,
                atoms,
                digest,
                tokens,
                consumer,
            } => out.push(Served {
                ts: *ts,
                what: what.clone(),
                atoms: atoms.clone(),
                digest: digest.clone(),
                tokens: *tokens,
                consumer: consumer.clone(),
                acked_by: None,
                acked_at: None,
            }),
            Line::Acked { ts, digest, by } => {
                for s in out
                    .iter_mut()
                    .filter(|s| &s.digest == digest && s.acked_by.is_none())
                {
                    s.acked_by = Some(by.clone());
                    s.acked_at = Some(*ts);
                }
            }
        }
    }
    out
}
