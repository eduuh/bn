//! The task store behind a seam: where bn's one person-scoped task list actually lives.
//!
//! Every reader and writer of the list used to reach for `Config::tasks_file` and the
//! `todo::{load, save, lock, append_task, claim}` functions directly, which welded "the task list"
//! to "a `tasks.txt` on this disk". [`TaskStore`] is the shape those call sites already had —
//! lock, load, mutate the in-memory [`TodoFile`], save — named once, so a second backend can sit
//! behind the same calls. Modelled on [`crate::agent::transport`]: one trait, one factory
//! ([`task_store`]), and the first implementation ([`FileTaskStore`]) doing exactly what the call
//! sites did inline.
//!
//! What stays file-only lives inside [`FileTaskStore`], not at the call sites: creating the store's
//! directory, provisioning the `merge=union` attribute ([`todo::ensure_union_merge`]) when a write
//! creates the file, and the heal-before-append rule for a union-merged file. The repair passes
//! ([`TodoFile::dedupe_by_uid`], [`TodoFile::heal_ids`]) were already inside [`todo::load`].
//!
//! `[tasks] provider` picks the implementation ([`TaskProviderKind`]). The Forge provider is
//! [`crate::forge_app::store::ForgeTaskStore`]. Without a Forge key it still builds, and every read
//! and write fails pointing at `bn forge login`. It never falls back to the file — a person who
//! asked for Forge and got a quietly diverging local `tasks.txt` would have two task lists and no
//! way to tell.

use std::any::Any;
use std::io;
use std::path::{Path, PathBuf};

use crate::resolver::Config;
use crate::todo::{self, Task, TodoFile};

/// Which backend holds the task list: `[tasks] provider`.
///
/// `ForgeApp`, not `Forge`: [`crate::forge`] is the unrelated git-host (GitHub / Azure DevOps)
/// abstraction, and a second `Forge` in the same crate would be a standing misreading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskProviderKind {
    /// `<notes_root>/tasks.txt`, synced with the notes repo. The default.
    #[default]
    File,
    /// The Forge app's MCP endpoint (forge.wira360.com): [`crate::forge_app`].
    ForgeApp,
}

impl TaskProviderKind {
    /// The config word. An absent or blank key is `File`; an unrecognised word is an error rather
    /// than a silent default, so the resolver can say so.
    pub fn parse(word: Option<&str>) -> Result<TaskProviderKind, String> {
        match word.map(|w| w.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("file") => Ok(TaskProviderKind::File),
            Some("forge") => Ok(TaskProviderKind::ForgeApp),
            Some(other) => Err(format!(
                "[tasks] provider = \"{other}\" is not a task provider (want \"file\" or \"forge\")"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TaskProviderKind::File => "file",
            TaskProviderKind::ForgeApp => "forge",
        }
    }
}

/// One backend for the task list.
///
/// The contract is the read-modify-write the call sites already follow: take [`lock`](Self::lock),
/// [`load`](Self::load), change the [`TodoFile`], [`save`](Self::save) it, drop the guard. A remote
/// store is free to make `save` a diff against what `load` returned.
pub trait TaskStore: Send + Sync {
    /// The whole list. A store that does not exist yet is an empty list, not an error.
    fn load(&self) -> io::Result<TodoFile>;

    /// Persist `file` as the new state of the list. Call under [`lock`](Self::lock).
    fn save(&self, file: &TodoFile) -> io::Result<()>;

    /// Add one already-built task (its id minted from a [`load`](Self::load) under the same lock)
    /// without rewriting the rest of the list where the backend can avoid it.
    fn append(&self, task: &Task) -> io::Result<()>;

    /// A guard serialising this process's load→save span against every other writer, released on
    /// drop. Hold it for the whole span, not just the write. May be inert.
    fn lock(&self) -> Box<dyn Any>;

    /// Bind task `id` to the worktree working it (`repo:` / `branch:`). Takes its own lock, so do
    /// not call it while holding [`lock`](Self::lock). `Ok(false)` for an unknown id.
    fn claim(&self, id: &str, repo: &str, branch: &str) -> io::Result<bool>;

    /// Leave a progress note on task `id` where the backend has somewhere to put one. The file
    /// store has nowhere, so it is a no-op there.
    fn note(&self, id: &str, text: &str) -> io::Result<()>;

    /// Which provider this is.
    fn kind(&self) -> TaskProviderKind;
}

/// The task store `cfg` selects.
pub fn task_store(cfg: &Config) -> Box<dyn TaskStore> {
    match cfg.tasks_provider {
        TaskProviderKind::File => Box::new(FileTaskStore::new(cfg.tasks_file())),
        TaskProviderKind::ForgeApp => {
            Box::new(crate::forge_app::store::ForgeTaskStore::from_config(cfg))
        }
    }
}

/// [`TaskStore::note`], best-effort: a failure is logged and swallowed, because a progress note is
/// a report on the work, never a step of it. A pipeline stage must not fail because Forge was
/// unreachable when it tried to say so.
pub fn note_best_effort(store: &dyn TaskStore, id: &str, text: &str) {
    if let Err(e) = store.note(id, text) {
        eprintln!("bn: could not leave a note on task {id}: {e}");
        crate::log::record(
            crate::log::LogLevel::Warn,
            "tasks",
            "note_failed",
            serde_json::json!({ "task": id, "provider": store.kind().label() }),
            serde_json::json!({ "error": e.to_string() }),
            None,
        );
    }
}

/// Leave `text` on the task governing `(repo, branch)` ([`crate::pipeline::gate::governing_in`]),
/// best-effort. Only a store with somewhere to put a note does any work: with the file provider
/// this returns before loading anything, so file-mode behaviour is exactly what it was.
pub fn note_governing(cfg: &Config, repo: &str, branch: &str, text: &str) {
    if cfg.tasks_provider == TaskProviderKind::File {
        return;
    }
    let store = task_store(cfg);
    let file = match store.load() {
        Ok(file) => file,
        Err(e) => {
            eprintln!("bn: could not load tasks to note {repo}:{branch}: {e}");
            return;
        }
    };
    if let Some(id) = crate::pipeline::gate::governing_in(&file, repo, branch).and_then(Task::id) {
        note_best_effort(store.as_ref(), id, text);
    }
}

/// The task list as a todo.txt-grammar file: `<notes_root>/tasks.txt` in production.
#[derive(Debug, Clone)]
pub struct FileTaskStore {
    path: PathBuf,
}

impl FileTaskStore {
    pub fn new(path: impl Into<PathBuf>) -> FileTaskStore {
        FileTaskStore { path: path.into() }
    }

    /// The file this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run a write that may create the file: make its directory first, and give a file it just
    /// created the `merge=union` attribute two machines' appends rely on (#995). Both are no-ops
    /// once the store exists, which is every write after the first.
    fn creating(&self, write: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        let is_new = !self.path.exists();
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        write()?;
        if is_new {
            if let Some(root) = self.path.parent() {
                // Silent on failure, as it always was: a missing merge driver costs conflicts,
                // never a task.
                let _ = todo::ensure_union_merge(root);
            }
        }
        Ok(())
    }
}

impl TaskStore for FileTaskStore {
    fn load(&self) -> io::Result<TodoFile> {
        todo::load(&self.path)
    }

    fn save(&self, file: &TodoFile) -> io::Result<()> {
        self.creating(|| todo::save(&self.path, file))
    }

    /// Append one line when the file is already in canonical form, so a concurrent add
    /// concatenates rather than clobbering and no existing line is reflowed. When [`todo::load`] had
    /// to heal the file (duplicate or missing ids after a union merge), persist the healed
    /// numbering with the new task instead — the ids a person just saw in a listing stay stable
    /// rather than being re-derived, and shifted, on the next read.
    fn append(&self, task: &Task) -> io::Result<()> {
        let raw = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut file = todo::load(&self.path)?;
        if raw == file.render() {
            self.creating(|| todo::append_task(&self.path, task))
        } else {
            file.push_task(task.clone());
            self.save(&file)
        }
    }

    fn lock(&self) -> Box<dyn Any> {
        Box::new(todo::lock(&self.path))
    }

    fn claim(&self, id: &str, repo: &str, branch: &str) -> io::Result<bool> {
        todo::claim(&self.path, id, repo, branch)
    }

    fn note(&self, _id: &str, _text: &str) -> io::Result<()> {
        Ok(())
    }

    fn kind(&self) -> TaskProviderKind {
        TaskProviderKind::File
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, text: &str) -> Task {
        Task::new_todo(id, text, None, "", "test", "2026-09-14")
    }

    #[test]
    fn provider_parses_file_by_default_and_forge_by_name() {
        assert_eq!(TaskProviderKind::parse(None), Ok(TaskProviderKind::File));
        assert_eq!(
            TaskProviderKind::parse(Some("")),
            Ok(TaskProviderKind::File)
        );
        assert_eq!(
            TaskProviderKind::parse(Some("file")),
            Ok(TaskProviderKind::File)
        );
        assert_eq!(
            TaskProviderKind::parse(Some(" Forge ")),
            Ok(TaskProviderKind::ForgeApp)
        );
    }

    #[test]
    fn an_unknown_provider_is_an_error_not_the_file() {
        let err = TaskProviderKind::parse(Some("froge")).unwrap_err();
        assert!(err.contains("froge"), "{err}");
    }

    #[test]
    fn file_store_round_trips_and_provisions_a_new_store() {
        let root = tempfile::tempdir().expect("tempdir");
        let store = FileTaskStore::new(root.path().join("notes").join("tasks.txt"));
        assert!(store.load().expect("empty load").tasks().next().is_none());

        store.append(&task("t1", "first")).expect("append");
        let mut file = store.load().expect("load");
        let id = file.next_id();
        file.push_task(task(&id, "second"));
        {
            let _guard = store.lock();
            store.save(&file).expect("save");
        }
        assert!(store.claim(&id, "BN", "feat/x").expect("claim"));
        assert!(!store.claim("t99", "bn", "main").expect("claim unknown"));
        store.note(&id, "ignored").expect("note is a no-op");

        let back = store.load().expect("reload");
        let summaries: Vec<String> = back.tasks().map(|t| t.summary()).collect();
        assert_eq!(summaries, ["first", "second"]);
        let claimed = back.tasks().find(|t| t.id() == Some(id.as_str())).unwrap();
        assert_eq!(claimed.meta("repo"), Some("bn"));
        assert_eq!(claimed.meta("branch"), Some("feat/x"));

        let attrs = std::fs::read_to_string(root.path().join("notes").join(".gitattributes"))
            .expect("union merge provisioned");
        assert!(attrs.contains("tasks.txt merge=union"));
    }

    #[test]
    fn append_heals_a_file_with_colliding_ids_before_adding() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("tasks.txt");
        std::fs::write(&path, "one id:t1\ntwo id:t1\n").expect("seed");
        let store = FileTaskStore::new(&path);

        let mut file = store.load().expect("load");
        let id = file.next_id();
        let three = task(&id, "three");
        store.append(&three).expect("append");
        file.push_task(three);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), file.render());
    }

    #[test]
    fn the_factory_picks_the_configured_provider() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut cfg = Config {
            personal_root: root.path().to_path_buf(),
            work_root: root.path().to_path_buf(),
            ..Config::default()
        };
        assert_eq!(task_store(&cfg).kind(), TaskProviderKind::File);

        // No key anywhere: the store still builds, and every call says how to get one.
        let _env = crate::test_support::env_lock();
        let _token = crate::test_support::EnvGuard::remove(crate::forge_app::TOKEN_ENV);
        let _cfg_dir = crate::test_support::EnvGuard::set("BN_CONFIG_DIR", root.path());
        cfg.tasks_provider = TaskProviderKind::ForgeApp;
        let store = task_store(&cfg);
        assert_eq!(store.kind(), TaskProviderKind::ForgeApp);
        let err = store.load().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("bn forge login"), "{err}");
        assert!(store.append(&task("t1", "x")).is_err());
        assert!(store.save(&TodoFile::default()).is_err());
        assert!(store.claim("t1", "bn", "main").is_err());
        assert!(
            !cfg.tasks_file().exists(),
            "forge must never touch tasks.txt"
        );
    }
}
