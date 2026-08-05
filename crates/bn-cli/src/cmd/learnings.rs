//! `bn learnings` — captured learnings + human review decisions.
use crate::*;

/// `bn learnings` — render the windowed, filtered, grouped fleet-learning inspection view (#322).
pub(crate) fn emit_learnings(cfg: &Config, filter: &bn_core::learnings::Filter, as_json: bool) {
    let view = bn_core::learnings::collect(cfg, &today(), filter);
    if as_json {
        let data = serde_json::to_value(&view).unwrap_or(serde_json::Value::Null);
        println!("{}", Envelope::success("learnings", data).to_json());
    } else {
        print!("{}", bn_core::learnings::render(&view));
    }
}

/// `bn learnings promote|dismiss <key|substring>` — record a human review decision, or report an
/// unresolvable/ambiguous query (listing the candidates so the overseer can rerun with a key).
pub(crate) fn emit_learnings_review(
    cfg: &Config,
    query: &str,
    state: bn_core::learnings::Review,
    note: Option<&str>,
    as_json: bool,
) {
    use bn_core::learnings::ReviewError;
    if query.trim().is_empty() {
        fail("bn learnings: a learning key or text substring is required");
    }
    match bn_core::learnings::review(cfg, &today(), query, state, note) {
        Ok(entry) => {
            if as_json {
                let data = serde_json::to_value(&entry).unwrap_or(serde_json::Value::Null);
                println!("{}", Envelope::success("learnings", data).to_json());
            } else {
                println!("{} {}  {}", state.label(), entry.key, entry.text);
            }
        }
        Err(ReviewError::NotFound) => {
            if as_json {
                println!(
                    "{}",
                    Envelope::failure(
                        "learnings",
                        "not_found",
                        format!("no learning matched {query:?}")
                    )
                    .to_json()
                );
                std::process::exit(1);
            }
            fail(&format!("bn learnings: no learning matched {query:?}"));
        }
        Err(ReviewError::Ambiguous(cands)) => {
            if as_json {
                let keys: Vec<&str> = cands.iter().map(|e| e.key.as_str()).collect();
                println!(
                    "{}",
                    Envelope::failure(
                        "learnings",
                        "ambiguous",
                        format!(
                            "{query:?} matched {} learnings: {}",
                            cands.len(),
                            keys.join(", ")
                        )
                    )
                    .to_json()
                );
                std::process::exit(1);
            }
            eprintln!(
                "bn learnings: {query:?} matched {} learnings — rerun with one of these keys:",
                cands.len()
            );
            for e in &cands {
                eprintln!("  {}  {}", e.key, e.text);
            }
            std::process::exit(1);
        }
        Err(ReviewError::Write(e)) => {
            fail(&format!("bn learnings: could not record decision: {e}"))
        }
    }
}
