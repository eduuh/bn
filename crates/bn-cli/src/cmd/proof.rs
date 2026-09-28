//! `bn proof` — the CLI over [`bn_core::proof`]: who is acting, which branch, which commit, then
//! the store call and a render. Readiness itself is derived in core.

use crate::cli::coord_group::ProofTarget;
use crate::*;
use bn_core::proof::{
    self, Freshness, ItemState, Judgement, Kind, NewProof, ProofError, Readiness,
};
use serde_json::json;

mod render;

const CMD: &str = "proof";

pub(crate) fn emit_proof(cfg: &Config, action: Option<ProofCmd>, json: bool) {
    let action = action.unwrap_or(ProofCmd::Show {
        id: None,
        freshness: "exact".into(),
        target: ProofTarget::default(),
    });
    match action {
        ProofCmd::Add {
            kind,
            for_,
            title,
            text,
            file,
            cmd,
            exit,
            sha,
            target,
        } => {
            let (ctx, who) = resolve(cfg, &target, json);
            let kind = Kind::parse(&kind).unwrap_or_else(|| {
                fail_json(json, CMD, "invalid_kind", &format!("unknown proof kind '{kind}'"))
            });
            let head = proof::head(cfg, &ctx);
            let new = NewProof {
                kind,
                for_: criteria(&for_),
                title,
                body: text.unwrap_or_default(),
                file,
                attach_bytes: None,
                truncated: false,
                sha: sha.or(head.sha),
                dirty: head.dirty,
                author: who,
                source: "bn proof add".into(),
                cmd,
                exit,
            };
            let p = proof::add(cfg, &ctx, &new, &bn_core::util::now_rfc3339_utc())
                .unwrap_or_else(|e| die(json, &e));
            if json {
                println!("{}", Envelope::success(CMD, json!(p)).to_json());
            } else {
                println!("{} added — {} for {}", p.id, p.kind, for_line(&p.for_));
                if p.for_.is_empty() {
                    println!("  link it to a criterion: bn proof link {} w1", p.id);
                }
                println!("  it counts once someone else accepts it: bn proof judge {} accept --reason …", p.id);
            }
        }
        ProofCmd::Judge {
            id,
            verdict,
            reason,
            target,
        } => {
            let (ctx, who) = resolve(cfg, &target, json);
            let verdict = Judgement::parse(&verdict).unwrap_or_else(|| {
                fail_json(json, CMD, "invalid_verdict", "verdict is accept, reject or withdraw")
            });
            let head = proof::head(cfg, &ctx);
            let v = proof::judge(
                cfg,
                &ctx,
                &id,
                &who,
                verdict,
                &reason,
                head.sha.as_deref(),
                &bn_core::util::now_rfc3339_utc(),
            )
            .unwrap_or_else(|e| die(json, &e));
            if json {
                println!("{}", Envelope::success(CMD, json!(v)).to_json());
            } else {
                println!("{} {} by {}", v.proof, v.verdict.as_str(), v.judge);
            }
        }
        ProofCmd::Link {
            id,
            criteria: ids,
            target,
        } => {
            let (ctx, who) = resolve(cfg, &target, json);
            let p = proof::link(cfg, &ctx, &id, &criteria(&ids), &who)
                .unwrap_or_else(|e| die(json, &e));
            if json {
                println!("{}", Envelope::success(CMD, json!(p)).to_json());
            } else {
                println!("{} now for {}", p.id, for_line(&p.for_));
            }
        }
        ProofCmd::Show {
            id,
            freshness,
            target,
        } => {
            let (ctx, _) = resolve(cfg, &target, json);
            let freshness = Freshness::parse(&freshness).unwrap_or_else(|| {
                fail_json(json, CMD, "invalid_freshness", "freshness is exact, ancestor or any")
            });
            let listing = proof::list(cfg, &ctx);
            let (verdicts, bad) = proof::verdicts(cfg, &ctx);
            let (r, head) = proof::readiness_now(cfg, &ctx, Some(freshness));
            match id.as_deref() {
                Some(one) if one.starts_with('p') => {
                    let p = listing.get(one).unwrap_or_else(|| {
                        die(json, &ProofError::NotFound(one.to_string()))
                    });
                    if json {
                        let mine: Vec<_> = verdicts.iter().filter(|v| v.proof == p.id).collect();
                        let data = json!({"proof": p, "verdicts": mine});
                        println!("{}", Envelope::success(CMD, data).to_json());
                    } else {
                        render::print_proof(p, &verdicts);
                    }
                }
                Some(one) => {
                    let item = r.item(one).unwrap_or_else(|| {
                        fail_json(json, CMD, "not_found", &format!("no criterion {one}"))
                    });
                    if json {
                        println!("{}", Envelope::success(CMD, json!(item)).to_json());
                    } else {
                        render::print_readiness(
                            &Readiness {
                                items: vec![item.clone()],
                                ready: matches!(item.state, ItemState::Accepted { .. }),
                            },
                            head.sha.as_deref(),
                            &[],
                            0,
                        );
                    }
                }
                None => {
                    if json {
                        let data = json!({"head": head.sha, "readiness": r,
                            "unregistered": listing.unregistered, "unreadable_verdicts": bad});
                        println!("{}", Envelope::success(CMD, data).to_json());
                    } else {
                        render::print_readiness(&r, head.sha.as_deref(), &listing.unregistered, bad);
                    }
                }
            }
        }
        ProofCmd::List { target } => {
            let (ctx, _) = resolve(cfg, &target, json);
            let listing = proof::list(cfg, &ctx);
            let (verdicts, _) = proof::verdicts(cfg, &ctx);
            if json {
                let data = json!({"proofs": listing.proofs, "verdicts": verdicts,
                    "unregistered": listing.unregistered});
                println!("{}", Envelope::success(CMD, data).to_json());
            } else {
                render::print_list(&listing, &verdicts);
            }
        }
    }
}

/// The note this acts on, and who is acting.
pub(crate) fn resolve(cfg: &Config, t: &ProofTarget, json: bool) -> (Resolved, String) {
    let ctx = target_ctx(cfg, t.repo.clone(), t.branch.clone()).unwrap_or_else(|| {
        fail_json(json, CMD, "no_target", "not in a git repo — pass --repo/--branch")
    });
    (ctx, identity(cfg, t.as_who.as_deref(), json))
}

/// See [`bn_core::proof::identity`].
pub(crate) fn identity(cfg: &Config, flag: Option<&str>, json: bool) -> String {
    let here = std::env::current_dir()
        .ok()
        .and_then(|cwd| resolver::resolve(cfg, &cwd).ok())
        .map(|r| format!("{}:{}", r.repo, r.branch));
    let env = std::env::var("BN_SEAT").ok();
    let user = std::env::var("USER").unwrap_or_default();
    let harness = bn_core::util::detect_harness().is_some();
    proof::identity(flag, env.as_deref(), harness, here.as_deref(), &user)
        .unwrap_or_else(|e| fail_json(json, CMD, "invalid_identity", &e.to_string()))
}

fn criteria(raw: &[String]) -> Vec<String> {
    raw.iter()
        .flat_map(|s| s.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn for_line(ids: &[String]) -> String {
    if ids.is_empty() {
        "no criteria yet".into()
    } else {
        ids.join(", ")
    }
}

fn die(json: bool, e: &ProofError) -> ! {
    let code = match e {
        ProofError::Io(_) => "io_error",
        ProofError::NotFound(_) => "not_found",
        ProofError::Forbidden(_) => "forbidden",
        ProofError::Invalid(_) => "invalid",
    };
    fail_json(json, CMD, code, &e.to_string())
}

/// A gate run to keep as proof.
pub(crate) struct Run<'a> {
    pub(crate) kind: Kind,
    pub(crate) job: &'a str,
    pub(crate) code: i32,
    pub(crate) log: &'a crate::cmd::verify::GateLog,
    pub(crate) sha: Option<String>,
    pub(crate) dirty: bool,
    pub(crate) for_: &'a [String],
}

/// Keep a verify/gate run as a proof with its (bounded) output attached, and return its id.
/// Best-effort: a branch with no note, or a store error, costs the proof — never the gate's
/// verdict. `[proof].capture = false` (or `BN_PROOF_CAPTURE=0`) turns it off.
pub(crate) fn record_run(cfg: &Config, ctx: &Resolved, run: Run<'_>) -> Option<String> {
    if !cfg.proof_capture
        || std::env::var("BN_PROOF_CAPTURE").is_ok_and(|v| v == "0")
        || !cfg.note_file(ctx).exists()
    {
        return None;
    }
    let (bytes, truncated) = run.log.lock().unwrap_or_else(|e| e.into_inner()).finish();
    let pass = run.code == 0;
    let verdict = if pass { "PASS" } else { "FAIL" };
    let new = NewProof {
        kind: run.kind,
        for_: criteria(run.for_),
        title: format!("{} {}: {verdict}", run.kind, run.job),
        body: format!(
            "`bn {}` ran `{}` and exited {}.{}",
            run.kind,
            run.job,
            run.code,
            if truncated {
                " The attached log is truncated in the middle."
            } else {
                ""
            }
        ),
        file: None,
        attach_bytes: Some((bytes, "log".into())),
        truncated,
        sha: run.sha,
        dirty: run.dirty,
        author: identity(cfg, None, false),
        source: format!("bn {}", run.kind),
        cmd: Some(run.job.to_string()),
        exit: Some(run.code),
    };
    match proof::add(cfg, ctx, &new, &bn_core::util::now_rfc3339_utc()) {
        Ok(p) => Some(p.id),
        Err(e) => {
            eprintln!("bn {}: run not kept as proof ({e})", run.kind);
            None
        }
    }
}
