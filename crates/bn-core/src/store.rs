//! Read/write `note.md`. Reads all three on-disk layouts; writes the new single-file
//! format (DECISIONS.md D-read):
//!   1. md-only legacy — frontmatter + `## Todos`/… markdown sections, no sidecar.
//!   2. split — prose `note.md` + sibling `note.yaml` holding the item lists.
//!   3. single-file — frontmatter embeds the item lists; body is prose only.
//!
//! On write we always emit single-file. Reading a legacy note therefore migrates it
//! in memory (items lifted into the frontmatter, item sections stripped from the body).

use crate::model::{Frontmatter, Item, Note, Section};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Parse(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "io error: {e}"),
            StoreError::Parse(m) => write!(f, "parse error: {m}"),
        }
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// Read a note from its `note.md` path, detecting the layout.
pub fn read(note_md: &Path) -> Result<Note, StoreError> {
    let text = std::fs::read_to_string(note_md)?;

    let yaml_sibling = note_md.with_extension("yaml");
    if yaml_sibling.exists() {
        // Split layout: items are authoritative in note.yaml; note.md is prose only and
        // (in this layout) usually has NO frontmatter fence of its own.
        let ytext = std::fs::read_to_string(&yaml_sibling)?;
        let front: Frontmatter =
            serde_yaml::from_str(&ytext).map_err(|e| StoreError::Parse(e.to_string()))?;
        let body = split_frontmatter(&text)
            .map(|(_, b)| b)
            .unwrap_or(text.as_str());
        return Ok(Note {
            front,
            body: strip_item_sections(body),
        });
    }

    let (front_block, body) = split_frontmatter(&text)
        .ok_or_else(|| StoreError::Parse("missing frontmatter fence".into()))?;

    if front_block.lines().any(|l| {
        // Match the `todos:` mapping key exactly — not a stray field like `todos_count:`,
        // which would misclassify a legacy md-only note as single-file and drop its body items.
        let t = l.trim_start();
        t == "todos:" || t.starts_with("todos: ") || t.starts_with("todos:\t")
    }) {
        // Single-file: frontmatter already embeds the item lists.
        let front: Frontmatter =
            serde_yaml::from_str(front_block).map_err(|e| StoreError::Parse(e.to_string()))?;
        return Ok(Note {
            front,
            body: body.trim_end().to_string(),
        });
    }

    // md-only legacy: parse the markdown item sections into the frontmatter.
    let mut front: Frontmatter =
        serde_yaml::from_str(front_block).map_err(|e| StoreError::Parse(e.to_string()))?;
    parse_md_sections_into(&mut front, body);
    Ok(Note {
        front,
        body: strip_item_sections(body),
    })
}

/// Persist a note as single-file, migrating a legacy split note in place. The note is written
/// **atomically** (see [`write_atomic`]), then any sibling `note.yaml` is retired to
/// `note.yaml.bak` so subsequent reads use the embedded frontmatter.
pub fn save(note: &Note, note_md: &Path) -> Result<(), StoreError> {
    let text = render(note)?;
    if let Some(parent) = note_md.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(note_md, text.as_bytes())?;
    let sibling = note_md.with_extension("yaml");
    if sibling.exists() {
        let bak = note_md.with_extension("yaml.bak");
        std::fs::rename(&sibling, &bak)?;
    }
    Ok(())
}

fn atomic_temp_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let fname = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "note.md".into());
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{fname}.{}.{}.tmp", std::process::id(), seq))
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    {
        let mut f = std::fs::File::create(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    Ok(())
}

fn replace_atomic(tmp: &Path, path: &Path) -> std::io::Result<()> {
    if let Err(e) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(e);
    }
    Ok(())
}

/// Replace an authoritative artifact without exposing a partial write.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = atomic_temp_path(path);
    write_synced(&tmp, bytes)?;
    replace_atomic(&tmp, path)
}

/// Serialize `value` to pretty JSON, mapping a serialization failure to an `io::Error` rather than
/// silently yielding an empty string. Callers persist the result (often via [`write_atomic`]), so a
/// swallowed error would make an empty `""` file durable — losing state. Fail loudly instead (#689).
pub(crate) fn to_json_pretty<T: serde::Serialize>(value: &T) -> std::io::Result<String> {
    serde_json::to_string_pretty(value).map_err(std::io::Error::other)
}

/// Like [`to_json_pretty`] but compact (single line) — for JSONL append logs. See [`to_json_pretty`].
pub(crate) fn to_json_line<T: serde::Serialize>(value: &T) -> std::io::Result<String> {
    serde_json::to_string(value).map_err(std::io::Error::other)
}

/// Serialize `value` before touching the filesystem, then atomically replace `path`.
pub(crate) fn save_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let text = to_json_pretty(value)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(path, text.as_bytes())
}

/// Path of the advisory lock file guarding a target's read-modify-write: `<target>.lock` beside it.
pub fn lock_path(target: &Path) -> PathBuf {
    let mut p = target.as_os_str().to_os_string();
    p.push(".lock");
    PathBuf::from(p)
}

/// A stale lock (owner crashed mid-edit) is stolen after this long so a target can't wedge forever.
pub const LOCK_STALE_SECS: u64 = 30;
/// How long a best-effort [`FileLock::acquire`] waits for a contended lock before giving up and
/// proceeding *unowned* (the atomic write still prevents corruption; only a lost update is at
/// stake, and blocking an edit forever is worse).
///
/// Kept comfortably under [`LOCK_STALE_SECS`]: a live holder releases a note read-modify-write in
/// milliseconds, so the only thing that can push a waiter onto the lossy unowned path is a truly
/// wedged holder — and that one is reclaimed by the stale-steal backstop at [`LOCK_STALE_SECS`].
/// The previous 2s budget was too tight under heavy multi-process contention (many agents
/// appending to one branch note at once): the serialized lock time could exceed it and silently
/// drop an update. A generous budget lets every contender wait its turn instead.
const LOCK_WAIT_MS: u64 = 20_000;
/// How long a durable [`FileLock::acquire_required`] waits before returning `WouldBlock`.
/// Deliberately short: these callers surface the timeout as an error and retry or abort rather than
/// risk proceeding without the lock, so they should not block for the full best-effort budget.
const REQUIRED_LOCK_WAIT_MS: u64 = 2_000;

/// Jittered retry backoff (in ms) for a contended-lock spin. Mixing the pid and attempt count
/// through a multiplicative hash desynchronizes the thundering herd of processes racing on the same
/// `create_new`, cutting the total time they waste colliding. Centered near the old fixed 15ms so a
/// single waiter's latency is unchanged.
fn backoff_ms(attempt: u64) -> u64 {
    let mix = (std::process::id() as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(attempt.wrapping_mul(0x2545_F491_4F6C_DD1D));
    8 + (mix >> 32) % 16
}

/// Best-effort cross-process guard for a load-mutate-atomic-save transaction.
#[must_use = "hold the guard for the whole read-modify-write, then drop it"]
pub struct FileLock {
    path: PathBuf,
    owned: bool,
}

/// Back-compat alias — a note's read-modify-write lock is just a [`FileLock`] on the note file.
pub type NoteLock = FileLock;

impl FileLock {
    /// Acquire (or, on timeout, best-effort skip) the lock for `target`.
    pub fn acquire(target: &Path) -> Self {
        use std::time::{Duration, Instant};
        let path = lock_path(target);
        if let Some(parent) = path.parent() {
            // Best-effort: if this fails the create_new open below can't make the lock file
            // either, so we surface it through the loud `owned: false` path rather than here.
            let _ = std::fs::create_dir_all(parent);
        }
        let start = Instant::now();
        let mut attempt: u64 = 0;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return FileLock { path, owned: true },
                Err(_) => {
                    if lock_is_stale(&path) {
                        // Steal the stale lock; if removal races another stealer the next
                        // create_new attempt just retries, so the error is safe to drop.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() >= Duration::from_millis(LOCK_WAIT_MS) {
                        // Give up waiting and proceed unowned rather than block the edit — but
                        // loudly: without the lock a concurrent read-modify-write can silently
                        // lose an update, so leave the operator a trace instead of a silent race.
                        eprintln!(
                            "bn: warning: proceeding without lock after {LOCK_WAIT_MS}ms on {} — a concurrent edit may be lost",
                            path.display()
                        );
                        return FileLock { path, owned: false };
                    }
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(backoff_ms(attempt)));
                }
            }
        }
    }

    /// Acquire the lock for a durable read-modify-write, failing instead of proceeding unowned.
    pub fn acquire_required(target: &Path) -> std::io::Result<Self> {
        Self::acquire_required_with_wait(
            target,
            std::time::Duration::from_millis(REQUIRED_LOCK_WAIT_MS),
        )
    }

    fn acquire_required_with_wait(
        target: &Path,
        wait: std::time::Duration,
    ) -> std::io::Result<Self> {
        use std::io::ErrorKind;
        use std::time::Instant;

        let path = lock_path(target);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to create lock directory {}: {e}", parent.display()),
                )
            })?;
        }

        let start = Instant::now();
        let mut attempt: u64 = 0;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => {
                    return Ok(FileLock { path, owned: true });
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        match std::fs::remove_file(&path) {
                            Ok(()) => continue,
                            Err(e) if e.kind() == ErrorKind::NotFound => continue,
                            Err(e) => {
                                return Err(std::io::Error::new(
                                    e.kind(),
                                    format!("failed to remove stale lock {}: {e}", path.display()),
                                ));
                            }
                        }
                    }
                    if start.elapsed() >= wait {
                        return Err(std::io::Error::new(
                            ErrorKind::WouldBlock,
                            format!(
                                "timed out after {}ms waiting for lock {}",
                                wait.as_millis(),
                                path.display()
                            ),
                        ));
                    }
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms(attempt)));
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        e.kind(),
                        format!("failed to acquire lock {}: {e}", path.display()),
                    ));
                }
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if self.owned {
            // Release only a lock we created; a failed remove just leaves a file the next
            // acquirer ages out as stale, so the error is safe to drop.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// True if the lock file exists but is older than [`LOCK_STALE_SECS`] — its owner likely crashed.
pub fn lock_is_stale(path: &Path) -> bool {
    use std::time::{Duration, SystemTime};
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| {
            SystemTime::now()
                .duration_since(t)
                .unwrap_or(Duration::ZERO)
                >= Duration::from_secs(LOCK_STALE_SECS)
        })
        .unwrap_or(false)
}

/// Render a note to single-file text: `---\n<frontmatter>---\n\n<body>`.
pub fn render(note: &Note) -> Result<String, StoreError> {
    let fm = serde_yaml::to_string(&note.front).map_err(|e| StoreError::Parse(e.to_string()))?;
    let body = note.body.trim_start_matches('\n').trim_end();
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&fm);
    out.push_str("---\n");
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
        out.push('\n');
    }
    Ok(out)
}

/// Map each item id → its 1-based line number in the rendered single-file text
/// (the line where its `id:` appears in the frontmatter). For the nvim `line` contract.
pub fn item_line_map(text: &str) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    let mut in_front = false;
    for (i, line) in text.lines().enumerate() {
        if line == "---" {
            if in_front {
                break; // end of frontmatter
            }
            in_front = true;
            continue;
        }
        if !in_front {
            continue;
        }
        if let Some(id) = item_id_on_line(line) {
            map.insert(id, i + 1);
        }
    }
    map
}

/// Extract an item id from a frontmatter line like `- id: t1` or `- {id: t1, ...}`.
fn item_id_on_line(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t.strip_prefix("- ")?;
    // block form: "- id: t1"
    if let Some(v) = rest.strip_prefix("id:") {
        return Some(v.trim().trim_matches(|c| c == '"' || c == '\'').to_string());
    }
    // flow form: "- {id: t1, ...}"
    if let Some(inner) = rest.strip_prefix('{') {
        let inner = inner.trim_start();
        if let Some(v) = inner.strip_prefix("id:") {
            let v = v.split(',').next().unwrap_or("");
            return Some(v.trim().trim_matches(|c| c == '"' || c == '\'').to_string());
        }
    }
    None
}

/// Split `---\n<yaml>\n---\n<body>` into (frontmatter-block-without-fences, body-after-fence).
/// Returns `None` if there is no leading `---` fence pair. The frontmatter block keeps the
/// trailing newline of its last field (serde-safe); the body has its leading blank lines
/// trimmed. This is the single source of truth for fence parsing — callers across the crate
/// (notes, daily, skills, migration) route through it rather than hand-slicing `---`.
///
/// Both LF (`\n`) and CRLF (`\r\n`) line endings are accepted: a note.md saved on Windows opens
/// with `---\r\n`, so matching only the LF fence would reject it as missing frontmatter and drop
/// the entire structured block. The returned slices keep whatever endings the source used
/// (serde_yaml parses CRLF fine); only the body's leading blank lines are trimmed.
pub(crate) fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    // The closing fence must stand alone on its line: "\n---" followed by a newline (LF or CRLF)
    // or EOF. A naive `find("\n---")` would also match inside a multi-line YAML value or a "----"
    // rule, truncating the frontmatter and corrupting the parse.
    let mut from = 0;
    let end = loop {
        let idx = rest[from..].find("\n---")? + from;
        let after = &rest[idx + 4..];
        if after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n") {
            break idx;
        }
        from = idx + 1;
    };
    // Keep the last field's trailing newline (serde-safe); then skip the "\n---" fence and drop
    // leading blank lines. CRLF-safe: trims both \r and \n.
    let front = &rest[..end + 1];
    let body = rest[end + 4..].trim_start_matches(['\n', '\r']);
    Some((front, body))
}

/// Split the frontmatter fence off `text` and deserialize the YAML block into `T` — the common
/// "read a fenced doc's frontmatter" shape shared by the daily layer and its queues. Errors
/// uniformly: a missing fence or a YAML parse failure both surface as [`StoreError::Parse`].
pub(crate) fn parse_frontmatter<T: serde::de::DeserializeOwned>(
    text: &str,
) -> Result<T, StoreError> {
    let (block, _) = split_frontmatter(text)
        .ok_or_else(|| StoreError::Parse("missing frontmatter fence".into()))?;
    serde_yaml::from_str(block).map_err(|e| StoreError::Parse(e.to_string()))
}

/// Parse `## Todos`/`## Blockers`/… markdown bullets into frontmatter item lists.
/// md-only notes have no ids/dates, so ids are synthesized (`t1`, `t2`, …) and the
/// item `created` is set to the note's `created` date.
fn parse_md_sections_into(front: &mut Frontmatter, body: &str) {
    let created = front.created.clone();
    for section in Section::all() {
        let bullets = collect_section_bullets(body, section.heading());
        let prefix = section.id_prefix();
        let mut items = Vec::new();
        for (n, (done, text)) in bullets.into_iter().enumerate() {
            let mut item = Item {
                id: format!("{prefix}{}", n + 1),
                text,
                created: created.clone(),
                done: None,
                done_at: None,
                link: None,
                pct: None,
                est: None,
                actual: None,
                q: None,
                notes: Vec::new(),
            };
            if section.tracks_done() {
                item.done = Some(done);
            }
            items.push(item);
        }
        *front.items_mut(section) = items;
    }
}

/// The checklist bullets under `section`'s heading in a free markdown body, as `(done, text)`.
///
/// The same parser the legacy-note migration uses, exposed so anything shaped like a note body —
/// a task context doc (#997), for instance — reads its `## Done When` through *this* rather than
/// growing a second, subtly-different checklist parser. Template placeholders (`-`, `- [ ]`) are
/// skipped, so an untouched template yields nothing, which is what makes the SMART gate's
/// "measurable" check honest.
pub fn section_bullets(body: &str, section: Section) -> Vec<(bool, String)> {
    collect_section_bullets(body, section.heading())
}

/// Collect bullet lines under a heading until the next `## ` / `---` / EOF.
/// Returns (done, text) per bullet; `done` is meaningful only for todo checkboxes.
/// Skips the template placeholder lines (`-`, `- [ ]`, `- [ ] test`).
fn collect_section_bullets(body: &str, heading: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut in_section = false;
    for (line, code) in crate::markdown::scan(body) {
        if !in_section {
            if !code && line == heading {
                in_section = true;
            }
            continue;
        }
        if !code && (line.starts_with("## ") || line == "---") {
            break;
        }
        // Bullets quoted inside a fence are sample text, not items of this section.
        if code {
            continue;
        }
        let trimmed = line.trim_end();
        if trimmed == "-" || trimmed == "- [ ]" || trimmed == "- [ ] test" {
            continue; // placeholder seed
        }
        if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            out.push((false, rest.to_string()));
        } else if let Some(rest) = trimmed
            .strip_prefix("- [x] ")
            .or_else(|| trimmed.strip_prefix("- [X] "))
        {
            // Markdown task lists accept both `x` and `X` as the checked marker.
            out.push((true, rest.to_string()));
        } else if let Some(rest) = trimmed.strip_prefix("- ") {
            out.push((false, rest.to_string()));
        }
    }
    out
}

/// Remove the six item-list markdown sections from a body, leaving prose
/// (Goal/Progress, etc.). Used when migrating legacy notes to single-file in memory.
fn strip_item_sections(body: &str) -> String {
    let headings: Vec<&str> = Section::all().map(Section::heading).collect();
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for (line, code) in crate::markdown::scan(body) {
        if !code && line.starts_with("## ") {
            skipping = headings.contains(&line);
            if skipping {
                continue;
            }
        }
        if skipping {
            // stop skipping at the next non-item H2 or a fence
            if !code && line == "---" {
                skipping = false;
                out.push(line);
            }
            continue;
        }
        out.push(line);
    }
    // normalize trailing whitespace
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, Note, Scalar};
    use crate::resolver::RepoBranch;

    struct SerializationFailure;

    impl serde::Serialize for SerializationFailure {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom(
                "intentional serialization failure",
            ))
        }
    }

    #[test]
    fn save_json_atomic_preserves_existing_file_on_serialization_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "existing state").unwrap();

        let err = save_json_atomic(&path, &SerializationFailure).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::Other);
        assert!(err
            .to_string()
            .contains("intentional serialization failure"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "existing state");
    }

    /// Atomic write never leaves a torn file: a reader hammering the note while writers replace it
    /// always parses a complete note, and no `.tmp` scratch files survive.
    #[test]
    fn atomic_write_is_never_observed_torn() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("note.md");
        save(
            &Note::new(RepoBranch::new("demo", "main"), "2026-07-02", "h"),
            &md,
        )
        .unwrap();

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (md, stop) = (md.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    // Every read must succeed — a torn (half-written) file would fail to parse.
                    read(&md).expect("reader never sees a torn note");
                }
            })
        };
        for i in 0..300 {
            let mut n = Note::new(RepoBranch::new("demo", "main"), "2026-07-02", "h");
            n.front.todos.push(Item {
                id: format!("t{i}"),
                text: format!("todo {i}"),
                created: "2026-07-02".into(),
                done: None,
                done_at: None,
                link: None,
                pct: None,
                est: None,
                actual: None,
                q: None,
                notes: Vec::new(),
            });
            save(&n, &md).unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().unwrap();

        // No scratch temp files left behind in the note dir.
        let leftover: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "temp files leaked: {leftover:?}");
    }

    /// The advisory lock serializes concurrent read-modify-write so no update is lost: N threads
    /// each append one todo; all N must survive.
    #[test]
    fn note_lock_prevents_lost_updates() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("note.md");
        save(
            &Note::new(RepoBranch::new("demo", "main"), "2026-07-02", "h"),
            &md,
        )
        .unwrap();

        let n_threads = 12;
        let handles: Vec<_> = (0..n_threads)
            .map(|i| {
                let md = md.clone();
                std::thread::spawn(move || {
                    let _lock = NoteLock::acquire(&md);
                    let mut note = read(&md).unwrap();
                    note.front.todos.push(Item {
                        id: format!("t{i}"),
                        text: format!("todo {i}"),
                        created: "2026-07-02".into(),
                        done: None,
                        done_at: None,
                        link: None,
                        pct: None,
                        est: None,
                        actual: None,
                        q: None,
                        notes: Vec::new(),
                    });
                    save(&note, &md).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let note = read(&md).unwrap();
        assert_eq!(
            note.front.todos.len(),
            n_threads,
            "a concurrent update was lost"
        );
        // The lock file is cleaned up when the guards drop.
        assert!(!lock_path(&md).exists(), "lock file leaked");
    }

    #[test]
    fn required_lock_rejects_active_contention() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("backlog.jsonl");
        let owner = FileLock::acquire_required(&target).unwrap();

        let err = match FileLock::acquire_required_with_wait(
            &target,
            std::time::Duration::from_millis(0),
        ) {
            Ok(_) => panic!("a required lock must not proceed without ownership"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        assert!(err.to_string().contains("timed out"));
        assert!(
            lock_path(&target).exists(),
            "the owner still holds the lock"
        );

        drop(owner);
        assert!(!lock_path(&target).exists(), "the owned lock is released");
    }

    /// A fresh lock is not stale; a lock file doesn't exist → not stale (nothing to steal).
    #[test]
    fn fresh_lock_is_not_stale() {
        let dir = tempfile::tempdir().unwrap();
        let lp = lock_path(&dir.path().join("note.md"));
        assert!(!lock_is_stale(&lp), "absent lock is not stale");
        std::fs::write(&lp, b"").unwrap();
        assert!(!lock_is_stale(&lp), "just-created lock is not stale");
    }

    #[test]
    fn split_frontmatter_skips_dash_runs_inside_values() {
        // A multi-line value contains a `---x` line; only the real fence (`\n---\n`) closes
        // the block. A naive first-`\n---` search would truncate the frontmatter here.
        let text = "---\nrepo: demo\nnote: \"a\n---x\nb\"\n---\nbody line\n";
        let (front, body) = split_frontmatter(text).expect("fence found");
        assert!(front.contains("repo: demo"));
        assert!(front.contains("note:"));
        assert_eq!(body, "body line\n");
    }

    #[test]
    fn split_frontmatter_handles_trailing_fence_at_eof() {
        let text = "---\nrepo: demo\n---";
        let (front, body) = split_frontmatter(text).expect("fence found");
        assert_eq!(front, "repo: demo\n");
        assert_eq!(body, "");
    }

    #[test]
    fn split_frontmatter_accepts_crlf_fences() {
        // A note.md saved with Windows line endings opens with `---\r\n` and closes with
        // `---\r\n`. Matching only the LF fence rejected it as missing frontmatter, dropping the
        // whole structured block. Both fences and the body trim must be CRLF-safe.
        let text = "---\r\nrepo: demo\r\nbranch: main\r\n---\r\n\r\nbody line\r\n";
        let (front, body) = split_frontmatter(text).expect("fence found");
        assert!(front.contains("repo: demo"));
        assert!(front.contains("branch: main"));
        // Leading blank lines (CR and LF) are trimmed; the body keeps its own CRLF endings.
        assert_eq!(body, "body line\r\n");
        // serde_yaml parses the CRLF frontmatter block unchanged.
        let parsed: serde_yaml::Value = serde_yaml::from_str(front).expect("valid yaml");
        assert_eq!(parsed["repo"], serde_yaml::Value::from("demo"));
    }

    #[test]
    fn split_frontmatter_handles_crlf_fence_at_eof() {
        let text = "---\r\nrepo: demo\r\n---";
        let (front, body) = split_frontmatter(text).expect("fence found");
        assert_eq!(front, "repo: demo\r\n");
        assert_eq!(body, "");
    }

    fn sample_note() -> Note {
        let front = Frontmatter {
            repo: "demo".into(),
            branch: "main".into(),
            created: "2026-06-19".into(),
            status: "active".into(),
            stage: None,
            machine: "testhost".into(),
            type_: None,
            work_item: None,
            pr: None,
            tags: vec![],
            collaborators: vec![],
            harnesses: vec![],
            models: vec![],
            todos: vec![
                Item {
                    id: "t1".into(),
                    text: "first".into(),
                    created: "2026-06-19".into(),
                    done: Some(false),
                    done_at: None,
                    link: None,
                    pct: None,
                    est: None,
                    actual: None,
                    q: None,
                    notes: Vec::new(),
                },
                Item {
                    id: "t2".into(),
                    text: "second".into(),
                    created: "2026-06-19".into(),
                    done: Some(true),
                    done_at: Some("2026-06-20".into()),
                    link: None,
                    pct: None,
                    est: None,
                    actual: None,
                    q: None,
                    notes: Vec::new(),
                },
            ],
            blockers: vec![],
            decisions: vec![Item {
                id: "d1".into(),
                text: "use serde".into(),
                created: "2026-06-19".into(),
                done: None,
                done_at: None,
                link: None,
                pct: None,
                est: None,
                actual: None,
                q: None,
                notes: Vec::new(),
            }],
            research: vec![],
            collab: vec![],
            ask: vec![],
            done_when: vec![],
        };
        Note {
            front,
            body: "## Goal\n\nShip it.\n\n## Progress\n\nStarted.".into(),
        }
    }

    #[test]
    fn single_file_round_trip() {
        let note = sample_note();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        save(&note, &path).unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back.front, note.front);
        assert_eq!(back.body, note.body);
        // save is idempotent
        let first = std::fs::read_to_string(&path).unwrap();
        save(&back, &path).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn reads_split_layout_from_sibling_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("note.md");
        let yaml = dir.path().join("note.yaml");
        std::fs::write(&md, "---\nrepo: demo\nbranch: main\ncreated: 2026-04-17\nstatus: active\nmachine: host\n---\n\n## Goal\n\nprose\n").unwrap();
        std::fs::write(&yaml, "repo: demo\nbranch: main\ncreated: 2026-04-17\nstatus: active\nmachine: host\ntype: null\nwork_item: null\npr: null\ntags: []\ncollaborators: []\ntodos:\n  - id: t1\n    text: legacy todo\n    done: true\n    added: \"2026-04-17\"\nblockers: []\ndecisions:\n  - id: d1\n    text: a decision\n    added: \"2026-04-17\"\nresearch: []\ncollab: []\nask: []\n").unwrap();
        let note = read(&md).unwrap();
        assert_eq!(note.front.todos.len(), 1);
        assert_eq!(note.front.todos[0].id, "t1");
        assert_eq!(note.front.todos[0].done, Some(true));
        // legacy `added` mapped onto `created`
        assert_eq!(note.front.todos[0].created, "2026-04-17");
        assert_eq!(note.front.decisions[0].text, "a decision");
        assert!(note.body.contains("prose"));
    }

    #[test]
    fn reads_md_only_legacy_sections() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("note.md");
        std::fs::write(&md, "---\nrepo: demo\nbranch: main\ncreated: 2026-06-19\nstatus: active\nmachine: host\ntype:\nwork_item:\npr:\ntags: []\ncollaborators: []\n---\n\n## Goal\n\nthe goal\n\n## Todos\n- [ ] open one\n- [x] done one\n\n## Blockers\n-\n\n## Decisions\n- chose rust\n\n## To Research\n-\n\n## Collaboration\n-\n\n## To Ask\n-\n").unwrap();
        let note = read(&md).unwrap();
        assert_eq!(note.front.todos.len(), 2);
        assert_eq!(note.front.todos[0].id, "t1");
        assert_eq!(note.front.todos[0].done, Some(false));
        assert_eq!(note.front.todos[1].done, Some(true));
        assert_eq!(note.front.decisions.len(), 1);
        assert_eq!(note.front.decisions[0].id, "d1");
        assert_eq!(note.front.decisions[0].text, "chose rust");
        // placeholders dropped
        assert!(note.front.blockers.is_empty());
        // item sections stripped from body, Goal kept
        assert!(note.body.contains("the goal"));
        assert!(!note.body.contains("## Todos"));
        // empty type/work_item/pr → None
        assert_eq!(note.front.type_, None);
    }

    /// Regression: an uppercase `- [X]` checkbox must parse as a *done* item with the
    /// bracket marker stripped — not fall through to the plain-dash arm and become an open
    /// todo whose text is the literal `[X] done`. Markdown task lists accept `x` and `X`.
    #[test]
    fn uppercase_checkbox_parses_as_done() {
        let body = "## Todos\n- [X] done\n- [x] lower\n- [ ] open\n";
        let bullets = collect_section_bullets(body, "## Todos");
        assert_eq!(
            bullets,
            vec![
                (true, "done".to_string()),
                (true, "lower".to_string()),
                (false, "open".to_string()),
            ]
        );
    }

    #[test]
    fn reads_note_with_crlf_line_endings() {
        // Regression: a note.md saved with CRLF endings opens with `---\r\n`. The parser used to
        // match only the LF fence, reject the note as "missing frontmatter fence", and drop the
        // entire structured frontmatter. A CRLF note must read the same frontmatter and body as
        // its LF twin.
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("note.md");
        let lf = "---\nrepo: demo\nbranch: main\ncreated: 2026-06-19\nstatus: active\nmachine: host\ntype:\nwork_item:\npr:\ntags: []\ncollaborators: []\n---\n\n## Goal\n\nthe goal\n\n## Todos\n- [ ] open one\n- [x] done one\n\n## Blockers\n-\n";
        std::fs::write(&md, lf.replace('\n', "\r\n")).unwrap();

        let note = read(&md).expect("CRLF note parses (not 'missing frontmatter fence')");
        // Structured frontmatter survived the CRLF opening/closing fences.
        assert_eq!(note.front.repo, "demo");
        assert_eq!(note.front.branch, "main");
        assert_eq!(note.front.created, "2026-06-19");
        assert_eq!(note.front.status, "active");
        assert_eq!(note.front.machine, "host");
        // Body item sections parsed through CRLF lines.
        assert_eq!(note.front.todos.len(), 2);
        assert_eq!(note.front.todos[0].done, Some(false));
        assert_eq!(note.front.todos[1].done, Some(true));
        // Prose body preserved; item sections stripped.
        assert!(note.body.contains("the goal"));
        assert!(!note.body.contains("## Todos"));
    }

    #[test]
    fn line_map_locates_item_ids() {
        let note = sample_note();
        let text = render(&note).unwrap();
        let map = item_line_map(&text);
        assert!(map.contains_key("t1"));
        assert!(map.contains_key("t2"));
        assert!(map.contains_key("d1"));
        // ids resolve to distinct lines inside the frontmatter
        assert_ne!(map["t1"], map["t2"]);
    }

    #[test]
    fn machine_null_reads_as_empty_so_note_is_local() {
        // Regression: legacy notes write `machine: null`; the oracle treats that as
        // "no machine" (local). It must not be read as a non-empty "null" string that
        // would wrongly fail the local filter and hide the note from rollups.
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("note.md");
        let yaml = dir.path().join("note.yaml");
        std::fs::write(&md, "# demo / main\n\n## Goal\n\nx\n").unwrap();
        std::fs::write(
            &yaml,
            "repo: demo\nbranch: main\ncreated: 2026-02-15\nstatus: active\nmachine: null\ntags: []\ncollaborators: []\ntodos:\n  - id: t1\n    text: hidden todo\n    done: false\n    added: \"2026-02-15\"\nblockers: []\ndecisions: []\nresearch: []\ncollab: []\nask: []\n",
        )
        .unwrap();
        let note = read(&md).unwrap();
        assert_eq!(note.front.machine, "");
    }

    #[test]
    fn cat_renders_open_todos_and_placeholders() {
        let note = sample_note();
        let out = crate::render::cat(&note);
        // open todo shown, done todo hidden
        assert!(out.contains("- [ ] first"));
        assert!(!out.contains("second"));
        // decision shown; empty sections get placeholders
        assert!(out.contains("- use serde"));
        assert!(out.contains("## Blockers\n-\n"));
        assert!(out.contains("## To Ask\n-\n"));
        // section order: Blockers before Todos before Decisions
        let b = out.find("## Blockers").unwrap();
        let t = out.find("## Todos").unwrap();
        let d = out.find("## Decisions").unwrap();
        assert!(b < t && t < d);
    }

    #[test]
    fn numeric_work_item_round_trips() {
        let mut note = sample_note();
        note.front.work_item = Some(Scalar::Int(12345));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        save(&note, &path).unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back.front.work_item, Some(Scalar::Int(12345)));
    }
}
