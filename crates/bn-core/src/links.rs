//! Links worth going back to (`bn links`) — a person-scoped store of URLs, ranked by use.
//!
//! A PBI, a PR, an Azure portal blade, a docs site, an internal dashboard: things you reach for
//! again, that today live in browser bookmarks or nowhere. The branch note holds exactly two
//! ([`crate::commands::set_pr`], [`crate::commands::set_work_item`]) and both leave with the
//! worktree, so this store sits beside `tasks.txt` and `references/` on the person axis —
//! `<work_root>/links.md`, no `<machine>` segment, because a link you need is not a link one
//! laptop needs.
//!
//! Ranking is frecency, not raw count: a link opened today beats one opened fifty times last
//! quarter. The bucket weights mirror zoxide's aging (which `crate`'s own `tat` picker already
//! reads for directory frecency), so "most used" means the same thing in both pickers.
//!
//! Storage is fenced-YAML frontmatter over a regenerated body, the same shape as
//! [`crate::inbox`], so [`crate::store::parse_frontmatter`] is reused and the file stays
//! hand-editable — `bn links edit` is a supported way to fix a name.

use crate::resolver::Config;
use crate::store::StoreError;
use crate::util::days_since;
use serde::{Deserialize, Serialize};

/// The tag vocabulary bn ships with, used when `[links] tags` is absent from config.toml.
///
/// Absent and empty mean different things: a machine whose config predates the setting should
/// still get a working vocabulary, while an explicit `tags = []` is someone saying they want
/// none. Kept short on purpose — six tags you use beat twenty you don't.
pub const DEFAULT_TAGS: [&str; 9] = [
    "domains",
    "dri",
    "telemetry",
    "security",
    "permissions",
    "repos",
    "pipelines",
    "incidents",
    "learning",
];

/// Id prefix for stored links (`k1`, `k2`, …). `l` belongs to learnings.
const K_PREFIX: char = 'k';

/// One stored link. `uses`/`last` are the ranking inputs, bumped by [`touch`] when the link is
/// actually opened — never by listing it. `repo` records which repository the link belongs to,
/// so links for the repo you are standing in sort first; `None` means it is repo-agnostic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub id: String,
    pub url: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub created: String,
    #[serde(default)]
    pub uses: u32,
    /// Date of the most recent open. `None` until the link has been opened once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
}

/// The links file's frontmatter: just the list, in insertion order. Rank is computed on read,
/// never stored as an order, so hand-editing the file cannot corrupt the ranking.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct LinksFront {
    #[serde(default)]
    items: Vec<Link>,
}

/// What can go wrong writing the store. Everything else (a missing file, unparseable YAML) reads
/// as an empty store — a link list is not load-bearing enough to fail a command over.
#[derive(Debug)]
pub enum LinkError {
    /// The URL is not an `http`/`https` URL.
    BadUrl(String),
    /// No stored link matched the id or name given.
    NotFound(String),
    /// The name or id matched more than one link; the candidates are carried for the message.
    Ambiguous(Vec<String>),
    /// The store could not be written.
    Store(StoreError),
}

impl LinkError {
    /// The message a CLI prints, and the `error.message` a JSON envelope carries.
    pub fn message(&self) -> String {
        match self {
            Self::BadUrl(u) => format!("not an http(s) URL: {u}"),
            Self::NotFound(n) => format!("no link matches '{n}'"),
            Self::Ambiguous(c) => format!("'{}' matches {} links", c.join("', '"), c.len()),
            Self::Store(e) => format!("{e}"),
        }
    }

    /// The `error.code` a JSON envelope carries.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadUrl(_) => "bad_url",
            Self::NotFound(_) => "not_found",
            Self::Ambiguous(_) => "ambiguous",
            Self::Store(_) => "store",
        }
    }
}

impl From<StoreError> for LinkError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// Read the store (empty if the file is missing or unparseable).
pub fn read(cfg: &Config) -> Vec<Link> {
    let Ok(text) = std::fs::read_to_string(cfg.links_file()) else {
        return vec![];
    };
    crate::store::parse_frontmatter::<LinksFront>(&text)
        .map(|f| f.items)
        .unwrap_or_default()
}

/// Write the store, preserving insertion order.
pub fn write(cfg: &Config, items: &[Link]) -> Result<(), StoreError> {
    let front = LinksFront {
        items: items.to_vec(),
    };
    let fm = serde_yaml::to_string(&front).map_err(|e| StoreError::Parse(e.to_string()))?;
    let text = format!("---\n{fm}---\n\n# Links\n");
    let path = cfg.links_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, text)?;
    Ok(())
}

/// Add a link, or update the one that already holds this URL.
///
/// Re-adding an existing URL is an update, never a duplicate: a link you save twice is a link
/// you meant to correct. The existing `uses`/`last`/`created` survive, so re-tagging a link does
/// not cost it its rank.
pub fn add(
    cfg: &Config,
    url: &str,
    name: Option<&str>,
    tags: &[String],
    repo: Option<&str>,
    today: &str,
) -> Result<Link, LinkError> {
    let url = url.trim();
    if !valid_url(url) {
        return Err(LinkError::BadUrl(url.to_string()));
    }
    let _lock = crate::store::NoteLock::acquire(&cfg.links_file());
    let mut items = read(cfg);
    let name = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_name(url));

    if let Some(existing) = items.iter_mut().find(|l| l.url == url) {
        existing.name = name;
        if !tags.is_empty() {
            existing.tags = tags.to_vec();
        }
        if repo.is_some() {
            existing.repo = repo.map(str::to_string);
        }
        let out = existing.clone();
        write(cfg, &items)?;
        return Ok(out);
    }

    let link = Link {
        id: crate::id::next_id_from_ids(items.iter().map(|l| l.id.as_str()), K_PREFIX),
        url: url.to_string(),
        name,
        tags: tags.to_vec(),
        repo: repo.map(str::to_string),
        created: today.to_string(),
        uses: 0,
        last: None,
    };
    items.push(link.clone());
    write(cfg, &items)?;
    Ok(link)
}

/// Remove the link matching `needle` (id or name, resolved as in [`resolve`]).
pub fn remove(cfg: &Config, needle: &str) -> Result<Link, LinkError> {
    let _lock = crate::store::NoteLock::acquire(&cfg.links_file());
    let mut items = read(cfg);
    let id = resolve(&items, needle)?.id.clone();
    let pos = items
        .iter()
        .position(|l| l.id == id)
        .ok_or_else(|| LinkError::NotFound(needle.to_string()))?;
    let gone = items.remove(pos);
    write(cfg, &items)?;
    Ok(gone)
}

/// Record an open: `uses += 1`, `last = today`. This is the only thing that changes rank.
pub fn touch(cfg: &Config, id: &str, today: &str) -> Result<Link, LinkError> {
    let _lock = crate::store::NoteLock::acquire(&cfg.links_file());
    let mut items = read(cfg);
    let link = items
        .iter_mut()
        .find(|l| l.id == id)
        .ok_or_else(|| LinkError::NotFound(id.to_string()))?;
    link.uses = link.uses.saturating_add(1);
    link.last = Some(today.to_string());
    let out = link.clone();
    write(cfg, &items)?;
    Ok(out)
}

/// Frecency weight for a link last opened `days` ago — zoxide's aging buckets.
fn bucket(days: i64) -> u32 {
    match days {
        d if d < 1 => 16,
        d if d < 7 => 8,
        d if d < 30 => 2,
        _ => 1,
    }
}

/// A link's rank score: how much it is used, decayed by how long since it was used.
///
/// `uses.max(1)` so a never-opened link still sorts by its recency bucket rather than
/// collapsing to zero — a link you saved this morning should be findable this morning.
pub fn score(link: &Link, today: &str) -> u32 {
    let days = link
        .last
        .as_deref()
        .and_then(|l| days_since(l, today))
        .unwrap_or_else(|| days_since(&link.created, today).unwrap_or(i64::MAX));
    link.uses.max(1).saturating_mul(bucket(days))
}

/// The store in the order a picker should show it: the current repo's links first, then by
/// frecency, then most-recently-used, then by name so the order is stable across runs.
pub fn ranked<'a>(items: &'a [Link], repo: Option<&str>, today: &str) -> Vec<&'a Link> {
    let mut out: Vec<&Link> = items.iter().collect();
    out.sort_by(|a, b| {
        let mine = |l: &Link| repo.is_some() && l.repo.as_deref() == repo;
        mine(b)
            .cmp(&mine(a))
            .then(score(b, today).cmp(&score(a, today)))
            .then(b.last.cmp(&a.last))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// Everything [`resolve`] fuzzy-matches against, lowercased: the name, the tags, and the URL.
fn haystack(l: &Link) -> String {
    format!(
        "{} {} {}",
        l.name.to_lowercase(),
        l.tags.join(" ").to_lowercase(),
        l.url.to_lowercase()
    )
}

/// Resolve `needle` to exactly one link: exact id, then exact name, then unique name prefix,
/// then unique substring across name, tags and URL. Mirrors [`crate::references::resolve`] so
/// "type enough of it" means the same thing everywhere in bn.
pub fn resolve<'a>(items: &'a [Link], needle: &str) -> Result<&'a Link, LinkError> {
    let n = needle.trim().to_lowercase();
    if n.is_empty() {
        return Err(LinkError::NotFound(needle.to_string()));
    }
    if let Some(l) = items.iter().find(|l| l.id == n) {
        return Ok(l);
    }
    if let Some(l) = items.iter().find(|l| l.name.to_lowercase() == n) {
        return Ok(l);
    }
    let passes: [fn(&Link, &str) -> bool; 2] = [
        |l, n| l.name.to_lowercase().starts_with(n),
        |l, n| haystack(l).contains(n),
    ];
    for pass in passes {
        let hits: Vec<&Link> = items.iter().filter(|l| pass(l, &n)).collect();
        match hits.len() {
            0 => continue,
            1 => return Ok(hits[0]),
            _ => {
                return Err(LinkError::Ambiguous(
                    hits.iter().map(|l| l.name.clone()).collect(),
                ))
            }
        }
    }
    Err(LinkError::NotFound(needle.to_string()))
}

/// Is this a URL we are willing to store and hand to a browser?
///
/// A scheme allowlist, not a denylist — a stored URL is handed straight to the platform opener,
/// so `javascript:`, `file:` and `data:` must never reach it. Same rule as the dashboard's task
/// links.
pub fn valid_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && url.len() > "https://".len()
        && !url.contains(char::is_whitespace)
}

/// The name to use when none was given: host plus the last meaningful path segment, so
/// `https://dev.azure.com/contoso/Project/_workitems/edit/11047593` reads as
/// `dev.azure.com/11047593` rather than as the whole URL again.
pub fn default_name(url: &str) -> String {
    let rest = url
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(url)
        .trim_end_matches('/');
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let tail = path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .find(|s| !s.is_empty());
    match tail {
        Some(t) => format!("{host}/{t}"),
        None => host.to_string(),
    }
}

/// Split a comma-separated `--tags` value against the configured vocabulary.
///
/// Returns `(accepted, unknown)` — both are stored. An unlisted tag is a warning, never a
/// rejection: the vocabulary is a habit worth keeping, and losing a capture to enforce it would
/// be a worse trade than a slightly untidy tag list.
pub fn split_tags(raw: &str, vocab: &[String]) -> (Vec<String>, Vec<String>) {
    let known: Vec<String> = vocab.iter().map(|t| t.to_lowercase()).collect();
    let mut accepted = Vec::new();
    let mut unknown = Vec::new();
    for t in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let t = t.to_lowercase();
        if accepted.contains(&t) {
            continue;
        }
        if !known.contains(&t) {
            unknown.push(t.clone());
        }
        accepted.push(t);
    }
    (accepted, unknown)
}

/// One thing a branch note offers the link store, as an openable URL.
#[derive(Debug, Clone, PartialEq)]
pub struct Capturable {
    /// What it is, for the stored name — "PR" or "work item".
    pub label: String,
    pub url: String,
}

/// The note's PR and work item as URLs a browser can open.
///
/// A value that is already http(s) passes through. A bare id (`bn link 11047593` stores one)
/// is resolved through this repo's forge, so a work item is capturable rather than skipped.
/// Anything that yields no URL is dropped — an absent link beats one that 404s.
///
/// The single source of truth for "what does this note owe the store": `bn links capture` and
/// `bn close`'s nudge both read it, so they cannot disagree about what is missing.
pub fn capturable(
    cfg: &Config,
    repo: &str,
    pr: Option<&str>,
    work_item: Option<&str>,
) -> Vec<Capturable> {
    // Resolved lazily: a note whose values are already URLs never touches the filesystem.
    let mut forge_links: Option<Option<crate::forge::ForgeLinks>> = None;
    let mut derive = |id: &str| -> Option<String> {
        let links = forge_links.get_or_insert_with(|| {
            let dir = crate::lifecycle::cloned_repo_dirs(cfg).get(repo).cloned()?;
            crate::forge::links_for(&dir)
        });
        crate::forge::item_url(links.as_ref()?, id)
    };
    [("PR", pr), ("work item", work_item)]
        .into_iter()
        .filter_map(|(label, value)| {
            let v = value?.trim();
            let url = if valid_url(v) {
                v.to_string()
            } else {
                derive(v)?
            };
            Some(Capturable {
                label: label.to_string(),
                url,
            })
        })
        .collect()
}

/// How many stored links carry each tag, for `bn links tags`.
pub fn tag_counts(items: &[Link]) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for tag in items.iter().flat_map(|l| l.tags.iter()) {
        *counts.entry(tag.clone()).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(name: &str, uses: u32, last: Option<&str>) -> Link {
        Link {
            id: format!("k{name}"),
            url: format!("https://example.com/{name}"),
            name: name.to_string(),
            tags: vec![],
            repo: None,
            created: "2026-01-01".to_string(),
            uses,
            last: last.map(str::to_string),
        }
    }

    #[test]
    fn the_link_you_reach_for_most_is_the_one_on_top() {
        let items = vec![
            link("rare", 1, Some("2026-09-03")),
            link("daily", 20, Some("2026-09-03")),
        ];
        let ranked = ranked(&items, None, "2026-09-03");
        assert_eq!(ranked[0].name, "daily");
    }

    #[test]
    fn a_link_used_today_outranks_a_busier_one_gone_cold() {
        // 3 uses today (3 * 16 = 48) beats 20 uses last quarter (20 * 1 = 20).
        let items = vec![
            link("stale", 20, Some("2026-01-05")),
            link("fresh", 3, Some("2026-09-03")),
        ];
        let ranked = ranked(&items, None, "2026-09-03");
        assert_eq!(ranked[0].name, "fresh");
    }

    #[test]
    fn the_repo_you_are_standing_in_sorts_first_even_when_it_is_used_less() {
        let mut mine = link("mine", 1, Some("2026-01-01"));
        mine.repo = Some("bn".to_string());
        let items = vec![link("busy", 99, Some("2026-09-03")), mine];
        let ranked = ranked(&items, Some("bn"), "2026-09-03");
        assert_eq!(ranked[0].name, "mine");
        assert_eq!(ranked[1].name, "busy");
    }

    #[test]
    fn a_never_opened_link_still_ranks_by_the_day_it_was_saved() {
        let saved_today = Link {
            created: "2026-09-03".to_string(),
            ..link("new", 0, None)
        };
        let saved_last_year = Link {
            created: "2025-09-03".to_string(),
            ..link("old", 0, None)
        };
        let items = vec![saved_last_year, saved_today];
        let ranked = ranked(&items, None, "2026-09-03");
        assert_eq!(ranked[0].name, "new");
    }

    #[test]
    fn the_store_survives_a_round_trip_through_yaml() {
        let items = vec![Link {
            tags: vec!["dri".into(), "telemetry".into()],
            repo: Some("BigMono".into()),
            ..link("pbi", 4, Some("2026-09-02"))
        }];
        let front = LinksFront {
            items: items.clone(),
        };
        let text = format!(
            "---\n{}---\n\n# Links\n",
            serde_yaml::to_string(&front).unwrap()
        );
        let back = crate::store::parse_frontmatter::<LinksFront>(&text).unwrap();
        assert_eq!(back.items, items);
    }

    #[test]
    fn resolve_finds_a_link_by_a_fragment_of_its_url() {
        let items = vec![link("Prod subscription", 0, None)];
        assert_eq!(
            resolve(&items, "example.com/Prod").unwrap().name,
            "Prod subscription"
        );
    }

    #[test]
    fn resolve_refuses_to_guess_between_two_matches() {
        let items = vec![link("portal one", 0, None), link("portal two", 0, None)];
        let err = resolve(&items, "portal").unwrap_err();
        assert!(
            matches!(err, LinkError::Ambiguous(ref c) if c.len() == 2),
            "{err:?}"
        );
    }

    #[test]
    fn an_exact_id_wins_over_any_name_match() {
        let mut decoy = link("k1", 0, None);
        decoy.id = "k9".to_string();
        let mut wanted = link("something else", 0, None);
        wanted.id = "k1".to_string();
        let items = vec![decoy, wanted];
        assert_eq!(resolve(&items, "k1").unwrap().name, "something else");
    }

    #[test]
    fn only_http_urls_are_storable() {
        assert!(valid_url("https://example.com/x"));
        assert!(valid_url("http://example.com"));
        assert!(!valid_url("javascript:alert(1)"));
        assert!(!valid_url("file:///etc/passwd"));
        assert!(!valid_url("data:text/html,<script>"));
        assert!(!valid_url("example.com"));
        assert!(!valid_url("https://exa mple.com"));
    }

    #[test]
    fn a_missing_name_becomes_the_host_and_the_last_path_segment() {
        assert_eq!(
            default_name("https://dev.azure.com/contoso/Project/_workitems/edit/11047593"),
            "dev.azure.com/11047593"
        );
        assert_eq!(default_name("https://example.com"), "example.com");
        assert_eq!(default_name("https://example.com/"), "example.com");
        assert_eq!(
            default_name("https://example.com/docs?q=1"),
            "example.com/docs"
        );
    }

    /// A config whose `bare_dir()` is `<root>/bare`, holding one bare clone with `remote` as its
    /// origin — all `forge::links_for` reads.
    fn cfg_with_clone(root: &std::path::Path, repo: &str, remote: &str) -> crate::resolver::Config {
        let bare = root.join("bare").join(format!("{repo}.git"));
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(
            bare.join("config"),
            format!("[remote \"origin\"]\n\turl = {remote}\n"),
        )
        .unwrap();
        crate::test_support::config_with_roots(root, root, root.join("worktrees"), "testhost")
    }

    #[test]
    fn a_bare_work_item_id_is_captured_as_its_forge_url() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with_clone(
            tmp.path(),
            "bigmono",
            "https://dev.azure.com/contoso/Project/_git/BigMono",
        );
        let got = capturable(
            &cfg,
            "bigmono",
            Some("https://github.com/eduuh/bn/pull/1250"),
            Some("11047593"),
        );
        assert_eq!(
            got,
            vec![
                Capturable {
                    label: "PR".into(),
                    url: "https://github.com/eduuh/bn/pull/1250".into()
                },
                Capturable {
                    label: "work item".into(),
                    url: "https://dev.azure.com/contoso/Project/_workitems/edit/11047593".into()
                },
            ]
        );
    }

    #[test]
    fn an_id_no_forge_can_place_is_dropped_rather_than_stored_broken() {
        let tmp = tempfile::tempdir().unwrap();
        // No bare clone for this repo at all — nothing to derive from.
        let cfg =
            crate::test_support::config_with_roots(tmp.path(), tmp.path(), tmp.path(), "testhost");
        assert!(capturable(&cfg, "nope", None, Some("11047593")).is_empty());
    }

    #[test]
    fn a_note_with_neither_owes_the_store_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg =
            crate::test_support::config_with_roots(tmp.path(), tmp.path(), tmp.path(), "testhost");
        assert!(capturable(&cfg, "bn", None, None).is_empty());
    }

    #[test]
    fn an_unlisted_tag_is_reported_but_still_kept() {
        let vocab = vec!["dri".to_string(), "telemetry".to_string()];
        let (accepted, unknown) = split_tags("DRI, sprawl, telemetry", &vocab);
        assert_eq!(accepted, vec!["dri", "sprawl", "telemetry"]);
        assert_eq!(unknown, vec!["sprawl"]);
    }

    #[test]
    fn tags_are_lowercased_and_deduplicated() {
        let (accepted, _) = split_tags("Security, security, , SECURITY", &[]);
        assert_eq!(accepted, vec!["security"]);
    }
}
