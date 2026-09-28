//! The proof and verdict records.

use serde::{Deserialize, Serialize};
use std::fmt;

/// What sort of evidence a proof is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// `bn verify` output.
    Verify,
    /// `bn gate` output (verify on the rebased head).
    Gate,
    /// A focused test run.
    Test,
    Screenshot,
    Log,
    /// A review or critique.
    Review,
    /// Anything else a person or agent attests to.
    Manual,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Verify,
        Kind::Gate,
        Kind::Test,
        Kind::Screenshot,
        Kind::Log,
        Kind::Review,
        Kind::Manual,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Verify => "verify",
            Kind::Gate => "gate",
            Kind::Test => "test",
            Kind::Screenshot => "screenshot",
            Kind::Log => "log",
            Kind::Review => "review",
            Kind::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s.trim())
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A proof's frontmatter. Authored only from flags, never by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proof {
    pub id: String,
    pub kind: Kind,
    /// The `done-when` criteria (`w<N>`) this is evidence for.
    #[serde(default, rename = "for")]
    pub for_: Vec<String>,
    pub title: String,
    /// The commit the evidence was taken at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    /// Whether the worktree had uncommitted changes at the time.
    #[serde(default)]
    pub dirty: bool,
    pub author: String,
    pub created: String,
    /// What produced it: `bn verify`, `bn gate`, `bn proof add`.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// The attached file's name, beside this one in `proof/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// The markdown body after the frontmatter.
    #[serde(skip)]
    pub body: String,
    /// The proof file's name.
    #[serde(skip)]
    pub file: String,
}

impl Proof {
    pub fn num(&self) -> u32 {
        super::parse_pid(&self.id).unwrap_or(0)
    }

    /// A failed run is still evidence (of failure); it just can't prove anything done.
    pub fn failed(&self) -> bool {
        self.exit.is_some_and(|c| c != 0)
    }
}

/// A proof to add.
#[derive(Debug, Clone)]
pub struct NewProof {
    pub kind: Kind,
    pub for_: Vec<String>,
    pub title: String,
    pub body: String,
    /// A file to copy in as the attachment (never moved).
    pub file: Option<std::path::PathBuf>,
    /// Or: attachment contents to write directly (a captured log), with its extension.
    pub attach_bytes: Option<(Vec<u8>, String)>,
    pub truncated: bool,
    pub sha: Option<String>,
    pub dirty: bool,
    pub author: String,
    pub source: String,
    pub cmd: Option<String>,
    pub exit: Option<i32>,
}

/// A judge's call on a proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Judgement {
    Accept,
    Reject,
    /// The author takes it back; it stops counting either way.
    Withdraw,
}

impl Judgement {
    pub fn parse(s: &str) -> Option<Judgement> {
        match s.trim() {
            "accept" => Some(Judgement::Accept),
            "reject" => Some(Judgement::Reject),
            "withdraw" => Some(Judgement::Withdraw),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Judgement::Accept => "accept",
            Judgement::Reject => "reject",
            Judgement::Withdraw => "withdraw",
        }
    }
}

/// One line of `verdicts.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub proof: String,
    pub judge: String,
    pub verdict: Judgement,
    pub reason: String,
    /// The branch head when the judgement was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    pub ts: String,
}
