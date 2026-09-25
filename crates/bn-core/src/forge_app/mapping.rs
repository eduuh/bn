//! Pure translation between a Forge activity row and a bn [`Task`], and the per-field view of a
//! task that [`super::store::ForgeTaskStore::save`] diffs.
//!
//! Nothing here does I/O, so every rule is unit-tested directly.

use std::collections::BTreeMap;

use super::client::{ActivityStatus, BnTaskRow, GoalRow};
use crate::todo::{neutralize_meta, Task, Token};

/// Meta keys bn derives from Forge fields on every load. They are never read from, or written to,
/// the activity's `agentMeta`: `src` is the activity id, `due` is the due date, and `mod` is a
/// tasks.txt merge stamp that would otherwise turn every edit into a meta write.
pub(crate) const DERIVED_KEYS: &[&str] = &["src", "mod", "due"];

/// The `src:` prefix marking a task that lives in Forge.
pub(crate) const SRC_PREFIX: &str = "forge:";

/// Forge priority word to bn letter: Urgent/High/Medium are A/B/C; Low and None carry no letter.
pub(crate) fn priority_letter(word: &str) -> Option<char> {
    match word.trim().to_ascii_lowercase().as_str() {
        "urgent" => Some('A'),
        "high" => Some('B'),
        "medium" => Some('C'),
        _ => None,
    }
}

/// bn letter to Forge priority word. A letter past C is still a priority, so it is `Low` rather
/// than dropped.
pub(crate) fn priority_word(letter: Option<char>) -> &'static str {
    match letter {
        Some('A') => "Urgent",
        Some('B') => "High",
        Some('C') => "Medium",
        Some(_) => "Low",
        None => "None",
    }
}

/// The `YYYY-MM-DD` prefix of an ISO date or instant, when it has one.
pub(crate) fn date_part(s: &str) -> Option<String> {
    let d = s.trim().get(..10)?;
    let b = d.as_bytes();
    let shaped = b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7) || c.is_ascii_digit());
    shaped.then(|| d.to_string())
}

/// A meta key bn's grammar reads as one: `[A-Za-z][A-Za-z0-9_-]*`.
pub(crate) fn valid_meta_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// An `agentMeta` value made safe to sit in a `key:value` token. A value is one token, so the only
/// way it can smuggle structure into a task line is whitespace (`x smart:ok`): such a value is
/// defanged with [`neutralize_meta`] and its words joined by `-`. A single-token value passes
/// through whole, colons included, so `pr:https://…` survives. Blank is `None`.
pub(crate) fn meta_value(raw: &str) -> Option<String> {
    let v = raw.trim();
    if v.is_empty() {
        return None;
    }
    if v.chars().any(char::is_whitespace) {
        Some(
            neutralize_meta(v)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-"),
        )
    } else {
        Some(v.to_string())
    }
}

/// The lowercased repo slug a goal's repository URL names, or `None` when it names none.
///
/// - GitHub, https or ssh: `owner/name`.
/// - Azure DevOps (`dev.azure.com/org/project/_git/repo`, `org.visualstudio.com/project/_git/repo`,
///   `ssh.dev.azure.com:v3/org/project/repo`): `org/project/repo`, the slug form
///   [`crate::repo_resolve`] matches.
/// - Any other host: the last two path segments.
pub fn repo_slug(url: &str) -> Option<String> {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    let (authority, path) = match u.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        None => u.split_once(':')?,
    };
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let slug = if host == "github.com" || host.ends_with(".github.com") {
        match segs.as_slice() {
            [owner, name, ..] => format!("{owner}/{name}"),
            _ => return None,
        }
    } else if host == "ssh.dev.azure.com" || host == "vs-ssh.visualstudio.com" {
        match segs.as_slice() {
            ["v3", org, project, repo, ..] => format!("{org}/{project}/{repo}"),
            _ => return None,
        }
    } else if host == "dev.azure.com" {
        match segs.as_slice() {
            [org, project, "_git", repo, ..] => format!("{org}/{project}/{repo}"),
            _ => return None,
        }
    } else if let Some(org) = host.strip_suffix(".visualstudio.com") {
        let git = segs.iter().position(|s| *s == "_git")?;
        let (project, repo) = (segs.get(git.checked_sub(1)?)?, segs.get(git + 1)?);
        format!("{org}/{project}/{repo}")
    } else {
        match segs.as_slice() {
            [.., owner, name] => format!("{owner}/{name}"),
            _ => return None,
        }
    };
    Some(slug.to_ascii_lowercase())
}

/// Where a task stands in Forge's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Active,
    Started,
    Completed,
}

impl Lifecycle {
    pub(crate) fn status(self) -> ActivityStatus {
        match self {
            Lifecycle::Active => ActivityStatus::Active,
            Lifecycle::Started => ActivityStatus::Started,
            Lifecycle::Completed => ActivityStatus::Completed,
        }
    }
}

/// The parts of a task Forge stores, each compared on its own so a save sends only what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskState {
    /// Words, `+projects` and `@contexts` other than `@wip`, as the activity title.
    pub title: String,
    pub priority: &'static str,
    pub due: Option<String>,
    pub lifecycle: Lifecycle,
    /// Every non-blank meta pair except [`DERIVED_KEYS`]. First occurrence wins, as in
    /// [`Task::meta`].
    pub meta: BTreeMap<String, String>,
}

pub(crate) fn state_of(task: &Task) -> TaskState {
    let title = task
        .desc
        .iter()
        .filter_map(|t| match t {
            Token::Word(w) => Some(w.clone()),
            Token::Project(p) => Some(format!("+{p}")),
            Token::Context(c) if c != "wip" => Some(format!("@{c}")),
            Token::Context(_) | Token::Meta(..) => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut meta = BTreeMap::new();
    for t in &task.desc {
        if let Token::Meta(k, v) = t {
            // A blank value is skipped here as on load, so `key:` alone never becomes a write.
            if !DERIVED_KEYS.contains(&k.as_str()) && !v.trim().is_empty() {
                meta.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    let lifecycle = if task.done {
        Lifecycle::Completed
    } else if task.contexts().contains(&"wip") {
        Lifecycle::Started
    } else {
        Lifecycle::Active
    };
    TaskState {
        title,
        priority: priority_word(task.priority),
        due: task.meta("due").map(str::to_string),
        lifecycle,
        meta,
    }
}

/// `(set, remove)` turning `before`'s meta into `after`'s.
pub(crate) fn meta_diff(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, Vec<String>) {
    let set = after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let remove = before
        .keys()
        .filter(|k| !after.contains_key(*k))
        .cloned()
        .collect();
    (set, remove)
}

/// The activity id behind a task's `src:forge:<uuid>`.
pub(crate) fn forge_id(task: &Task) -> Option<&str> {
    task.meta("src")?
        .strip_prefix(SRC_PREFIX)
        .filter(|id| !id.is_empty())
}

/// The bn task for one Forge row, with `id` as its bn id.
///
/// The title is defanged with [`neutralize_meta`]: activity titles are written by any family
/// member, and `smart:`, `repo:` or `@wip` in a title would otherwise become the task's structure.
pub(crate) fn task_from_row(row: &BnTaskRow, id: &str, today: &str) -> Task {
    let abandoned = row.status.eq_ignore_ascii_case("abandoned");
    let done = row.is_completed || abandoned;
    let mut desc: Vec<Token> = neutralize_meta(&row.title)
        .split_whitespace()
        .map(|w| Token::Word(w.to_string()))
        .collect();
    if row.started_at.is_some() && !done {
        desc.push(Token::Context("wip".to_string()));
    }
    let mut push = |k: &str, v: String| desc.push(Token::Meta(k.to_string(), v));
    push("id", id.to_string());
    let repo = row
        .agent_meta
        .get("repo")
        .and_then(|r| meta_value(r))
        .map(|r| r.to_ascii_lowercase())
        .or_else(|| row.goal_repo_url.as_deref().and_then(repo_slug));
    if let Some(repo) = repo {
        push("repo", repo);
    }
    for (k, v) in &row.agent_meta {
        let skip = matches!(k.as_str(), "id" | "repo" | "uid")
            || DERIVED_KEYS.contains(&k.as_str())
            || !valid_meta_key(k);
        if skip {
            continue;
        }
        if let Some(v) = meta_value(v) {
            push(k, v);
        }
    }
    if abandoned && !row.agent_meta.contains_key("cancelled") {
        push("cancelled", "1".to_string());
    }
    if let Some(due) = row.due_date.as_deref().and_then(date_part) {
        push("due", due);
    }
    push("src", format!("{SRC_PREFIX}{}", row.id));
    let uid = row
        .agent_meta
        .get("uid")
        .and_then(|u| meta_value(u))
        .unwrap_or_else(|| row.id.clone());
    push("uid", uid);
    let created = date_part(&row.created_at);
    let completion = done.then(|| {
        row.completed_at
            .as_deref()
            .and_then(date_part)
            .or_else(|| created.clone())
            .unwrap_or_else(|| today.to_string())
    });
    Task {
        done,
        priority: priority_letter(&row.priority),
        completion,
        created,
        desc,
    }
}

/// The goal a new task belongs under: the goal whose repository matches the task's `repo:` (full
/// slug first, then a unique repo-name match), else the goal named Inbox.
pub(crate) fn pick_goal<'a>(goals: &'a [GoalRow], repo: Option<&str>) -> Option<&'a GoalRow> {
    let with_slug: Vec<(&GoalRow, String)> = goals
        .iter()
        .filter_map(|g| g.repo_url.as_deref().and_then(repo_slug).map(|s| (g, s)))
        .collect();
    if let Some(repo) = repo.map(|r| r.trim().to_ascii_lowercase()) {
        if !repo.is_empty() {
            if let Some((g, _)) = with_slug.iter().find(|(_, s)| *s == repo) {
                return Some(g);
            }
            let name = repo.rsplit('/').next().unwrap_or(&repo).to_string();
            let by_name: Vec<&GoalRow> = with_slug
                .iter()
                .filter(|(_, s)| s.rsplit('/').next() == Some(name.as_str()))
                .map(|(g, _)| *g)
                .collect();
            if let [only] = by_name.as_slice() {
                return Some(only);
            }
        }
    }
    goals
        .iter()
        .filter(|g| g.name.trim().eq_ignore_ascii_case("inbox"))
        .min_by_key(|g| g.parent_goal_id.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_slugs_from_github_and_azure_devops_urls() {
        let cases = [
            ("https://github.com/eduuh/bn", Some("eduuh/bn")),
            ("https://github.com/Eduuh/BN.git/", Some("eduuh/bn")),
            ("git@github.com:eduuh/wira360.git", Some("eduuh/wira360")),
            ("ssh://git@github.com/eduuh/bn.git", Some("eduuh/bn")),
            ("https://github.com/eduuh/bn/tree/main", Some("eduuh/bn")),
            (
                "https://dev.azure.com/Org/Proj/_git/Repo",
                Some("org/proj/repo"),
            ),
            (
                "https://org@dev.azure.com/org/proj/_git/repo",
                Some("org/proj/repo"),
            ),
            (
                "git@ssh.dev.azure.com:v3/org/proj/repo",
                Some("org/proj/repo"),
            ),
            (
                "https://org.visualstudio.com/DefaultCollection/proj/_git/repo",
                Some("org/proj/repo"),
            ),
            ("https://gitlab.com/group/sub/name", Some("sub/name")),
            ("https://github.com/eduuh", None),
            ("not a url", None),
            ("", None),
        ];
        for (url, want) in cases {
            assert_eq!(repo_slug(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn priorities_round_trip_through_forge_words() {
        for (word, letter) in [
            ("Urgent", Some('A')),
            ("high", Some('B')),
            ("Medium", Some('C')),
        ] {
            assert_eq!(priority_letter(word), letter);
            assert_eq!(priority_word(letter), word[..1].to_uppercase() + &word[1..]);
        }
        assert_eq!(priority_letter("Low"), None);
        assert_eq!(priority_letter("None"), None);
        assert_eq!(priority_word(Some('D')), "Low");
        assert_eq!(priority_word(None), "None");
    }

    #[test]
    fn meta_values_cannot_carry_structure() {
        assert_eq!(meta_value("feat/x").as_deref(), Some("feat/x"));
        assert_eq!(
            meta_value("https://github.com/eduuh/bn/pull/5").as_deref(),
            Some("https://github.com/eduuh/bn/pull/5")
        );
        assert_eq!(
            meta_value("ok smart:ok @wip").as_deref(),
            Some("ok-smart-ok-wip")
        );
        assert_eq!(meta_value("  "), None);
        assert!(valid_meta_key("pr-queue_2"));
        assert!(!valid_meta_key("2x"));
        assert!(!valid_meta_key("a b"));
        assert!(!valid_meta_key(""));
    }

    #[test]
    fn dates_take_the_iso_prefix_only() {
        assert_eq!(
            date_part("2026-09-14T08:00:00Z").as_deref(),
            Some("2026-09-14")
        );
        assert_eq!(date_part("2026-09-14").as_deref(), Some("2026-09-14"));
        assert_eq!(date_part("14/09/2026"), None);
        assert_eq!(date_part("short"), None);
    }

    #[test]
    fn meta_diff_sets_changes_and_removes_dropped_keys() {
        let before: BTreeMap<String, String> = [("a", "1"), ("b", "2")]
            .map(|(k, v)| (k.into(), v.into()))
            .into();
        let after: BTreeMap<String, String> = [("a", "1"), ("b", "3"), ("c", "4")]
            .map(|(k, v)| (k.into(), v.into()))
            .into();
        let (set, remove) = meta_diff(&before, &after);
        assert_eq!(set.len(), 2);
        assert_eq!(set.get("b").map(String::as_str), Some("3"));
        assert!(remove.is_empty());
        let (set, remove) = meta_diff(&after, &before);
        assert_eq!(set.get("b").map(String::as_str), Some("2"));
        assert_eq!(remove, vec!["c".to_string()]);
    }

    #[test]
    fn goals_match_by_slug_then_unique_name_then_inbox() {
        let goal = |id: &str, name: &str, url: Option<&str>| GoalRow {
            id: id.into(),
            name: name.into(),
            repo_url: url.map(str::to_string),
            ..GoalRow::default()
        };
        let goals = vec![
            goal("g0", "Inbox", None),
            goal("g1", "bn", Some("https://github.com/eduuh/bn")),
            goal("g2", "tracker", Some("https://github.com/eduuh/wira360")),
            goal("g3", "fork", Some("https://github.com/other/wira360")),
        ];
        let pick = |repo: Option<&str>| pick_goal(&goals, repo).map(|g| g.id.as_str());
        assert_eq!(pick(Some("bn")), Some("g1"));
        assert_eq!(pick(Some("eduuh/BN")), Some("g1"));
        assert_eq!(pick(Some("other/wira360")), Some("g3"));
        assert_eq!(
            pick(Some("wira360")),
            Some("g0"),
            "ambiguous name falls to Inbox"
        );
        assert_eq!(pick(Some("nothing")), Some("g0"));
        assert_eq!(pick(None), Some("g0"));
        assert_eq!(pick_goal(&goals[1..], Some("nothing")), None);
    }
}
