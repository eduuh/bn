//! `bn links` — pick a stored link and open it, and everything that feeds the store.
//!
//! The decisions (rank, resolve, validate, tag) live in [`bn_core::links`] and are tested there;
//! this file is the renderer, the fzf call and the browser hand-off. It follows the one-shot
//! `fzf_select` shape rather than the self-referencing `--pick-stage` shape used by `tat` and
//! `doctor --pick`: selection comes straight back here, so bn — not an fzf keybinding — is what
//! records the open and calls the opener. No preview panel, no per-key actions, one round trip.

use crate::*;
use bn_core::links::{self, Link};
use std::io::Write;
use std::process::{Command, Stdio};

/// fzf field separator. Field 1 is the rendered row; field 2 is the id we act on.
const TAB: char = '\t';

/// Dispatch `bn links` — bare picker, `bn links <query>` open, or a subcommand.
pub(crate) fn emit_links(cfg: &Config, action: Option<LinksCmd>, query: Option<String>, json: bool) {
    match action {
        Some(LinksCmd::Add {
            url,
            name,
            tags,
            repo,
        }) => add_link(cfg, &url, name, tags, repo, json),
        Some(LinksCmd::Rm { needle }) => match links::remove(cfg, &needle) {
            Ok(gone) => {
                if json {
                    println!("{}", Envelope::success("links", link_json(&gone)).to_json());
                } else {
                    println!("Removed: {} — {}", gone.name, gone.url);
                }
            }
            Err(e) => fail_json(json, "links", e.code(), &e.message()),
        },
        Some(LinksCmd::List { tag, repo }) => list_links(cfg, tag, repo, json),
        Some(LinksCmd::Capture) => capture(cfg, json),
        Some(LinksCmd::Tags) => show_tags(cfg, json),
        Some(LinksCmd::Edit) => {
            let path = cfg.links_file();
            if !path.exists() {
                bn_core::util::best_effort(links::write(cfg, &[]), "links: create store");
            }
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
            let _ = Command::new(editor).arg(&path).status();
        }
        None => open_one(cfg, query, json),
    }
}

/// The repo the user is standing in, or `None` outside a worktree bn recognises. Never exits —
/// links are person-scoped, so being outside a repo is normal, not an error.
fn current_repo(cfg: &Config) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    resolver::resolve(cfg, &cwd).ok().map(|r| r.repo)
}

/// `bn links add <url>` — store or update one link.
fn add_link(
    cfg: &Config,
    url: &str,
    name: Option<String>,
    tags: Option<String>,
    repo: Option<String>,
    json: bool,
) {
    let (tags, unknown) = match tags.as_deref() {
        Some(raw) => links::split_tags(raw, &resolver::link_tags()),
        None => (vec![], vec![]),
    };
    // `--repo ""` is the explicit "this link belongs to no repo"; omitting it inherits the
    // worktree you are standing in, which is the common case and the one worth defaulting.
    let repo = match repo {
        Some(r) if r.trim().is_empty() => None,
        Some(r) => Some(r),
        None => current_repo(cfg),
    };
    match links::add(cfg, url, name.as_deref(), &tags, repo.as_deref(), &today()) {
        Ok(link) => {
            if !unknown.is_empty() && !json {
                eprintln!(
                    "⚠ not in your tag vocabulary: {} — saved anyway (`bn links tags` to see the set)",
                    unknown.join(", ")
                );
            }
            if json {
                println!("{}", Envelope::success("links", link_json(&link)).to_json());
            } else {
                println!("Saved [{}] {} — {}", link.id, link.name, link.url);
            }
        }
        Err(e) => fail_json(json, "links", e.code(), &e.message()),
    }
}

/// `bn links list` — the store in picker order, optionally narrowed.
fn list_links(cfg: &Config, tag: Option<String>, repo: Option<String>, json: bool) {
    let today = today();
    let items = links::read(cfg);
    let here = repo.clone().or_else(|| current_repo(cfg));
    let tag = tag.map(|t| t.to_lowercase());
    let rows: Vec<&Link> = links::ranked(&items, here.as_deref(), &today)
        .into_iter()
        .filter(|l| tag.as_ref().is_none_or(|t| l.tags.contains(t)))
        .filter(|l| repo.as_ref().is_none_or(|r| l.repo.as_deref() == Some(r)))
        .collect();
    if json {
        let data: Vec<_> = rows.iter().map(|l| link_json(l)).collect();
        println!(
            "{}",
            Envelope::success("links", serde_json::json!({ "items": data })).to_json()
        );
        return;
    }
    if rows.is_empty() {
        println!("No links stored. `bn links add <url>` to save one.");
        return;
    }
    for l in rows {
        println!("{:<4} {:<40} {}", l.id, truncate(&l.name, 40), l.url);
    }
}

/// `bn links capture` — harvest the branch note's PR and work item into the store.
///
/// The point of the command is that capturing costs one word: at the moment a PR completes, the
/// two links you will want again are already recorded on the note, and both leave with the
/// worktree if nobody moves them.
///
/// No tag is invented. The `repo` field already groups these, and a guessed tag is noise you
/// would have to clean up later — tag them yourself if they deserve one.
fn capture(cfg: &Config, json: bool) {
    let ctx = resolve_or_exit(cfg);
    let today = today();
    let pr = commands::read_pr(cfg, &ctx);
    let wi = commands::read_work_item(cfg, &ctx);
    let found = links::capturable(cfg, &ctx.repo, pr.as_deref(), wi.as_deref());
    // What the note carries but no forge could turn into a URL — reported, never stored.
    let stored_urls: Vec<&str> = found.iter().map(|c| c.url.as_str()).collect();
    let skipped: Vec<String> = [pr.as_deref(), wi.as_deref()]
        .into_iter()
        .flatten()
        .filter(|v| !stored_urls.iter().any(|u| u.contains(v.trim())))
        .map(str::to_string)
        .collect();

    let mut saved = Vec::new();
    for c in &found {
        let name = format!("{} {} {}", ctx.repo, ctx.branch, c.label);
        match links::add(cfg, &c.url, Some(&name), &[], Some(&ctx.repo), &today) {
            Ok(l) => saved.push(l),
            Err(e) => fail_json(json, "links", e.code(), &e.message()),
        }
    }
    if json {
        let data: Vec<_> = saved.iter().map(link_json).collect();
        println!(
            "{}",
            Envelope::success(
                "links",
                serde_json::json!({ "saved": data, "skipped": skipped }),
            )
            .to_json()
        );
        return;
    }
    if saved.is_empty() && skipped.is_empty() {
        println!(
            "Nothing to capture: {}/{} has no PR or work item linked.",
            ctx.repo, ctx.branch
        );
        return;
    }
    for l in &saved {
        println!("Saved [{}] {} — {}", l.id, l.name, l.url);
    }
    for s in &skipped {
        eprintln!(
            "⚠ no URL could be derived for {s} — this repo's remote names no issue page; \
             `bn links add <url>` with the full link"
        );
    }
}

/// `bn links tags` — the configured vocabulary and what is actually in use.
fn show_tags(cfg: &Config, json: bool) {
    let (vocab, source) = resolver::link_tags_with_source();
    let source = match source {
        resolver::TagSource::Config => "config.toml",
        resolver::TagSource::Default => "built-in default",
    };
    let counts = links::tag_counts(&links::read(cfg));
    if json {
        println!(
            "{}",
            Envelope::success(
                "links",
                serde_json::json!({ "vocabulary": vocab, "source": source, "in_use": counts }),
            )
            .to_json()
        );
        return;
    }
    if vocab.is_empty() {
        println!("No tag vocabulary — `[links] tags` in config.toml is set to an empty list.");
    } else {
        println!("Vocabulary ({source}):");
        for t in &vocab {
            println!("  {:<14} {}", t, counts.get(&t.to_lowercase()).unwrap_or(&0));
        }
    }
    let extra: Vec<_> = counts
        .iter()
        .filter(|(t, _)| !vocab.iter().any(|v| v.to_lowercase() == **t))
        .collect();
    if !extra.is_empty() {
        println!("Outside the vocabulary:");
        for (t, n) in extra {
            println!("  {t:<14} {n}");
        }
    }
}

/// Bare `bn links` (picker) and `bn links <query>` (direct) — resolve one link, record the open,
/// and hand it to the browser.
fn open_one(cfg: &Config, query: Option<String>, json: bool) {
    let today = today();
    let items = links::read(cfg);
    if items.is_empty() {
        fail_json(
            json,
            "links",
            "empty",
            "No links stored. `bn links add <url>` to save one.",
        );
    }
    let chosen = match query {
        Some(q) => match links::resolve(&items, &q) {
            Ok(l) => l.id.clone(),
            Err(e) => fail_json(json, "links", e.code(), &e.message()),
        },
        None => {
            let here = current_repo(cfg);
            let ranked = links::ranked(&items, here.as_deref(), &today);
            match pick(&ranked) {
                Some(id) => id,
                None => {
                    // fzf missing or cancelled. Print what the picker would have shown so the
                    // command is still useful on a box without fzf, and exit non-zero so a
                    // script can tell "nothing opened" from "opened".
                    for l in &ranked {
                        println!("{:<4} {:<40} {}", l.id, truncate(&l.name, 40), l.url);
                    }
                    eprintln!("Nothing opened. `bn links <query>` opens one without fzf.");
                    std::process::exit(1);
                }
            }
        }
    };
    match links::touch(cfg, &chosen, &today) {
        Ok(link) => {
            if json {
                println!("{}", Envelope::success("links", link_json(&link)).to_json());
            } else if !open_url(&link.url) {
                println!("{}", link.url);
            }
        }
        Err(e) => fail_json(json, "links", e.code(), &e.message()),
    }
}

/// Show the ranked links in fzf and return the chosen id.
///
/// Rows are `<rendered>\t<id>`: fzf displays and matches on field 1 only (`--with-nth=1`), which
/// carries the name, repo, tags *and* URL, so typing any part of any of them finds the link.
/// `--tiebreak=begin,index` keeps our usage order as the tiebreak between equally-good matches,
/// and with an empty query fzf preserves input order outright — so the link you use most is the
/// one already under the cursor.
fn pick(ranked: &[&Link]) -> Option<String> {
    let rows: Vec<String> = ranked
        .iter()
        .map(|l| format!("{}{TAB}{}", render(l), l.id))
        .collect();
    let mut child = Command::new("fzf")
        .args([
            "--ansi",
            "--reverse",
            "--delimiter",
            "\t",
            "--with-nth",
            "1",
            "--tiebreak",
            "begin,index",
            "--header",
            "enter: open in browser",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let input = rows.join("\n");
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sel = String::from_utf8_lossy(&out.stdout);
    let id = sel.trim().rsplit(TAB).next()?.trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// One picker row: the name, then the repo, tags and URL dimmed behind it. Everything shown is
/// also everything fzf matches on, so there is nothing searchable that is invisible.
fn render(l: &Link) -> String {
    let repo = l
        .repo
        .as_deref()
        .map(|r| format!("  \x1b[36m[{r}]\x1b[0m"))
        .unwrap_or_default();
    let tags = if l.tags.is_empty() {
        String::new()
    } else {
        format!("  \x1b[2m·{}\x1b[0m", l.tags.join(" ·"))
    };
    format!(
        "{}{repo}{tags}  \x1b[2m{}\x1b[0m",
        truncate(&l.name, 48),
        l.url
    )
}

/// Trim a display string to `max` characters, marking that it was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// One link as the JSON envelope's `data`.
fn link_json(l: &Link) -> serde_json::Value {
    serde_json::json!({
        "id": l.id,
        "url": l.url,
        "name": l.name,
        "tags": l.tags,
        "repo": l.repo,
        "created": l.created,
        "uses": l.uses,
        "last": l.last,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(name: &str, url: &str) -> Link {
        Link {
            id: "k1".into(),
            url: url.into(),
            name: name.into(),
            tags: vec!["dri".into()],
            repo: Some("bn".into()),
            created: "2026-01-01".into(),
            uses: 0,
            last: None,
        }
    }

    #[test]
    fn a_picker_row_carries_the_id_after_the_last_tab() {
        let l = link("Prod subscription", "https://portal.azure.com/x");
        let row = format!("{}{TAB}{}", render(&l), l.id);
        assert_eq!(row.rsplit(TAB).next().unwrap(), "k1");
    }

    #[test]
    fn everything_searchable_is_in_the_row_fzf_matches_on() {
        let display = render(&link("Prod subscription", "https://portal.azure.com/x"));
        for needle in ["Prod subscription", "bn", "dri", "portal.azure.com"] {
            assert!(display.contains(needle), "row is missing {needle}: {display}");
        }
    }

    #[test]
    fn a_long_name_is_cut_with_an_ellipsis_not_silently() {
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        assert_eq!(truncate("abc", 5), "abc");
    }
}
