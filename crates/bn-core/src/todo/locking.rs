use std::path::Path;

/// An advisory exclusive lock over a file's read-modify-write, released on drop.
/// Held for the whole load-mutate-save span, including atomic replacements.
pub struct TodoLock(Option<std::fs::File>);

impl Drop for TodoLock {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            let _ = file.unlock();
        }
    }
}

/// Acquire a best-effort lock, proceeding unlocked if the state directory is unavailable.
/// State-directory locks survive atomic replacements and never enter the synced document store.
pub fn lock(path: &Path) -> TodoLock {
    TodoLock(acquire_lock(path).ok())
}

/// Acquire the same advisory lock, but never let a durable document edit proceed unlocked.
pub fn lock_required(path: &Path) -> std::io::Result<TodoLock> {
    acquire_lock(path).map(|file| TodoLock(Some(file)))
}

fn acquire_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let dir = crate::resolver::state_dir().join("locks");
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::File::create(dir.join(format!("todo-{}.lock", lock_key(path))))?;
    file.lock()?;
    Ok(file)
}

/// Canonical parent paths make symlinked and literal spellings contend on one lock.
fn lock_key(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let canon = path
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .map(|parent| parent.join(path.file_name().unwrap_or_default()))
        .unwrap_or_else(|| path.to_path_buf());
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    canon.hash(&mut hash);
    format!("{:016x}", hash.finish())
}
