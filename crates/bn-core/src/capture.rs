//! `bn shot` screenshot / GIF capture: move fresh captures from the configured source
//! folder into the branch note's `screenshots/` (or `recordings/`) dir and embed a
//! markdown link for each. Side-effecting file capture with its own `GrabError` — kept
//! out of the note-command façade (`commands`).

use crate::model::Note;
use crate::resolver::{Config, Resolved};
use crate::store;

/// What `bn shot` grabs: a screenshot, or a GIF recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    Screenshot,
    Gif,
}

impl Capture {
    fn ext(self) -> &'static str {
        match self {
            Capture::Screenshot => "png",
            Capture::Gif => "gif",
        }
    }
    /// Sub-directory under the note dir where grabs land.
    fn subdir(self) -> &'static str {
        match self {
            Capture::Screenshot => "screenshots",
            Capture::Gif => "recordings",
        }
    }
    /// Body heading the markdown links are appended under.
    fn heading(self) -> &'static str {
        match self {
            Capture::Screenshot => "## Screenshots",
            Capture::Gif => "## Recordings",
        }
    }
    /// Human label for messages and the JSON envelope.
    pub fn label(self) -> &'static str {
        match self {
            Capture::Screenshot => "screenshot",
            Capture::Gif => "gif",
        }
    }
    /// The configured source folder for this kind, or `None` (with the config key to set).
    fn source(self, cfg: &Config) -> Result<&std::path::Path, &'static str> {
        match self {
            Capture::Screenshot => cfg
                .screenshot_dir
                .as_deref()
                .ok_or("[capture].screenshot_dir"),
            Capture::Gif => cfg.gif_dir.as_deref().ok_or("[capture].gif_dir"),
        }
    }
}

/// Result of a successful `bn shot`: the kind, the note-relative paths that landed, and the
/// heading they were filed under.
#[derive(Debug)]
pub struct GrabOutcome {
    pub kind: &'static str,
    pub moved: Vec<String>,
    pub heading: String,
}

/// Why `bn shot` couldn't grab anything.
#[derive(Debug)]
pub enum GrabError {
    /// The source folder isn't configured; payload is the config key to set.
    Unconfigured(&'static str),
    /// The configured source folder doesn't exist.
    SourceMissing(std::path::PathBuf),
    /// Nothing in the source folder is newer than the freshness window.
    NothingFresh(u64, std::path::PathBuf),
    Io(String),
}

impl GrabError {
    pub fn message(&self) -> String {
        match self {
            GrabError::Unconfigured(key) => format!(
                "bn shot: no capture folder configured — set {key} in ~/.config/bn/config.toml"
            ),
            GrabError::SourceMissing(p) => {
                format!("bn shot: source folder not found: {}", p.display())
            }
            GrabError::NothingFresh(secs, p) => format!(
                "bn shot: nothing captured in the last {secs}s in {}\n  capture first (e.g. Win+PrtScn / ScreenToGif), then re-run.",
                p.display()
            ),
            GrabError::Io(e) => format!("bn shot: {e}"),
        }
    }
    /// Exit code: "nothing fresh" is a distinct, expected outcome (2); the rest are errors (1).
    pub fn exit_code(&self) -> i32 {
        match self {
            GrabError::NothingFresh(..) => 2,
            _ => 1,
        }
    }
}

/// Move the capture(s) newer than `fresh_secs` from the configured source folder into the
/// branch note's `screenshots/` (or `recordings/`) dir, then append a markdown image link for
/// each under the matching body heading. Files are taken oldest-first so links land in capture
/// order; a batch captured within the same second is disambiguated with a `-N` suffix. The
/// move falls back to copy+delete across filesystems (e.g. `/mnt/c` → ext4 on WSL).
pub fn grab(
    cfg: &Config,
    ctx: &Resolved,
    kind: Capture,
    fresh_secs: u64,
    today: &str,
) -> Result<GrabOutcome, GrabError> {
    let src = kind.source(cfg).map_err(GrabError::Unconfigured)?;
    if !src.is_dir() {
        return Err(GrabError::SourceMissing(src.to_path_buf()));
    }

    let threshold =
        std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(fresh_secs));
    let ext = kind.ext();
    let mut fresh: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    let rd = std::fs::read_dir(src).map_err(|e| GrabError::Io(e.to_string()))?;
    for entry in rd.flatten() {
        let path = entry.path();
        let matches_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(ext));
        if !matches_ext || !path.is_file() {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if threshold.is_some_and(|th| mtime < th) {
            continue;
        }
        fresh.push((mtime, path));
    }
    if fresh.is_empty() {
        return Err(GrabError::NothingFresh(fresh_secs, src.to_path_buf()));
    }
    fresh.sort_by_key(|(t, _)| *t);

    let dest_dir = cfg.note_dir(ctx).join(kind.subdir());
    std::fs::create_dir_all(&dest_dir).map_err(|e| GrabError::Io(e.to_string()))?;

    let stamp = crate::util::now_filestamp();
    let batch = fresh.len() > 1;
    let mut moved = Vec::new();
    for (i, (_, srcfile)) in fresh.iter().enumerate() {
        let name = if batch {
            format!("{stamp}-{}.{ext}", i + 1)
        } else {
            format!("{stamp}.{ext}")
        };
        let out = dest_dir.join(&name);
        move_file(srcfile, &out).map_err(GrabError::Io)?;
        moved.push(format!("{}/{}", kind.subdir(), name));
    }

    // Append a link per grab under the heading, reusing the note machinery so the section is
    // created/extended exactly like `bn notes` — and survives bn's round-trip verbatim.
    let note_md = cfg.note_file(ctx);
    // Lock the read-modify-write (see set_goal) so a parallel writer can't lose this update.
    let _lock = store::NoteLock::acquire(&note_md);
    let mut note = if note_md.exists() {
        store::read(&note_md).map_err(|e| GrabError::Io(e.to_string()))?
    } else {
        Note::new(ctx.rb(), today, &cfg.hostname)
    };
    for rel in &moved {
        note.body =
            crate::render::append_to_section(&note.body, kind.heading(), &format!("![]({rel})"));
    }
    note.front.record_session();
    store::save(&note, &note_md).map_err(|e| GrabError::Io(e.to_string()))?;

    Ok(GrabOutcome {
        kind: kind.label(),
        moved,
        heading: kind.heading().to_string(),
    })
}

/// Move a file, falling back to copy+delete when `rename` fails because src and dst are on
/// different filesystems (the WSL `/mnt/c` → Linux case — `rename` returns `EXDEV` there).
fn move_file(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dst)
        .map_err(|e| format!("copy {} → {}: {e}", src.display(), dst.display()))?;
    std::fs::remove_file(src).map_err(|e| format!("remove {}: {e}", src.display()))?;
    Ok(())
}
