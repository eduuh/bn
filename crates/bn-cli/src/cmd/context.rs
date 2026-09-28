//! `bn context` — context profiles for a branch: list them, compose one with its costs, or serve a
//! single piece. The composition itself is [`bn_core::context`].

use crate::cli::coord_group::CompactTarget;
use crate::*;
use bn_core::context::{self, Atom, AtomInputs, Composed};
use serde_json::json;
use std::path::PathBuf;

const CMD: &str = "context";

pub(crate) fn emit_context(cfg: &Config, action: Option<ContextCmd>, json: bool) {
    match action.unwrap_or(ContextCmd::List) {
        ContextCmd::List => {
            let m = manifest(cfg, json);
            let rows: Vec<_> = m
                .profile_names()
                .into_iter()
                .filter_map(|n| m.profile(&n).map(|p| (n, p)))
                .collect();
            if json {
                let data: Vec<_> = rows
                    .iter()
                    .map(|(n, p)| json!({"name": n, "atoms": p.atoms, "budget": p.budget}))
                    .collect();
                println!("{}", Envelope::success(CMD, json!({"profiles": data})).to_json());
            } else {
                for (n, p) in rows {
                    let budget = p.budget.map(|b| format!("  ≤{b} tok")).unwrap_or_default();
                    println!("{n:<16} {}{budget}", p.atoms.join(", "));
                }
            }
        }
        ContextCmd::Profile {
            name,
            budget,
            fit,
            render,
            target,
        } => {
            let ctx = ctx_for(cfg, &target, json);
            let c = compose(cfg, &ctx, &name, &[], budget, fit)
                .unwrap_or_else(|e| fail_json(json, CMD, "invalid", &e));
            if render {
                let text = c.text();
                let atoms: Vec<String> = c.pieces.iter().map(|p| p.atom.clone()).collect();
                let digest = served(cfg, &ctx, &name, &atoms, &text);
                print!("{text}");
                if let Some(d) = digest {
                    eprintln!("(served {d} — `bn context ack {d}` once read)");
                }
            } else if json {
                println!("{}", Envelope::success(CMD, json!(c)).to_json());
            } else {
                print_costs(&c);
            }
        }
        ContextCmd::Get { what, target } => {
            let ctx = ctx_for(cfg, &target, json);
            let text = get(cfg, &ctx, &what).unwrap_or_else(|e| fail_json(json, CMD, "invalid", &e));
            let digest = served(cfg, &ctx, &what, &[], &text);
            if json {
                let data = json!({"what": what, "digest": digest,
                    "tokens": bn_core::tokens::estimate(&text), "text": text});
                println!("{}", Envelope::success(CMD, data).to_json());
            } else {
                print!("{text}");
            }
        }
        ContextCmd::Ack {
            digest,
            as_who,
            target,
        } => {
            let ctx = ctx_for(cfg, &target, json);
            let by = as_who.unwrap_or_else(|| format!("{}:{}", ctx.repo, ctx.branch));
            bn_core::context::served::ack(cfg, &ctx, &digest, &by)
                .unwrap_or_else(|e| fail_json(json, CMD, "not_found", &e));
            if json {
                println!("{}", Envelope::success(CMD, json!({"digest": digest, "by": by})).to_json());
            } else {
                println!("{digest} acknowledged by {by}");
            }
        }
        ContextCmd::Served { target } => {
            let ctx = ctx_for(cfg, &target, json);
            let rows = bn_core::context::served::list(cfg, &ctx);
            if json {
                println!("{}", Envelope::success(CMD, json!({"served": rows})).to_json());
            } else if rows.is_empty() {
                println!("No context served to {}/{} yet.", ctx.repo, ctx.branch);
            } else {
                for s in rows {
                    let read = s.acked_by.map_or("not acknowledged".to_string(), |b| format!("read by {b}"));
                    println!(
                        "{}  {}  {:<24} {:>6} tok  via {:<8} {read}",
                        bn_core::util::fmt_local_short(s.ts),
                        s.digest,
                        s.what,
                        s.tokens,
                        s.consumer
                    );
                }
            }
        }
    }
}

/// Log a serve; best-effort, returning the digest when it was recorded.
fn served(cfg: &Config, ctx: &Resolved, what: &str, atoms: &[String], text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    bn_core::context::served::record(cfg, ctx, what, atoms, text, "cli").ok()
}

/// The checkout the manifest lives in: the branch's own worktree, else this directory's repo.
pub(crate) fn repo_root(cfg: &Config, ctx: &Resolved) -> Option<PathBuf> {
    context::root_for(cfg, ctx).or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|d| bn_core::resolver::repo_root(&d))
    })
}

fn manifest(cfg: &Config, json: bool) -> context::Manifest {
    let root = target_ctx(cfg, None, None).and_then(|ctx| repo_root(cfg, &ctx));
    context::load(root.as_deref()).unwrap_or_else(|e| fail_json(json, CMD, "manifest", &e))
}

/// Compose `profile` for `ctx`. `global_refs` is a dispatch's `--ref` selection.
pub(crate) fn compose(
    cfg: &Config,
    ctx: &Resolved,
    profile: &str,
    global_refs: &[String],
    budget: Option<usize>,
    fit: bool,
) -> Result<Composed, String> {
    let root = repo_root(cfg, ctx);
    let m = context::load(root.as_deref())?;
    let inputs = AtomInputs {
        cfg,
        ctx,
        root: root.as_deref(),
        global_refs,
    };
    context::compose(&m, profile, &inputs, budget, fit)
}

/// One piece by reference — see [`ContextCmd::Get`].
pub(crate) fn get(cfg: &Config, ctx: &Resolved, what: &str) -> Result<String, String> {
    let root = repo_root(cfg, ctx);
    let m = context::load(root.as_deref())?;
    let inputs = AtomInputs {
        cfg,
        ctx,
        root: root.as_deref(),
        global_refs: &[],
    };
    let (kind, rest) = what
        .split_once(':')
        .ok_or("say what to get: profile:, atom:, ref:, skill: or file:")?;
    let atom = match kind {
        "profile" => return context::compose(&m, rest, &inputs, None, false).map(|c| c.text()),
        "atom" => m.atom(rest).ok_or(format!("no atom '{rest}'"))?,
        "ref" => Atom::Ref { r: rest.to_string() },
        "skill" => Atom::Skill { name: rest.to_string() },
        "file" => Atom::File { path: rest.to_string() },
        other => return Err(format!("unknown kind '{other}'")),
    };
    Ok(context::render_atom(&atom, &inputs)?.unwrap_or_default())
}

fn print_costs(c: &Composed) {
    for p in &c.pieces {
        println!("  {:<18} {:>6} tok", p.atom, p.tokens);
    }
    for e in &c.empty {
        println!("  {e:<18}      – (nothing to say)");
    }
    for d in &c.dropped {
        println!("  {d:<18}      ✂ dropped to fit");
    }
    for m in &c.missing {
        println!("  ! {m}");
    }
    let budget = c.budget.map(|b| format!(" of {b}")).unwrap_or_default();
    println!("{}: {} tok{budget}", c.profile, c.total);
    if c.overage > 0 {
        println!("⚠ {} tok over budget — trim the profile, or --fit to drop from the end", c.overage);
    }
}

fn ctx_for(cfg: &Config, t: &CompactTarget, json: bool) -> Resolved {
    target_ctx(cfg, t.repo.clone(), t.branch.clone())
        .unwrap_or_else(|| fail_json(json, CMD, "no_target", "not in a git repo — pass --repo/--branch"))
}
