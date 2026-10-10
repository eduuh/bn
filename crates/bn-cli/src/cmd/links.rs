//! `bn links` — pick a stored link and open it, and everything that feeds the store.
//!
//! The decisions (rank, resolve, validate, tag) live in [`bn_core::links`] and are tested there;
//! this file is the renderer, the fzf call and the browser hand-off. It follows the one-shot
//! `fzf_select` shape rather than the self-referencing `--pick-stage` shape used by `tat` and
//! `doctor --pick`: selection comes straight back here, so bn — not an fzf keybinding — is what
//! records the open and calls the opener. No preview panel; extra keys come through `--expect`:
//! ctrl-y copies the selection instead of opening it, and ctrl-t/ctrl-e/ctrl-d tag, rename or
//! delete it and reopen the picker.

use crate::*;
use bn_core::links::{self, Link};
use std::io::Write;
use std::process::{Command, Stdio};

/// fzf field separator. Field 1 is the rendered row; field 2 is the id we act on.
const TAB: char = '\t';

mod check;
mod harvest;
mod picker;
mod review;
use self::harvest::*;
use self::picker::*;

/// Dispatch `bn links` — bare picker, `bn links <query>` open, or a subcommand.
pub(crate) fn emit_links(
    cfg: &Config,
    action: Option<LinksCmd>,
    query: Option<String>,
    tag: Option<String>,
    when: Option<String>,
    fill: Vec<String>,
    json: bool,
) {
    let fills = parse_fills(&fill, json);
    let span = parse_when(when.as_deref(), json);
    match action {
        Some(LinksCmd::Add {
            url,
            name,
            tags,
            repo,
            ttl,
        }) => add_link(cfg, &url, name, tags, repo, ttl, json),
        Some(LinksCmd::Prune) => prune(cfg, json),
        Some(LinksCmd::Check { tag }) => check::check_links(cfg, tag, json),
        Some(LinksCmd::Tag { needle, changes }) => tag_link(cfg, &needle, &changes, json),
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
        Some(LinksCmd::List {
            query,
            tag,
            repo,
            when,
        }) => {
            let span = parse_when(when.as_deref(), json);
            list_links(cfg, query, tag, repo, span.as_ref(), json)
        }
        Some(LinksCmd::Copy { needle, tag, plain }) => copy_cmd(cfg, needle, tag, plain, json),
        Some(LinksCmd::Capture) => capture(cfg, json),
        Some(LinksCmd::Tags) => show_tags(cfg, json),
        Some(LinksCmd::New) => review::review(cfg, json),
        Some(LinksCmd::Harvest {
            transcript,
            copilot_session,
            dry_run,
        }) => harvest(cfg, transcript, copilot_session, dry_run),
        Some(LinksCmd::Group { tag }) => open_group(cfg, &tag, json),
        Some(LinksCmd::Edit) => {
            let path = cfg.links_file();
            if !path.exists() {
                bn_core::util::best_effort(links::write(cfg, &[]), "links: create store");
            }
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
            let _ = Command::new(editor).arg(&path).status();
        }
        None if json && query.is_none() => list_links(cfg, None, tag, None, span.as_ref(), true),
        None => open_one(cfg, query, tag, span.as_ref(), &fills, json),
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
    ttl: Option<String>,
    json: bool,
) {
    // Checked before anything is stored, so a typo in --ttl never leaves a link without one.
    let ttl_days = ttl.as_deref().map(|raw| {
        links::parse_ttl(raw).unwrap_or_else(|| {
            fail_json(json, "links", "usage", &format!("--ttl wants 7d, 2w or a day count, got '{raw}'"))
        })
    });
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
    // An inherited repo brings this worktree's branch with it, so the link heads that branch's
    // note; an explicit `--repo` says nothing about a branch.
    let (repo, branch) = match repo {
        Some(r) if r.trim().is_empty() => (None, None),
        Some(r) => (Some(r), None),
        None => match std::env::current_dir()
            .ok()
            .and_then(|cwd| resolver::resolve(cfg, &cwd).ok())
        {
            Some(r) => (Some(r.repo), Some(r.branch)),
            None => (None, None),
        },
    };
    match links::add(
        cfg,
        url,
        name.as_deref(),
        &tags,
        repo.as_deref(),
        branch.as_deref(),
        &today(),
    ) {
        Ok(link) => {
            let link = match ttl_days {
                Some(days) => links::set_expiry(cfg, &link.id, &today(), days)
                    .unwrap_or_else(|e| fail_json(json, "links", e.code(), &e.message())),
                None => link,
            };
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

/// `bn links prune` — delete the links whose `--ttl` ran out.
fn prune(cfg: &Config, json: bool) {
    match links::prune(cfg, &today()) {
        Ok(gone) => {
            if json {
                let data: Vec<_> = gone.iter().map(link_json).collect();
                println!(
                    "{}",
                    Envelope::success("links", serde_json::json!({ "removed": data })).to_json()
                );
            } else if gone.is_empty() {
                println!("Nothing has expired.");
            } else {
                for l in &gone {
                    println!("Removed: {} — {} (expired {})", l.name, l.url, l.expires.as_deref().unwrap_or("?"));
                }
            }
        }
        Err(e) => fail_json(json, "links", e.code(), &e.message()),
    }
}

/// `bn links tag <link> +a -b c` — add and drop tags on one link.
fn tag_link(cfg: &Config, needle: &str, changes: &[String], json: bool) {
    let items = links::read(cfg);
    let id = match links::resolve(&items, needle) {
        Ok(l) => l.id.clone(),
        Err(e) => fail_json(json, "links", e.code(), &e.message()),
    };
    let (add, drop) = split_changes(changes);
    match links::retag(cfg, &id, &add, &drop) {
        Ok(l) => {
            if json {
                println!("{}", Envelope::success("links", link_json(&l)).to_json());
            } else {
                println!("[{}] {} — tags: {}", l.id, l.name, l.tags.join(", "));
            }
        }
        Err(e) => fail_json(json, "links", e.code(), &e.message()),
    }
}

/// Split `+a -b c` (or comma-joined `+a,-b`) into tags to add and tags to drop.
fn split_changes(changes: &[String]) -> (Vec<String>, Vec<String>) {
    let mut add = Vec::new();
    let mut drop = Vec::new();
    for c in changes.iter().flat_map(|c| c.split([',', ' '])) {
        let c = c.trim();
        match c.strip_prefix('-') {
            Some(t) if !t.trim_start_matches('#').is_empty() => {
                drop.push(t.trim_start_matches('#').to_string())
            }
            Some(_) => {}
            None => {
                let t = c.trim_start_matches('+').trim_start_matches('#');
                if !t.is_empty() {
                    add.push(t.to_string());
                }
            }
        }
    }
    (add, drop)
}

/// `bn links list` — the store in picker order, optionally narrowed.
fn list_links(
    cfg: &Config,
    query: Option<String>,
    tag: Option<String>,
    repo: Option<String>,
    span: Option<&links::when::Span>,
    json: bool,
) {
    let today = today();
    let items = links::read(cfg);
    let here = repo.clone().or_else(|| current_repo(cfg));
    let tag = tag.map(|t| t.to_lowercase());
    let rows: Vec<&Link> = links::ranked(&items, here.as_deref(), &today)
        .into_iter()
        .filter(|l| tag.as_ref().is_none_or(|t| l.tags.contains(t)))
        .filter(|l| repo.as_ref().is_none_or(|r| l.repo.as_deref() == Some(r)))
        .filter(|l| query.as_ref().is_none_or(|q| links::matches_query(l, q)))
        .filter(|l| span.is_none_or(|s| links::when::saved_in(l, s)))
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
        if items.is_empty() {
            println!("No links stored. `bn links add <url>` to save one.");
        } else {
            println!("No links match.");
        }
        return;
    }
    for l in rows {
        let day = links::when::label(l, &today);
        println!("{:<4} {:<10} {:<40} {}", l.id, day, truncate(&l.name, 40), l.url);
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

    let saved = match links::capture(
        cfg,
        &ctx.repo,
        &ctx.branch,
        pr.as_deref(),
        wi.as_deref(),
        &today,
    ) {
        Ok(s) => s,
        Err(e) => fail_json(json, "links", e.code(), &e.message()),
    };
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
    if saved.is_empty() && skipped.is_empty() && !found.is_empty() {
        println!("Already saved: {}/{}'s PR and work item.", ctx.repo, ctx.branch);
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
fn open_one(
    cfg: &Config,
    query: Option<String>,
    tag: Option<String>,
    span: Option<&links::when::Span>,
    fills: &[(String, String)],
    json: bool,
) {
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
        None => pick_loop(cfg, tag.as_deref(), span, json),
    };
    if action == PickAction::Copy {
        copy_ids(cfg, &chosen, false, fills, json);
        return;
    }
    open_ids(cfg, &chosen, fills, json);
}

/// `bn links group <tag>` — open every link carrying the tag.
fn open_group(cfg: &Config, tag: &str, json: bool) {
    let items = links::read(cfg);
    let today = today();
    let mut ids: Vec<String> = links::in_group(&items, tag)
        .iter()
        .map(|l| l.id.clone())
        .collect();
    // No such tag: a day is a group too, so `bn links group yesterday` opens yesterday's links.
    if ids.is_empty() {
        if let Some(span) = links::when::parse(tag, &today) {
            ids = links::ranked(&items, None, &today)
                .into_iter()
                .filter(|l| links::when::saved_in(l, &span))
                .map(|l| l.id.clone())
                .collect();
        }
    }
    if ids.is_empty() {
        fail_json(
            json,
            "links",
            "not_found",
            &format!(
                "No link is tagged or saved {tag}. `bn links add <url> --tags {tag}` to start the group."
            ),
        );
    }
    open_ids(cfg, &ids, &[], json);
}

/// Record an open for each id and hand each URL to the browser. A URL no opener took is printed
/// so it can still be clicked.
fn open_ids(cfg: &Config, ids: &[String], fills: &[(String, String)], json: bool) {
    let today = today();
    let mut opened = Vec::new();
    for id in ids {
        match links::touch(cfg, id, &today) {
            Ok(link) => {
                let link = filled(link, fills, json);
                if !json && !open_url(&link.url) {
                    println!("{}", link.url);
                }
                opened.push(link);
            }
            Err(e) => fail_json(json, "links", e.code(), &e.message()),
        }
    }
    if json {
        // One link keeps the single-object shape scripts already read; a group is a list.
        let data = match opened.as_slice() {
            [one] => link_json(one),
            many => serde_json::json!({ "items": many.iter().map(link_json).collect::<Vec<_>>() }),
        };
        println!("{}", Envelope::success("links", data).to_json());
    }
}

/// `--fill name=value` flags as pairs. A flag without `=` is a usage error, not a silent miss.
fn parse_fills(raw: &[String], json: bool) -> Vec<(String, String)> {
    raw.iter()
        .map(|f| match f.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => (k.trim().to_string(), v.to_string()),
            _ => fail_json(json, "links", "usage", &format!("--fill wants NAME=VALUE, got '{f}'")),
        })
        .collect()
}

/// The link with its `{name}` placeholders filled: from `--fill` first, then asked for on the
/// terminal. With no terminal to ask on (a script, `--json`), a missing value is an error
/// naming the flag that supplies it, never a half-filled URL handed to the browser.
fn filled(mut link: Link, fills: &[(String, String)], json: bool) -> Link {
    let mut values = fills.to_vec();
    for name in links::placeholders(&link.url) {
        if values.iter().any(|(k, _)| *k == name) {
            continue;
        }
        if json || !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            fail_json(
                json,
                "links",
                "missing_value",
                &format!("'{}' needs {{{name}}}: pass --fill {name}=…", link.name),
            );
        }
        let value = ask(&format!("{} · {name}: ", link.name));
        values.push((name, value.trim().to_string()));
    }
    link.url = links::fill(&link.url, &values);
    link
}

/// Narrow ranked links to one tag (case-insensitive); `None` keeps them all.
/// `--when` as a span of days. A spec it cannot read is a usage error naming the forms it takes.
fn parse_when(raw: Option<&str>, json: bool) -> Option<links::when::Span> {
    let raw = raw?;
    Some(links::when::parse(raw, &today()).unwrap_or_else(|| {
        fail_json(
            json,
            "links",
            "usage",
            &format!(
                "--when wants today, yesterday, a weekday, week, last-week, 7d or YYYY-MM-DD; got '{raw}'"
            ),
        )
    }))
}

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
    copy_ids(cfg, &ids, plain, &[], json);
}

/// Put these links on the clipboard, record each as used (sharing a link is using it), and say
/// what happened. No clipboard tool: print the text, as `bn pr --copy` does.
fn copy_ids(cfg: &Config, ids: &[String], plain: bool, fills: &[(String, String)], json: bool) {
    let today = today();
    let mut picked = Vec::new();
    for id in ids {
        match links::touch(cfg, id, &today) {
            Ok(l) => picked.push(filled(l, fills, json)),
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
        "new": l.new,
        "expires": l.expires,
    })
}

#[cfg(test)]
mod tests;
