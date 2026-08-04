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

/// A temp sibling of `path` no other writer can collide with: pid (cross-process) plus a
/// process-local counter (cross-thread). A *fixed* temp name is the subtle bug this avoids — two
/// writers would share one scratch file and each could rename the other's half-written bytes into
/// place. Shared with the task store, which needs the same guarantee.
pub(crate) fn atomic_temp_path(path: &Path) -> PathBuf {
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


