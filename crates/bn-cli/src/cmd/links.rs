//! `bn links` — pick a stored link and open it, and everything that feeds the store.
//!
//! The decisions (rank, resolve, validate, tag) live in [`bn_core::links`] and are tested there;
//! this file is the renderer, the fzf call and the browser hand-off. It follows the one-shot
//! `fzf_select` shape rather than the self-referencing `--pick-stage` shape used by `tat` and
//! `doctor --pick`: selection comes straight back here, so bn — not an fzf keybinding — is what
//! records the open and calls the opener. No preview panel and one round trip; the only extra key
//! is ctrl-y (via `--expect`), which copies the selection instead of opening it.

use crate::*;
use bn_core::links::{self, Link};
use std::io::Write;
use std::process::{Command, Stdio};

/// fzf field separator. Field 1 is the rendered row; field 2 is the id we act on.
const TAB: char = '\t';

/// Dispatch `bn links` — bare picker, `bn links <query>` open, or a subcommand.
pub(crate) fn emit_links(
    cfg: &Config,
    action: Option<LinksCmd>,
    query: Option<String>,
    tag: Option<String>,
    json: bool,
) {
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
        Some(LinksCmd::Copy { needle, tag, plain }) => copy_cmd(cfg, needle, tag, plain, json),
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
        None if json && query.is_none() => list_links(cfg, tag, None, true),
        None => open_one(cfg, query, tag, json),
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
    let vocab = resolver::link_tags();
    let (tags, unknown) = match tags.as_deref() {
        Some(raw) => links::split_tags(raw, &vocab),
        None => (vec![], vec![]),
    };
    // No tags given: file it by host (an ICM incident under `icm`) rather than leave it untagged.
    let guessed = tags.is_empty();
    let tags = if guessed {
        links::guess_tags(url, &vocab)
    } else {
        tags
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
                let how = if guessed && !tags.is_empty() {
                    format!(" (tagged {} from host)", tags.join(", "))
                } else {
                    String::new()
                };
                println!("Saved [{}] {} — {}{how}", link.id, link.name, link.url);
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

/// Bare `bn links` (picker) and `bn links <query>` (direct) — resolve links, record the use,
/// and hand them to the browser, or to the clipboard when the picker was left with ctrl-y.
fn open_one(cfg: &Config, query: Option<String>, tag: Option<String>, json: bool) {
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
    let (action, chosen) = match query {
        Some(q) => match links::resolve(&items, &q) {
            Ok(l) => (PickAction::Open, vec![l.id.clone()]),
            Err(e) => fail_json(json, "links", e.code(), &e.message()),
        },
        None => {
            let here = current_repo(cfg);
            let ranked = with_tag(links::ranked(&items, here.as_deref(), &today), tag.as_deref());
            if ranked.is_empty() {
                fail_json(
                    json,
                    "links",
                    "empty",
                    &format!("No links tagged {}.", tag.as_deref().unwrap_or("")),
                );
            }
            match pick(&ranked) {
                Some(p) => p,
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
    if action == PickAction::Copy {
        copy_ids(cfg, &chosen, false, json);
        return;
    }
    for id in &chosen {
        match links::touch(cfg, id, &today) {
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
}

/// Narrow ranked links to one tag (case-insensitive); `None` keeps them all.
fn with_tag<'a>(ranked: Vec<&'a Link>, tag: Option<&str>) -> Vec<&'a Link> {
    let Some(tag) = tag.map(str::to_lowercase) else {
        return ranked;
    };
    ranked.into_iter().filter(|l| l.tags.contains(&tag)).collect()
}

/// `bn links copy [needle] [--tag T] [--plain]` — one link by needle, or every link with a tag.
fn copy_cmd(
    cfg: &Config,
    needle: Option<String>,
    tag: Option<String>,
    plain: bool,
    json: bool,
) {
    let items = links::read(cfg);
    let ids: Vec<String> = match (&needle, &tag) {
        (Some(n), _) => {
            // A tag narrows what the needle can match, so `--tag icm sev2` finds the ICM one.
            let pool: Vec<Link> = match tag.as_deref() {
                Some(t) => {
                    let t = t.to_lowercase();
                    items.iter().filter(|l| l.tags.contains(&t)).cloned().collect()
                }
                None => items,
            };
            match links::resolve(&pool, n) {
                Ok(l) => vec![l.id.clone()],
                Err(e) => fail_json(json, "links", e.code(), &e.message()),
            }
        }
        (None, Some(t)) => {
            let here = current_repo(cfg);
            with_tag(links::ranked(&items, here.as_deref(), &today()), Some(t))
                .iter()
                .map(|l| l.id.clone())
                .collect()
        }
        (None, None) => fail_json(
            json,
            "links",
            "usage",
            "Say which: `bn links copy <needle>` or `bn links copy --tag <tag>`.",
        ),
    };
    if ids.is_empty() {
        fail_json(
            json,
            "links",
            "empty",
            &format!("No links tagged {}.", tag.as_deref().unwrap_or("")),
        );
    }
    copy_ids(cfg, &ids, plain, json);
}

/// Put these links on the clipboard, record each as used (sharing a link is using it), and say
/// what happened. No clipboard tool: print the text, as `bn pr --copy` does.
fn copy_ids(cfg: &Config, ids: &[String], plain: bool, json: bool) {
    let today = today();
    let mut picked = Vec::new();
    for id in ids {
        match links::touch(cfg, id, &today) {
            Ok(l) => picked.push(l),
            Err(e) => fail_json(json, "links", e.code(), &e.message()),
        }
    }
    let refs: Vec<&Link> = picked.iter().collect();
    let (html, text) = share_payload(&refs);
    let copied = (!plain && copy_html(&html, &text)) || copy_to_clipboard(&text);
    if json {
        let data: Vec<_> = picked.iter().map(link_json).collect();
        println!(
            "{}",
            Envelope::success(
                "links",
                serde_json::json!({ "items": data, "copied": copied }),
            )
            .to_json()
        );
        return;
    }
    if !copied {
        println!("{text}");
        eprintln!("(no clipboard tool found)");
        return;
    }
    match picked.as_slice() {
        [one] => println!("Copied: {}", one.name),
        many => println!("Copied {} links", many.len()),
    }
}

/// The clipboard payload for sharing: HTML where each name is a hyperlink (a bulleted list when
/// there are several), and the plain-text alternative, one `name url` line per link, which Teams
/// and most chat tools still turn into a clickable link.
fn share_payload(links: &[&Link]) -> (String, String) {
    let a = |l: &Link| {
        format!(
            "<a href=\"{}\">{}</a>",
            html_escape(&l.url),
            html_escape(&l.name)
        )
    };
    let html = match links {
        [one] => a(one),
        many => format!(
            "<ul>{}</ul>",
            many.iter()
                .map(|l| format!("<li>{}</li>", a(l)))
                .collect::<String>()
        ),
    };
    let text = links
        .iter()
        .map(|l| format!("{} {}", l.name, l.url))
        .collect::<Vec<_>>()
        .join("\n");
    (html, text)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// What the picker was left with: enter opens, ctrl-y copies.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PickAction {
    Open,
    Copy,
}

/// Show the ranked links in fzf and return what to do with the chosen ids.
///
/// Rows are `<rendered>\t<id>`: fzf displays and matches on field 1 only (`--with-nth=1`), which
/// carries the name, repo, tags *and* URL, so typing any part of any of them finds the link.
/// `--tiebreak=begin,index` keeps our usage order as the tiebreak between equally-good matches,
/// and with an empty query fzf preserves input order outright — so the link you use most is the
/// one already under the cursor. `--multi` lets tab select several; `--expect ctrl-y` makes the
/// first output line the key that closed fzf, so ctrl-y can mean "copy" rather than "open".
fn pick(ranked: &[&Link]) -> Option<(PickAction, Vec<String>)> {
    let rows: Vec<String> = ranked
        .iter()
        .map(|l| format!("{}{TAB}{}", render(l), l.id))
        .collect();
    let mut child = Command::new("fzf")
        .args([
            "--ansi",
            "--reverse",
            "--multi",
            "--expect",
            "ctrl-y",
            "--delimiter",
            "\t",
            "--with-nth",
            "1",
            "--tiebreak",
            "begin,index",
            "--header",
            "enter: open · ctrl-y: copy for Teams · tab: select several",
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
    parse_pick(&String::from_utf8_lossy(&out.stdout))
}

/// Parse fzf's `--expect` output: the key line (empty for enter), then one selected row per line.
fn parse_pick(out: &str) -> Option<(PickAction, Vec<String>)> {
    let mut lines = out.lines();
    let action = match lines.next()?.trim() {
        "ctrl-y" => PickAction::Copy,
        _ => PickAction::Open,
    };
    let ids: Vec<String> = lines
        .filter_map(|row| row.rsplit(TAB).next())
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    (!ids.is_empty()).then_some((action, ids))
}

/// One picker row: the name, then the repo, tags and URL dimmed behind it. Everything shown is
/// also everything fzf matches on, so there is nothing searchable that is invisible.
fn render(l: &Link) -> String {
    // `pick` feeds these to `fzf --ansi`, so the row paints whatever bn's own stdout is doing.
    let ink = style::Ink::always();
    let repo = l
        .repo
        .as_deref()
        .map(|r| format!("  {}", ink.accent(&format!("[{r}]"))))
        .unwrap_or_default();
    let tags = if l.tags.is_empty() {
        String::new()
    } else {
        format!("  {}", ink.meta(&format!("·{}", l.tags.join(" ·"))))
    };
    format!(
        "{}{repo}{tags}  {}",
        truncate(&l.name, 48),
        ink.meta(&l.url)
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

    #[test]
    fn one_link_shares_as_a_single_escaped_hyperlink() {
        let l = link("ICM <sev2> & co", "https://x.test/?a=1&b=\"2\"");
        let (html, text) = share_payload(&[&l]);
        assert_eq!(
            html,
            r#"<a href="https://x.test/?a=1&amp;b=&quot;2&quot;">ICM &lt;sev2&gt; &amp; co</a>"#
        );
        assert_eq!(text, "ICM <sev2> & co https://x.test/?a=1&b=\"2\"");
    }

    #[test]
    fn several_links_share_as_a_list() {
        let a = link("A", "https://a.test");
        let b = link("B", "https://b.test");
        let (html, text) = share_payload(&[&a, &b]);
        assert_eq!(
            html,
            r#"<ul><li><a href="https://a.test">A</a></li><li><a href="https://b.test">B</a></li></ul>"#
        );
        assert_eq!(text, "A https://a.test\nB https://b.test");
    }

    #[test]
    fn fzf_expect_output_maps_to_an_action_and_ids() {
        assert_eq!(
            parse_pick("\nrow one\tk3\n"),
            Some((PickAction::Open, vec!["k3".to_string()]))
        );
        assert_eq!(
            parse_pick("ctrl-y\nrow\tk1\nrow\tk7\n"),
            Some((PickAction::Copy, vec!["k1".to_string(), "k7".to_string()]))
        );
        assert_eq!(parse_pick("ctrl-y\n"), None);
    }
}
