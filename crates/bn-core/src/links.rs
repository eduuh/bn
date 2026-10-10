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

pub mod check;
pub mod when;
use serde::{Deserialize, Serialize};

/// The tag vocabulary bn ships with, used when `[links] tags` is absent from config.toml.
///
/// Absent and empty mean different things: a machine whose config predates the setting should
/// still get a working vocabulary, while an explicit `tags = []` is someone saying they want
/// none. Kept short on purpose — six tags you use beat twenty you don't.
pub const DEFAULT_TAGS: [&str; 11] = [
    "icm",
    "docs",
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
    /// The branch it was saved from, so that branch's note lists it first. `None` for links
    /// saved outside a worktree or before links carried a branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub created: String,
    #[serde(default)]
    pub uses: u32,
    /// Date of the most recent open. `None` until the link has been opened once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    /// Saved by the session hook and not yet reviewed with `bn links new`. Never set by an
    /// explicit add or capture: a link you chose is already reviewed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
    /// The first date (`YYYY-MM-DD`) the link is hidden from every listing, from `--ttl`.
    /// Hidden, never deleted on its own: `bn links prune` deletes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
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
    branch: Option<&str>,
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
        .map(str::to_string);

    if let Some(existing) = items.iter_mut().find(|l| l.url == url) {
        // No name given keeps the one already there: a name is usually hand-written, and
        // re-adding a URL to re-tag it must not quietly reset it to `host/segment`.
        if let Some(name) = name {
            existing.name = name;
        }
        if !tags.is_empty() {
            existing.tags = tags.to_vec();
        }
        if repo.is_some() {
            existing.repo = repo.map(str::to_string);
        }
        if branch.is_some() {
            existing.branch = branch.map(str::to_string);
        }
        let out = existing.clone();
        write(cfg, &items)?;
        return Ok(out);
    }

    let link = Link {
        id: crate::id::next_id_from_ids(items.iter().map(|l| l.id.as_str()), K_PREFIX),
        url: url.to_string(),
        name: name.unwrap_or_else(|| default_name(url)),
        tags: tags.to_vec(),
        repo: repo.map(str::to_string),
        branch: branch.map(str::to_string),
        created: today.to_string(),
        uses: 0,
        last: None,
        new: false,
        expires: None,
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

/// Add and drop tags on one link without restating the rest — the way into and out of a group.
/// Tags are lowercased; adding one it already has, or dropping one it lacks, is a no-op.
pub fn retag(cfg: &Config, id: &str, add: &[String], drop: &[String]) -> Result<Link, LinkError> {
    edit(cfg, id, |l| {
        for t in drop {
            let t = t.to_lowercase();
            l.tags.retain(|x| *x != t);
        }
        for t in add {
            let t = t.to_lowercase();
            if !l.tags.contains(&t) {
                l.tags.push(t);
            }
        }
    })
}

/// Rename one link. A blank name is refused as not-found-shaped rather than stored.
pub fn rename(cfg: &Config, id: &str, name: &str) -> Result<Link, LinkError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LinkError::NotFound(id.to_string()));
    }
    edit(cfg, id, |l| l.name = name.to_string())
}

/// Apply `f` to the link with this id under the store lock, and write it back.
fn edit(cfg: &Config, id: &str, f: impl FnOnce(&mut Link)) -> Result<Link, LinkError> {
    let _lock = crate::store::NoteLock::acquire(&cfg.links_file());
    let mut items = read(cfg);
    let link = items
        .iter_mut()
        .find(|l| l.id == id)
        .ok_or_else(|| LinkError::NotFound(id.to_string()))?;
    f(link);
    let out = link.clone();
    write(cfg, &items)?;
    Ok(out)
}

/// The links waiting for review — saved by the session hook, not yet kept, tagged or dropped —
/// oldest first, expired ones left out.
pub fn pending<'a>(items: &'a [Link], today: &str) -> Vec<&'a Link> {
    items
        .iter()
        .filter(|l| l.new && !expired(l, today))
        .collect()
}

/// Clear the new mark on these links: they have been looked at. Returns how many changed.
pub fn mark_reviewed(cfg: &Config, ids: &[String]) -> Result<usize, LinkError> {
    let _lock = crate::store::NoteLock::acquire(&cfg.links_file());
    let mut items = read(cfg);
    let mut changed = 0;
    for l in items.iter_mut().filter(|l| l.new && ids.contains(&l.id)) {
        l.new = false;
        changed += 1;
    }
    if changed > 0 {
        write(cfg, &items)?;
    }
    Ok(changed)
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
/// frecency, then most-recently-used, then by name so the order is stable across runs. Expired
/// links are left out, so every listing hides them without its own filter.
pub fn ranked<'a>(items: &'a [Link], repo: Option<&str>, today: &str) -> Vec<&'a Link> {
    let mut out: Vec<&Link> = items.iter().filter(|l| !expired(l, today)).collect();
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

/// Parse a `--ttl` (`7d`, `2w`, or bare days `30`) into whole days. `None` for anything else,
/// including zero: a link that expires the moment it is saved is a mistake, not a choice.
pub fn parse_ttl(raw: &str) -> Option<i64> {
    let raw = raw.trim().to_lowercase();
    let (num, per) = match raw.strip_suffix('w') {
        Some(n) => (n, 7),
        None => (raw.strip_suffix('d').unwrap_or(&raw), 1),
    };
    let n: i64 = num.parse().ok()?;
    (n > 0).then_some(n * per)
}

/// Give one link an expiry `days` from `today` — it disappears from every listing on that date.
pub fn set_expiry(cfg: &Config, id: &str, today: &str, days: i64) -> Result<Link, LinkError> {
    let date =
        crate::util::add_days(today, days).ok_or_else(|| LinkError::NotFound(id.to_string()))?;
    edit(cfg, id, |l| l.expires = Some(date))
}

/// Delete every expired link, returning what went. The only thing that deletes on expiry —
/// [`ranked`] merely hides them, so an expiry can be undone by editing the date until pruned.
pub fn prune(cfg: &Config, today: &str) -> Result<Vec<Link>, LinkError> {
    let _lock = crate::store::NoteLock::acquire(&cfg.links_file());
    let items = read(cfg);
    let (gone, kept): (Vec<Link>, Vec<Link>) = items.into_iter().partition(|l| expired(l, today));
    if !gone.is_empty() {
        write(cfg, &kept)?;
    }
    Ok(gone)
}

/// Has this link reached its `expires` date? Dates compare as `YYYY-MM-DD` strings.
pub fn expired(link: &Link, today: &str) -> bool {
    link.expires.as_deref().is_some_and(|e| e <= today)
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

/// True when `q` appears (case-insensitively) in the link's name, tags or URL — the filter
/// behind `bn links list <words>` and the MCP `bn_links_search` tool.
pub fn matches_query(l: &Link, q: &str) -> bool {
    format!("{} {} {}", l.name, l.tags.join(" "), l.url)
        .to_lowercase()
        .contains(&q.to_lowercase())
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

/// Host → tag rules for [`guess_tags`]. A rule matches the host exactly, as a `.suffix`, or as a
/// `prefix.`/`prefix*` start. Kept small and obvious: a wrong guess is a tag you have to clean up.
const GUESS_RULES: [(&str, &[&str]); 4] = [
    ("icm", &[".example-incidents.com", "icm."]),
    (
        "docs",
        &[
            "learn.microsoft.com",
            "docs.microsoft.com",
            "docs.example.com",
            "docs.",
            ".readthedocs.io",
        ],
    ),
    (
        "permissions",
        &[
            "myaccess.microsoft.com",
            "groups.example*",
            "identity.example.com",
        ],
    ),
    (
        "security",
        &["security.microsoft.com", ".sdl.example.com"],
    ),
];

/// The URL's host, lowercased, without port or credentials.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    authority.split(':').next().unwrap_or("").to_lowercase()
}

/// Tags suggested by the URL's host, restricted to the active vocabulary so a config that
/// dropped a tag never gets it back through a guess. Callers apply this only when no tags were
/// given; `capture` never does, since its links are grouped by repo already.
pub fn guess_tags(url: &str, vocab: &[String]) -> Vec<String> {
    let host = host(url);
    let known: Vec<String> = vocab.iter().map(|t| t.to_lowercase()).collect();
    GUESS_RULES
        .iter()
        .filter(|(tag, _)| known.iter().any(|k| k == tag))
        .filter(|(_, pats)| {
            pats.iter().any(|p| {
                if let Some(suffix) = p.strip_prefix('.') {
                    host.ends_with(&format!(".{suffix}")) || host == suffix
                } else if let Some(prefix) = p.strip_suffix('*') {
                    host.starts_with(prefix)
                } else if p.ends_with('.') {
                    host.starts_with(p)
                } else {
                    host == *p
                }
            })
        })
        .map(|(tag, _)| tag.to_string())
        .collect()
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

/// Store a branch's PR and work item, named `<repo> <branch> <PR|work item>` and filed under the
/// repo. Returns the links newly saved; a URL already in the store is left exactly as it is, so
/// a name or tags you gave it by hand survive a later capture.
///
/// This is what [`crate::commands::close`] runs, so every way a worktree goes away — closing
/// the note, `bn sync` seeing the PR merge, worktree cleanup — keeps its links first.
pub fn capture(
    cfg: &Config,
    repo: &str,
    branch: &str,
    pr: Option<&str>,
    work_item: Option<&str>,
    today: &str,
) -> Result<Vec<Link>, LinkError> {
    let stored = read(cfg);
    let mut saved = Vec::new();
    for c in capturable(cfg, repo, pr, work_item) {
        if stored.iter().any(|l| l.url == c.url) {
            continue;
        }
        let name = format!("{repo} {branch} {}", c.label);
        saved.push(add(
            cfg,
            &c.url,
            Some(&name),
            &[],
            Some(repo),
            Some(branch),
            today,
        )?);
    }
    Ok(saved)
}

/// The `{name}` placeholders in a link's URL, each once, in order. A link with any is a
/// template: `https://dev.azure.com/o/p/_workitems/edit/{id}` stands for every work item, and
/// opening it asks for `id`.
pub fn placeholders(url: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = url;
    while let Some(open) = rest.find('{') {
        let Some(len) = rest[open..].find('}') else {
            break;
        };
        let name = &rest[open + 1..open + len];
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &rest[open + len + 1..];
    }
    out
}

/// Fill a template's placeholders. Values are percent-encoded, so `a b/c` cannot break out of
/// its path segment or query value; a placeholder with no value is left as it is.
pub fn fill(url: &str, values: &[(String, String)]) -> String {
    let mut out = url.to_string();
    for (name, value) in values {
        out = out.replace(&format!("{{{name}}}"), &encode(value));
    }
    out
}

/// Percent-encode everything outside RFC 3986's unreserved set.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The URLs in `text` worth keeping without being asked: pull requests, issues, work items,
/// portal blades, and the incident, permission and security hosts the tag rules recognise. Everything else a session mentions
/// (a crate's docs, a search result) stays out, so the store fills with what you return to
/// rather than everything that scrolled past. In order of appearance, without duplicates.
pub fn notable_urls(text: &str) -> Vec<String> {
    // Docs pages are left out: agents cite them constantly, and one you return to is one
    // you save on purpose.
    let rule_tags: Vec<String> = GUESS_RULES
        .iter()
        .map(|(t, _)| t.to_string())
        .filter(|t| t != "docs")
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("http") {
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '<' | '>' | '`' | '|')
            })
            .unwrap_or(tail.len());
        let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '*']);
        rest = &tail[end.max(1)..];
        if !(url.starts_with("https://") || url.starts_with("http://")) || !valid_url(url) {
            continue;
        }
        if (is_work_url(url) || !guess_tags(url, &rule_tags).is_empty())
            && !out.iter().any(|u| u == url)
        {
            out.push(url.to_string());
        }
    }
    out
}

/// A PR, an issue or a work item on GitHub or Azure DevOps, or an Azure portal page.
fn is_work_url(url: &str) -> bool {
    let numbered = |marker: &str| {
        url.split_once(marker)
            .is_some_and(|(_, tail)| tail.chars().next().is_some_and(|c| c.is_ascii_digit()))
    };
    numbered("/pull/")
        || numbered("/issues/")
        || numbered("/pullrequest/")
        || numbered("/_workitems/edit/")
        || host(url) == "portal.azure.com"
}

/// Store the URLs a session surfaced that the store does not hold yet, named by default,
/// tagged by host and marked [`Link::new`]. A URL already stored is left alone — its name and
/// tags may be hand-made.
pub fn harvest(
    cfg: &Config,
    urls: &[String],
    repo: Option<&str>,
    branch: Option<&str>,
    today: &str,
) -> Result<Vec<Link>, LinkError> {
    let stored = read(cfg);
    let vocab = crate::resolver::link_tags();
    let mut saved = Vec::new();
    for url in urls {
        if stored.iter().any(|l| &l.url == url) {
            continue;
        }
        let tags = guess_tags(url, &vocab);
        let link = add(cfg, url, None, &tags, repo, branch, today)?;
        // Saved without anyone choosing it, so it waits in `bn links new` for a look.
        saved.push(edit(cfg, &link.id, |l| l.new = true)?);
    }
    Ok(saved)
}

/// How many of a repo's links `bn cat` shows. Enough for the board, the PR and a dashboard;
/// the picker has the rest.
pub const NOTE_LINKS: usize = 5;

/// What `bn cat` lists under the note, at most `n`: the links saved on this branch, then the
/// rest of the repo's, each in picker order.
pub fn for_note(cfg: &Config, repo: &str, branch: &str, today: &str, n: usize) -> Vec<Link> {
    let items = read(cfg);
    let (mine, rest): (Vec<&Link>, Vec<&Link>) = ranked(&items, Some(repo), today)
        .into_iter()
        .filter(|l| l.repo.as_deref() == Some(repo))
        .partition(|l| l.branch.as_deref() == Some(branch));
    mine.into_iter().chain(rest).take(n).cloned().collect()
}

/// How many stored links carry each tag, for `bn links tags`.
pub fn tag_counts(items: &[Link]) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for tag in items.iter().flat_map(|l| l.tags.iter()) {
        *counts.entry(tag.clone()).or_insert(0) += 1;
    }
    counts
}

/// A tag shared by at least this many links is offered as a group in the picker. A group of one
/// is just the link, so it would only add a duplicate row.
pub const GROUP_MIN: usize = 2;

/// The groups the picker offers: every tag carried by [`GROUP_MIN`] or more links, each with its
/// members in the order given (pass [`ranked`] output so members keep picker order). Groups sort
/// largest first, then by name, so the order is stable between opens.
pub fn groups<'a>(ranked: &[&'a Link]) -> Vec<(String, Vec<&'a Link>)> {
    let mut out: Vec<(String, Vec<&'a Link>)> = Vec::new();
    for l in ranked {
        for t in &l.tags {
            match out.iter_mut().find(|(name, _)| name == t) {
                Some((_, members)) => members.push(l),
                None => out.push((t.clone(), vec![l])),
            }
        }
    }
    out.retain(|(_, members)| members.len() >= GROUP_MIN);
    out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Every link carrying `tag` (case-insensitive), in store order. Empty when nothing does.
pub fn in_group<'a>(items: &'a [Link], tag: &str) -> Vec<&'a Link> {
    let tag = tag.trim().trim_start_matches('#').to_lowercase();
    items.iter().filter(|l| l.tags.contains(&tag)).collect()
}

#[cfg(test)]
mod tests;
