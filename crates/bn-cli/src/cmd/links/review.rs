//! `bn links new`: review the links the session hook saved without asking. Each one is kept,
//! tagged or dropped; the list reopens until nothing is left to look at.

use super::*;

pub(super) fn review(cfg: &Config, json: bool) {
    loop {
        let items = links::read(cfg);
        let waiting = links::pending(&items, &today());
        if json {
            let data: Vec<_> = waiting.iter().map(|l| link_json(l)).collect();
            println!(
                "{}",
                Envelope::success("links", serde_json::json!({ "items": data })).to_json()
            );
            return;
        }
        if waiting.is_empty() {
            println!("No new links to review.");
            return;
        }
        let rows: Vec<String> = waiting
            .iter()
            .map(|l| format!("{}{TAB}{}", render(l), l.id))
            .collect();
        let header = format!(
            "{} new · enter: keep · ctrl-t: tag and keep · ctrl-d: drop · tab: select several",
            waiting.len()
        );
        let chosen = std::io::IsTerminal::is_terminal(&std::io::stdin())
            .then(|| fzf(&rows, "ctrl-t,ctrl-d", &header))
            .flatten()
            .and_then(|out| parse_pick(&out));
        let Some((action, ids)) = chosen else {
            // No terminal, no fzf, or cancelled: show what is waiting and leave it waiting.
            for l in &waiting {
                println!("{:<4} {:<40} {}", l.id, truncate(&l.name, 40), l.url);
            }
            return;
        };
        match action {
            PickAction::Delete => {
                let answer = ask(&format!("drop {} link(s)? [y/N] ", ids.len()));
                if answer.trim().eq_ignore_ascii_case("y") {
                    for id in &ids {
                        report(links::remove(cfg, id));
                    }
                }
            }
            PickAction::Tag => {
                let (add, drop) = split_changes(&[ask("tags (+add -drop): ")]);
                for id in &ids {
                    report(links::retag(cfg, id, &add, &drop));
                }
                keep(cfg, &ids);
            }
            _ => keep(cfg, &ids),
        }
    }
}

/// Clear the new mark: these were looked at and kept.
fn keep(cfg: &Config, ids: &[String]) {
    if let Err(e) = links::mark_reviewed(cfg, ids) {
        eprintln!("✗ {}", e.message());
    }
}
