//! Which agent dialogs bn may answer without a human, and which it must escalate.
//!
//! Supervision has always had exactly one auto-answer rule, hardcoded: an orchestrated ship lane
//! dispatched with `--yolo` got its command approvals cleared with the session-scoped allow;
//! everything else raised a toast. That rule is right, but it is also the *only* one — an agent
//! dispatched outside an orchestrate run got nothing, and there was no way to say "clippy is fine,
//! `git push` is not" without editing Rust.
//!
//! This module is that rule made explicit and extensible. It is a **pure decision function** over a
//! parsed [`PendingPrompt`] and a policy read from `approvals.toml`, so what bn will and won't
//! approve on your behalf is a document you can read rather than behaviour you have to discover.
//!
//! The safety shape, in the order it is evaluated:
//!
//!   1. **Default deny.** [`decide`] approves nothing unless an explicit rule says so. With no
//!      policy file (or `enabled = false`), [`decide_with_builtin_default`] adds back exactly the
//!      one pre-existing rule and nothing more, so upgrading changes no behaviour. Write a policy
//!      and yours becomes the whole policy — the old permissive rule stops applying.
//!   2. **Never arbitrary questions.** A `question-menu` is a genuine decision — a design choice, a
//!      "which of these three?" — and is *never* auto-answered, at any setting. Only permission
//!      dialogs (`command-approval`) and trust menus are eligible.
//!   3. **Deny wins.** A deny rule matching the subject stops the decision dead, even if an allow
//!      rule also matches. Order in the file is irrelevant; there is no way to write a policy where
//!      a broad allow silently re-permits something you denied.
//!   4. **The option must exist.** The chosen answer is resolved against the options the harness
//!      actually offered, so a policy can never send a digit the dialog does not have.

use super::prompt::{PendingPrompt, PromptSemantic};
use crate::orchestrate::PaneState;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Which lanes a policy is allowed to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyScope {
    /// Only lanes owned by a running `bn orchestrate` wave (the historical behaviour).
    #[default]
    Orchestrated,
    /// Orchestrated lanes plus agents started by `bn dispatch`.
    Dispatched,
    /// Every supervised agent, including sessions bn never launched.
    All,
}

impl PolicyScope {
    fn admits(self, lane: LaneKind) -> bool {
        match self {
            PolicyScope::Orchestrated => lane == LaneKind::Orchestrated,
            PolicyScope::Dispatched => {
                matches!(lane, LaneKind::Orchestrated | LaneKind::Dispatched)
            }
            PolicyScope::All => true,
        }
    }
}

/// How a lane came to exist — what `scope` is matched against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneKind {
    /// An in-flight item of a `bn orchestrate` run.
    Orchestrated,
    /// Started by `bn dispatch` (a recorded launch, but no owning wave).
    Dispatched,
    /// Observed only — bn did not start it.
    Foreign,
}

/// How far an approval extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnswerScope {
    /// Allow this one request.
    Once,
    /// Take the session-scoped allow, so the harness stops asking for this class.
    #[default]
    Session,
}

impl AnswerScope {
    fn semantic(self) -> PromptSemantic {
        match self {
            AnswerScope::Once => PromptSemantic::Approve,
            AnswerScope::Session => PromptSemantic::ApproveSession,
        }
    }
}

/// One allow (or deny) rule: substrings matched case-insensitively against the dialog's subject.
///
/// Substrings, not regexes, on purpose. A rule you can misread is a rule that approves something
/// you did not mean, and `commands = ["cargo check"]` cannot be misread. A rule with no patterns
/// at all matches nothing — an empty rule is a typo, and it would otherwise mean "everything".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rule {
    /// Tool names (`Read`, `Bash`, `Edit`) as they appear in the dialog's title.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Command-line fragments (`cargo check`, `npm run lint`).
    #[serde(default)]
    pub commands: Vec<String>,
    /// How far the approval extends. Ignored on a deny rule.
    #[serde(default)]
    pub answer: AnswerScope,
}

impl Rule {
    fn patterns(&self) -> impl Iterator<Item = &String> {
        self.tools.iter().chain(self.commands.iter())
    }

    /// Whether this rule matches `subject`. Empty rules never match.
    fn matches(&self, subject: &str) -> Option<&String> {
        let lower = subject.to_ascii_lowercase();
        self.patterns()
            .filter(|p| !p.trim().is_empty())
            .find(|p| lower.contains(&p.to_ascii_lowercase()))
    }
}

/// The `[approve]` table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApproveTable {
    /// Master switch. **Off by default** — a policy file that exists but is not enabled still
    /// approves nothing, so writing rules and turning them on are two deliberate steps.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub scope: PolicyScope,
    /// Whether folder/MCP trust menus may be cleared automatically. `false` by default.
    #[serde(default)]
    pub trust_menu: bool,
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

/// The `[deny]` table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DenyTable {
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

/// The whole `approvals.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    #[serde(default)]
    pub approve: ApproveTable,
    #[serde(default)]
    pub deny: DenyTable,
}

impl ApprovalPolicy {
    /// Read the policy from `approvals.toml`. A missing file is the default (approve nothing).
    ///
    /// A **malformed** file is also the default, and says so on stderr. Degrading a policy file to
    /// "deny everything" is the only safe failure: the alternative — falling back to some partially
    /// parsed subset — could enable a rule whose matching deny rule failed to parse.
    pub fn load() -> ApprovalPolicy {
        Self::load_from(&crate::resolver::approvals_file_path())
    }

    pub fn load_from(path: &Path) -> ApprovalPolicy {
        let Ok(text) = std::fs::read_to_string(path) else {
            return ApprovalPolicy::default();
        };
        match toml::from_str::<ApprovalPolicy>(&text) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "bn: ignoring malformed {} ({e}) — auto-approval is off until it parses",
                    path.display()
                );
                ApprovalPolicy::default()
            }
        }
    }

    /// The scaffold `bn agent approvals --init` writes. Every rule is commented out: the file's job
    /// on day one is to show you the vocabulary, not to start approving things.
    pub fn template() -> &'static str {
        include_str!("approvals_template.toml")
    }
}

/// What bn knows about the lane a dialog belongs to.
#[derive(Debug, Clone, Copy)]
pub struct PolicyCtx {
    pub lane: LaneKind,
    /// Whether the lane carries the `--yolo` trust contract (an orchestrated coding/ship item).
    ///
    /// This is the historical gate, kept: research and scout lanes are not dispatched under a trust
    /// contract, so their approvals are a human's call even when a rule would match.
    pub trusted: bool,
}

/// The decision, with the reason it was reached — recorded verbatim in the audit log, because
/// "why did bn approve this?" must be answerable months later without re-deriving the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Auto { option: u8, reason: String },
    Escalate { reason: String },
}

impl ApprovalDecision {
    pub fn is_auto(&self) -> bool {
        matches!(self, ApprovalDecision::Auto { .. })
    }

    pub fn reason(&self) -> &str {
        match self {
            ApprovalDecision::Auto { reason, .. } | ApprovalDecision::Escalate { reason } => reason,
        }
    }
}

fn escalate(reason: impl Into<String>) -> ApprovalDecision {
    ApprovalDecision::Escalate {
        reason: reason.into(),
    }
}

/// [`decide`], falling back to bn's **built-in default** when no policy has been written.
///
/// The built-in default is the rule that predates this module and must keep working for anyone who
/// never writes an `approvals.toml`: an orchestrated `--yolo` ship lane's command approvals are
/// cleared with the session-scoped allow. It applies *only* while `[approve] enabled` is false —
/// once you write a policy, your policy is the whole policy, and a request it does not allow
/// escalates rather than quietly falling through to the old permissive rule.
pub fn decide_with_builtin_default(
    prompt: &PendingPrompt,
    ctx: &PolicyCtx,
    policy: &ApprovalPolicy,
) -> ApprovalDecision {
    let decision = decide(prompt, ctx, policy);
    if decision.is_auto() || policy.approve.enabled {
        return decision;
    }
    // The lane gate is not optional. The old rule only ever ran inside the orchestrate supervisor,
    // where every lane is orchestrated by construction — so "orchestrated" was implicit and unstated.
    // Reachable from the standalone pass, an unstated gate would silently extend the most permissive
    // rule bn has to agents it never launched.
    if ctx.lane != LaneKind::Orchestrated
        || prompt.kind != PaneState::CommandApproval
        || !ctx.trusted
    {
        return decision;
    }
    match prompt.option_for(PromptSemantic::ApproveSession) {
        Some(o) => ApprovalDecision::Auto {
            option: o.number,
            // Deliberately the session-scoped allow and never the one-shot "Yes": clearing the
            // class once is what keeps a ship lane moving, and it is the narrower commitment to
            // make automatically than re-deciding the same request every few minutes.
            reason: "built-in default: trusted ship lane, session-scoped allow".to_string(),
        },
        None => decision,
    }
}

/// Decide whether `prompt` may be answered without a human.
///
/// Pure: no I/O, no clock. Every gate below is a reason to escalate; reaching the end is the only
/// way to auto-answer.
pub fn decide(
    prompt: &PendingPrompt,
    ctx: &PolicyCtx,
    policy: &ApprovalPolicy,
) -> ApprovalDecision {
    // (2) An arbitrary decision is never bn's to make, whatever the file says. Checked before the
    // master switch so this is true even of a fully-enabled, wide-open policy.
    if prompt.kind == PaneState::QuestionMenu {
        return escalate("a question menu is a genuine decision — never auto-answered");
    }
    if !policy.approve.enabled {
        return escalate("auto-approval is off (approvals.toml `[approve] enabled`)");
    }
    if !policy.approve.scope.admits(ctx.lane) {
        return escalate(format!(
            "this lane is out of the configured scope ({:?})",
            policy.approve.scope
        ));
    }
    if !ctx.trusted {
        return escalate("this lane carries no trust contract (not a --yolo ship lane)");
    }

    // The text a rule is matched against: the subject when the dialog names one, plus the question,
    // which is where an unboxed harness puts the command ("Allow command: rm -rf build").
    let subject = match &prompt.subject {
        Some(s) => format!("{s} {}", prompt.question),
        None => prompt.question.clone(),
    };

    // (3) Deny first and unconditionally.
    if let Some(rule) = policy.deny.rules.iter().find_map(|r| r.matches(&subject)) {
        return escalate(format!("a deny rule matches ({rule:?})"));
    }

    if prompt.kind == PaneState::TrustMenu {
        if !policy.approve.trust_menu {
            return escalate("trust menus are not auto-cleared (`[approve] trust_menu`)");
        }
        return match prompt.resolve(PromptSemantic::Approve) {
            Some(option) => ApprovalDecision::Auto {
                option,
                reason: "trust menu, auto-cleared by policy".to_string(),
            },
            None => escalate("trust menu offers no approval option to pick"),
        };
    }

    let Some((rule, pattern)) = policy
        .approve
        .rules
        .iter()
        .find_map(|r| r.matches(&subject).map(|p| (r, p)))
    else {
        return escalate("no allow rule matches this request");
    };

    // (4) Only ever send an option the harness actually offered.
    match prompt.resolve(rule.answer.semantic()) {
        Some(option) => ApprovalDecision::Auto {
            option,
            reason: format!("allow rule matched {pattern:?}"),
        },
        None => escalate(format!(
            "allow rule matched {pattern:?} but the dialog offers no {} option",
            rule.answer.semantic().label()
        )),
    }
}
