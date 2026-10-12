//! The `bn links` fzf picker: rows (links and group rows), the keys that close it, and the
//! edit actions that reopen it.

use super::*;

/// The picker, kept open across edits: ctrl-t/ctrl-e/ctrl-d change the store and show the picker
/// again, so tidying links costs no round trip through `bn links edit`. Returns once the picker is
/// left with enter (open) or ctrl-y (copy); exits when fzf is missing or cancelled.
pub(super) fn pick_loop(
    cfg: &Config,
    tag: Option<&str>,
    span: Option<&links::when::Span>,
    include_groups: bool,
    json: bool,
) -> (PickAction, Vec<String>) {
    loop {
        let items = links::read(cfg);
        let here = current_repo(cfg);
        let ranked: Vec<&Link> = with_tag(links::ranked(&items, here.as_deref(), &today()), tag)
            .into_iter()
            .filter(|l| span.is_none_or(|s| links::when::saved_in(l, s)))
            .collect();
        if ranked.is_empty() {
            fail_json(json, "links", "empty", "No links match.");
        }
        let Some((action, keys)) = pick(&ranked, include_groups) else {
            // fzf missing or cancelled. Print what the picker would have shown so the command
            // is still useful on a box without fzf, and exit non-zero so a script can tell
            // "nothing opened" from "opened".
            for l in &ranked {
                println!("{:<4} {:<40} {}", l.id, truncate(&l.name, 40), l.url);
            }
            eprintln!("Nothing opened. `bn links <query>` opens one without fzf.");
            std::process::exit(1);
        };
        let ids = expand(&items, &keys);
        match action {
            PickAction::Open | PickAction::Copy => return (action, ids),
            PickAction::Tag => {
                let raw = ask("tags (+add -drop): ");
                let (add, drop) = split_changes(&[raw]);
                for id in &ids {
                    report(links::retag(cfg, id, &add, &drop));
                }
            }
            PickAction::Rename => {
                // A group row names many links; renaming them all to one name is never meant.
                for id in keys.iter().filter(|k| !k.starts_with('#')) {
                    let Some(l) = items.iter().find(|l| &l.id == id) else {
                        continue;
                    };
                    let name = ask(&format!("rename '{}' to: ", l.name));
                    if !name.trim().is_empty() {
                        report(links::rename(cfg, id, &name));
                    }
                }
            }
            PickAction::Delete => {
                let answer = ask(&format!("delete {} link(s)? [y/N] ", ids.len()));
                if answer.trim().eq_ignore_ascii_case("y") {
                    for id in &ids {
                        report(links::remove(cfg, id));
                    }
                }
            }
        }
    }
}

/// Prompt on the terminal the picker ran in and read one line. Empty on EOF.
pub(super) fn ask(prompt: &str) -> String {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim_end_matches(['\n', '\r']).to_string()
}

/// A picker edit's outcome, on stderr so it never mixes with a printed URL.
pub(super) fn report(r: Result<Link, links::LinkError>) {
    match r {
        Ok(l) => eprintln!("[{}] {} · {}", l.id, l.name, l.tags.join(" ·")),
        Err(e) => eprintln!("✗ {}", e.message()),
    }
}

/// Turn picker selections into link ids: a `#tag` key is a group row and expands to its
/// members, anything else is already an id. Duplicates (a link marked by hand *and* inside a
/// chosen group) open once.
pub(super) fn expand(items: &[Link], keys: &[String]) -> Vec<String> {
    let today = today();
    let mut ids: Vec<String> = Vec::new();
    for key in keys {
        let found: Vec<String> = if let Some(tag) = key.strip_prefix('#') {
            links::in_group(items, tag)
                .iter()
                .map(|l| l.id.clone())
                .collect()
        } else if let Some(day) = key.strip_prefix('@') {
            // A day row: every live link saved on that date.
            items
                .iter()
                .filter(|l| l.created == day && !links::expired(l, &today))
                .map(|l| l.id.clone())
                .collect()
        } else {
            vec![key.clone()]
        };
        for id in found {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// What the picker was left with: enter opens, ctrl-y copies, and ctrl-t / ctrl-e / ctrl-d tag,
/// rename or delete the selection and come back to the picker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum PickAction {
    Open,
    Copy,
    Tag,
    Rename,
    Delete,
}

/// Show the ranked links in fzf and return what to do with the chosen keys: link ids, and `#tag`
/// for a group row.
///
/// Rows are `<rendered>\t<key>`: fzf displays and matches on field 1 only (`--with-nth=1`), which
/// carries the name, repo, tags *and* URL, so typing any part of any of them finds the link.
/// Group rows rank among the links (see [`rows`]) and carry their tag and member names, so
/// typing a tag surfaces its group. `--tiebreak=begin,index` keeps our usage order as the
/// tiebreak between equally-good matches, and with an empty query fzf preserves input order
/// outright — so the link you use most is the one already under the cursor. `--multi` lets tab
/// select several; `--expect` makes the first output line the key that closed fzf, so ctrl-y can
/// mean "copy" and ctrl-t/e/d "edit" rather than "open".
pub(super) fn pick(ranked: &[&Link], include_groups: bool) -> Option<(PickAction, Vec<String>)> {
    let picker_rows = picker_rows(ranked, include_groups);
    let header = if include_groups {
        "enter: open · ctrl-y: copy for Teams · tab: select several · ▸ rows are a whole group\n\
         ctrl-t: tag · ctrl-e: rename · ctrl-d: delete"
    } else {
        "enter: open selected · ctrl-y: copy selected · tab: select several\n\
         ctrl-t: tag · ctrl-e: rename · ctrl-d: delete"
    };
    let out = fzf(
        &picker_rows,
        "ctrl-y,ctrl-t,ctrl-e,ctrl-d",
        header,
    )?;
    parse_pick(&out)
}

/// A scoped group picker shows only member links, never the synthetic row that expands all.
pub(super) fn picker_rows(ranked: &[&Link], include_groups: bool) -> Vec<String> {
    if include_groups {
        rows(ranked)
    } else {
        ranked
            .iter()
            .map(|link| format!("{}{TAB}{}", render(link), link.id))
            .collect()
    }
}

/// Run fzf over `<rendered>\t<key>` rows with `--multi` and `--expect <expect>`, and return its
/// raw stdout: the key line, then one selected row per line. `None` when fzf is missing or
/// cancelled. Shared by the main picker and `bn links new`.
pub(super) fn fzf(rows: &[String], expect: &str, header: &str) -> Option<String> {
    let mut child = Command::new("fzf")
        .args([
            "--ansi",
            "--reverse",
            "--multi",
            "--expect",
            expect,
            "--delimiter",
            "\t",
            "--with-nth",
            "1",
            "--tiebreak",
            "begin,index",
            "--header",
            header,
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
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The picker's rows, `<rendered>\t<key>`: links in rank order, with each group placed just above
/// its highest-ranked member. Opening a group records an open on every member, so a group you
/// use climbs with its links instead of sitting pinned to the top or bottom.
pub(super) fn rows(ranked: &[&Link]) -> Vec<String> {
    let groups = links::groups(ranked);
    // Days first, newest first: a small calendar above the list. The key is the date itself, so a
    // row labelled `yesterday` opens that day's links even if the picker sits open past midnight.
    let today = today();
    let mut out: Vec<String> = links::when::day_groups(ranked, &today)
        .iter()
        .map(|(day, members)| format!("{}{TAB}@{day}", render_day_group(day, members, &today)))
        .collect();
    for l in ranked {
        // `groups` keeps members in rank order, so a group's first member is its best one.
        for (tag, members) in groups.iter().filter(|(_, m)| m[0].id == l.id) {
            out.push(format!("{}{TAB}#{tag}", render_group(tag, members)));
        }
        out.push(format!("{}{TAB}{}", render(l), l.id));
    }
    out
}

/// Parse fzf's `--expect` output: the key line (empty for enter), then one selected row per line,
/// its key after the row's last tab.
pub(super) fn parse_pick(out: &str) -> Option<(PickAction, Vec<String>)> {
    let mut lines = out.lines();
    let action = match lines.next()?.trim() {
        "ctrl-y" => PickAction::Copy,
        "ctrl-t" => PickAction::Tag,
        "ctrl-e" => PickAction::Rename,
        "ctrl-d" => PickAction::Delete,
        _ => PickAction::Open,
    };
    let keys: Vec<String> = lines
        .filter_map(|row| row.rsplit(TAB).next())
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    (!keys.is_empty()).then_some((action, keys))
}

/// One group row: the tag, how many links it opens, and the member names dimmed behind it so
/// typing a member's name also finds the group it belongs to.
pub(super) fn render_group(tag: &str, members: &[&Link]) -> String {
    let ink = style::Ink::always();
    let names: Vec<&str> = members.iter().map(|l| l.name.as_str()).collect();
    format!(
        "{} {}  {}",
        ink.accent(&format!("▸ #{tag}")),
        ink.meta(&format!("({} links)", members.len())),
        ink.meta(&truncate(&names.join(", "), 80))
    )
}

/// One day row: the day as a word (`yesterday`, `mon`) and as a date, how many links it opens,
/// and their names dimmed behind it.
pub(super) fn render_day_group(day: &str, members: &[&Link], today: &str) -> String {
    let ink = style::Ink::always();
    let word = links::when::day_label(day, today);
    let date = links::when::day_long(day);
    let names: Vec<&str> = members.iter().map(|l| l.name.as_str()).collect();
    format!(
        "{} {}  {}",
        ink.accent(&format!("▸ {word} · {date}")),
        ink.meta(&format!("({} links)", members.len())),
        ink.meta(&truncate(&names.join(", "), 80))
    )
}

/// One picker row: the name, then the repo, tags and URL dimmed behind it. Everything shown is
/// also everything fzf matches on, so there is nothing searchable that is invisible.
pub(super) fn render(l: &Link) -> String {
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
    // Saved by the session hook and not reviewed yet (`bn links new`).
    let new = if l.new {
        format!("  {}", ink.accent("•new"))
    } else {
        String::new()
    };
    // The day it was last opened or saved, as a word you can type: `yesterday`, `mon`, `Sep 28`.
    let day = ink.meta(&links::when::label(l, &today()));
    format!(
        "{}{new}{repo}{tags}  {day}  {}",
        truncate(&l.name, 48),
        ink.meta(&l.url)
    )
}
