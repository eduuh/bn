//! [`ForgeTaskStore`]: bn's task list read from, and written back to, Forge activities.
//!
//! **Load** calls `list_bn_tasks` and maps each row to a task ([`super::mapping::task_from_row`]).
//! bn ids must stay stable across loads, so an activity with no usable `id` in its `agentMeta`
//! (none, malformed, or a duplicate of an earlier activity's) gets the next free `t<N>` and the id
//! is written back straight away with `set_activity_meta`. Rows are ordered by creation time, so
//! which of two colliding activities keeps the id is deterministic on every machine. The load also
//! keeps a snapshot: for each activity, its version and the [`TaskState`] bn saw.
//!
//! **Save** compares each task with that snapshot and sends only what changed, meta first because
//! it is the one guarded write: `set_activity_meta` carries `expectedVersion`, and a
//! `version_conflict` fails the save naming the bn id rather than overwriting someone else's edit.
//! Title, priority and due date go to one `update_activity`; done, wip and reopen go to
//! `set_activity_status`. A task with no `src:forge:` is new: it is created under the goal whose
//! repository matches its `repo:`, or the Inbox goal, and its meta is written after.
//!
//! **Lock** is inert: Forge serialises writers with versions, not with a local file lock.
//!
//! **Ids** never repeat: a load leaves out tasks completed more than a week ago, so the highest id
//! it can see is not the highest ever assigned. [`super::ids`] remembers that per endpoint, and
//! the loaded file's [`TodoFile::id_floor`] carries it to every `next_id` a caller mints.
//!
//! A store built without a key is still a store: every call fails with the error pointing at
//! `bn forge login`, because the factory that builds it cannot return an error.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use super::client::{ActivityPatch, BnTaskRow, ForgeApi, GoalRow, MetaOutcome, NewActivity};
use super::ids::{self, IdMark, Mark};
use super::mapping::{self, Lifecycle, TaskState};
use crate::resolver::Config;
use crate::task_store::{TaskProviderKind, TaskStore};
use crate::todo::{Task, TodoFile};

/// How many days of completed activities a load includes, so a task finished this week still
/// shows as done instead of vanishing.
pub const COMPLETED_WINDOW_DAYS: u32 = 7;

/// What the last load saw of one activity.
#[derive(Debug, Clone)]
struct Entry {
    bn_id: String,
    /// `None` once a write whose result carries no version (`update_activity`,
    /// `set_activity_status`) has moved it on; the next guarded write fetches it fresh.
    version: Option<i64>,
    state: TaskState,
}

#[derive(Debug, Default)]
struct Snapshot {
    /// Activity id -> entry.
    entries: HashMap<String, Entry>,
    /// bn id -> activity id.
    by_bn_id: HashMap<String, String>,
    /// Identity (uid) of a task this store created -> the activity it became, so saving the same
    /// in-memory file twice does not create it twice.
    created: HashMap<String, String>,
    goals: Option<Vec<GoalRow>>,
    /// The highest bn id this store knows was assigned: the mark plus everything loaded or
    /// created since.
    max_id: u64,
    /// Whether a load has run. `claim` and `note` load only a store that never has, so they
    /// never swap the snapshot out from under a caller between its load and its save.
    loaded: bool,
}

/// The Forge-backed [`TaskStore`].
pub struct ForgeTaskStore {
    api: Option<Arc<dyn ForgeApi>>,
    snapshot: Mutex<Snapshot>,
    /// Where the id high-water mark persists. `None` keeps it in memory only.
    ids: Option<IdMark>,
}

impl ForgeTaskStore {
    /// A store over `api`.
    pub fn new(api: Arc<dyn ForgeApi>) -> ForgeTaskStore {
        ForgeTaskStore {
            api: Some(api),
            snapshot: Mutex::new(Snapshot::default()),
            ids: None,
        }
    }

    /// A store with no key: every call fails with [`super::missing_token_error`].
    pub fn without_token() -> ForgeTaskStore {
        ForgeTaskStore {
            api: None,
            snapshot: Mutex::new(Snapshot::default()),
            ids: None,
        }
    }

    /// Persist the id high-water mark in `mark` instead of memory.
    pub fn with_id_mark(mut self, mark: IdMark) -> ForgeTaskStore {
        self.ids = Some(mark);
        self
    }

    /// The store `cfg` describes: `[tasks] forge_url` and the key from [`super::resolve_token`],
    /// through the process's shared client ([`super::session::api_for`]), with the id mark in the
    /// state directory.
    pub fn from_config(cfg: &Config) -> ForgeTaskStore {
        let Some((token, _)) = super::resolve_token() else {
            return ForgeTaskStore::without_token();
        };
        let url = cfg.tasks_forge_url.trim();
        let state = std::env::var("BN_STATE_DIR")
            .ok()
            .filter(|d| !d.trim().is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| cfg.state_dir());
        ForgeTaskStore::new(super::session::api_for(url, &token))
            .with_id_mark(IdMark::new(state.join(ids::FILE), url))
    }

    /// The Forge tools behind this store, for callers that need more than the task list (the
    /// claim sweep, `bn forge status`).
    pub fn api(&self) -> io::Result<&dyn ForgeApi> {
        self.api.as_deref().ok_or_else(super::missing_token_error)
    }

    /// The activity id behind bn id `id`, as of the last load.
    pub fn activity_id(&self, id: &str) -> Option<String> {
        self.snap().by_bn_id.get(id).cloned()
    }

    fn snap(&self) -> MutexGuard<'_, Snapshot> {
        self.snapshot.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Map `rows` into a file and a fresh snapshot, writing back any id this load had to assign.
    /// `Err(None)` asks the caller to list again: a write-back lost a version race, which means
    /// another writer (likely another machine assigning the same id) got there first.
    fn build(
        &self,
        api: &dyn ForgeApi,
        mut rows: Vec<BnTaskRow>,
        last_attempt: bool,
        floor: u64,
    ) -> Result<TodoFile, Option<io::Error>> {
        rows.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        let parse = ids::number;
        let mut max = ids::max_in(&rows).max(floor);
        let mut seen = std::collections::HashSet::new();
        let today = crate::util::today();
        let mut file = TodoFile::default();
        let mut snap = self.snap();
        snap.entries.clear();
        snap.by_bn_id.clear();
        snap.loaded = true;
        for row in &rows {
            let stored = row
                .agent_meta
                .get("id")
                .map(|id| id.trim().to_string())
                .filter(|id| parse(id).is_some() && !seen.contains(id));
            let (bn_id, assigned) = match stored {
                Some(id) => (id, false),
                None => {
                    max += 1;
                    (format!("t{max}"), true)
                }
            };
            seen.insert(bn_id.clone());
            let task = mapping::task_from_row(row, &bn_id, &today);
            let mut state = mapping::state_of(&task);
            let mut version = Some(row.version);
            if assigned {
                let set = BTreeMap::from([("id".to_string(), bn_id.clone())]);
                match api.set_activity_meta(&row.id, &set, &[], Some(row.version)) {
                    Ok(MetaOutcome::Ok { version: v }) => version = Some(v),
                    Ok(MetaOutcome::VersionConflict { .. }) if !last_attempt => return Err(None),
                    // The id stays assigned in memory for this load, and is left out of the
                    // snapshot so the next save writes it. A failed write-back never fails a
                    // read.
                    Ok(MetaOutcome::VersionConflict { .. }) | Err(_) => {
                        state.meta.remove("id");
                        version = None;
                    }
                }
            }
            snap.by_bn_id.insert(bn_id.clone(), row.id.clone());
            snap.entries.insert(
                row.id.clone(),
                Entry {
                    bn_id,
                    version,
                    state,
                },
            );
            file.push_task(task);
        }
        file.raise_id_floor(max);
        snap.max_id = max;
        Ok(file)
    }

    /// The id floor before a load: the persisted mark, refreshed from the widest list when it is
    /// stale. Returns the floor and whether the wide list confirmed it.
    fn id_floor(&self, api: &dyn ForgeApi, now: i64) -> io::Result<(u64, bool)> {
        let known = self.snap().max_id;
        let Some(mark) = &self.ids else {
            return Ok((known, false));
        };
        let stored = mark.get();
        let floor = known.max(stored.map_or(0, |m| m.max));
        if !Mark::is_stale(stored, now) {
            return Ok((floor, false));
        }
        let wide = api.list_bn_tasks(Some(ids::ID_WINDOW_DAYS))?;
        Ok((floor.max(ids::max_in(&wide)), true))
    }

    /// Persist the id mark, best-effort: a state dir that cannot be written costs the cross-week
    /// guarantee, never a read or a save.
    fn record_ids(&self, max: u64, now: i64, confirmed: bool) {
        if let Some(mark) = &self.ids {
            let _ = mark.record(max, now, confirmed);
        }
    }

    /// Raise the mark past any id this store just created.
    fn record_created(&self, snap: &mut Snapshot) {
        let top = snap
            .by_bn_id
            .keys()
            .filter_map(|id| ids::number(id))
            .max()
            .unwrap_or(0);
        if top > snap.max_id {
            snap.max_id = top;
            self.record_ids(top, crate::util::now_epoch_secs(), false);
        }
    }

    /// Load once, if nothing has loaded yet.
    fn ensure_loaded(&self) -> io::Result<()> {
        if !self.snap().loaded {
            self.load()?;
        }
        Ok(())
    }

    /// Push one task: a diff when it is already an activity, a create when it is not.
    fn push(&self, api: &dyn ForgeApi, snap: &mut Snapshot, task: &Task) -> io::Result<()> {
        let identity = identity(task);
        let activity = mapping::forge_id(task)
            .map(str::to_string)
            .or_else(|| identity.as_ref().and_then(|k| snap.created.get(k).cloned()));
        match activity {
            Some(activity) => push_existing(api, snap, &activity, task),
            None => create(api, snap, task, identity),
        }
    }
}

/// A task's identity for the created-activity map: its uid, else its bn id.
fn identity(task: &Task) -> Option<String> {
    task.uid()
        .map(|u| format!("uid:{u}"))
        .or_else(|| task.id().map(|id| format!("id:{id}")))
}

fn label(task: &Task) -> String {
    task.id().unwrap_or("(no id)").to_string()
}

/// The activity's current version, fetched fresh.
fn fresh_version(api: &dyn ForgeApi, activity: &str) -> io::Result<i64> {
    api.list_bn_tasks(Some(COMPLETED_WINDOW_DAYS))?
        .into_iter()
        .find(|r| r.id == activity)
        .map(|r| r.version)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("Forge activity {activity} is no longer listed"),
            )
        })
}

fn push_existing(
    api: &dyn ForgeApi,
    snap: &mut Snapshot,
    activity: &str,
    task: &Task,
) -> io::Result<()> {
    let Some(entry) = snap.entries.get_mut(activity) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "task {} (forge:{activity}) was not loaded through this store; load, change, then save",
                label(task)
            ),
        ));
    };
    let local = mapping::state_of(task);
    if local == entry.state {
        return Ok(());
    }

    let (set, remove) = mapping::meta_diff(&entry.state.meta, &local.meta);
    if !set.is_empty() || !remove.is_empty() {
        let expected = match entry.version {
            Some(v) => v,
            None => fresh_version(api, activity)?,
        };
        match api.set_activity_meta(activity, &set, &remove, Some(expected))? {
            MetaOutcome::Ok { version } => {
                entry.version = Some(version);
                entry.state.meta = local.meta.clone();
            }
            MetaOutcome::VersionConflict { version } => {
                return Err(io::Error::other(format!(
                    "task {} changed in Forge since it was loaded (version {expected}, now {version}); nothing was overwritten, load and retry",
                    entry.bn_id
                )));
            }
        }
    }

    let mut patch = ActivityPatch::default();
    if local.title != entry.state.title && !local.title.trim().is_empty() {
        patch.title = Some(local.title.clone());
    }
    if local.priority != entry.state.priority {
        patch.priority = Some(local.priority.to_string());
    }
    if local.due != entry.state.due {
        match &local.due {
            Some(d) => patch.due_date = Some(d.clone()),
            None => patch.clear_due_date = true,
        }
    }
    if !patch.is_empty() {
        api.update_activity(activity, &patch)?;
        entry.version = None;
        entry.state.title = local.title.clone();
        entry.state.priority = local.priority;
        entry.state.due = local.due.clone();
    }
    if local.lifecycle != entry.state.lifecycle {
        api.set_activity_status(activity, local.lifecycle.status())?;
        entry.version = None;
        entry.state.lifecycle = local.lifecycle;
    }
    Ok(())
}

fn create(
    api: &dyn ForgeApi,
    snap: &mut Snapshot,
    task: &Task,
    identity: Option<String>,
) -> io::Result<()> {
    let local = mapping::state_of(task);
    if local.title.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("task {} has no title to create in Forge", label(task)),
        ));
    }
    if snap.goals.is_none() {
        snap.goals = Some(api.get_goals()?);
    }
    let goals = snap.goals.as_deref().unwrap_or_default();
    let goal = mapping::pick_goal(goals, task.meta("repo")).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "task {}: no Forge goal has repository {} and there is no Inbox goal to fall back to",
                label(task),
                task.meta("repo").unwrap_or("(none)")
            ),
        )
    })?;
    let new = NewActivity {
        goal_id: goal.id.clone(),
        title: local.title.clone(),
        description: None,
        due_date: local.due.clone(),
        priority: (local.priority != "None").then(|| local.priority.to_string()),
    };
    let activity = api.create_activity(&new)?;
    let mut set = local.meta.clone();
    if let Some(origin) = task
        .meta("src")
        .filter(|s| !s.starts_with(mapping::SRC_PREFIX))
    {
        set.insert("origin".to_string(), origin.to_string());
    }
    let mut version = match api.set_activity_meta(&activity, &set, &[], None)? {
        MetaOutcome::Ok { version } => Some(version),
        MetaOutcome::VersionConflict { .. } => None,
    };
    if local.lifecycle != Lifecycle::Active {
        api.set_activity_status(&activity, local.lifecycle.status())?;
        version = None;
    }
    if let Some(key) = identity {
        snap.created.insert(key, activity.clone());
    }
    let bn_id = task.id().unwrap_or_default().to_string();
    if !bn_id.is_empty() {
        snap.by_bn_id.insert(bn_id.clone(), activity.clone());
    }
    snap.entries.insert(
        activity,
        Entry {
            bn_id,
            version,
            state: local,
        },
    );
    Ok(())
}

impl TaskStore for ForgeTaskStore {
    fn load(&self) -> io::Result<TodoFile> {
        let api = self.api()?;
        let now = crate::util::now_epoch_secs();
        let (floor, _) = self.id_floor(api, now)?;
        let mut attempt = 0;
        loop {
            let rows = api.list_bn_tasks(Some(COMPLETED_WINDOW_DAYS))?;
            match self.build(api, rows, attempt > 0, floor) {
                Ok(file) => {
                    // A successful load saw every id still inside the window, which is what
                    // keeps the mark fresh (see `super::ids`).
                    self.record_ids(file.id_floor(), now, true);
                    return Ok(file);
                }
                Err(Some(e)) => return Err(e),
                Err(None) => attempt += 1,
            }
        }
    }

    /// Push every change since the last load. Tasks are independent, so one task's failure does
    /// not stop the others; the error names every task that failed.
    fn save(&self, file: &TodoFile) -> io::Result<()> {
        let api = self.api()?;
        let mut snap = self.snap();
        let failures: Vec<String> = file
            .tasks()
            .filter_map(|t| self.push(api, &mut snap, t).err())
            .map(|e| e.to_string())
            .collect();
        self.record_created(&mut snap);
        match failures.as_slice() {
            [] => Ok(()),
            [one] => Err(io::Error::other(one.clone())),
            many => Err(io::Error::other(format!(
                "{} tasks failed to save to Forge: {}",
                many.len(),
                many.join("; ")
            ))),
        }
    }

    fn append(&self, task: &Task) -> io::Result<()> {
        let api = self.api()?;
        let mut snap = self.snap();
        let pushed = self.push(api, &mut snap, task);
        self.record_created(&mut snap);
        pushed
    }

    fn lock(&self) -> Box<dyn Any> {
        Box::new(())
    }

    /// Stamp `repo:` and `branch:` into the activity's meta. A merge of two keys, so it needs no
    /// version guard. The snapshot keeps what the last load saw, with its version cleared: a later
    /// save of a file loaded before the claim neither removes the stamp nor trips over the version
    /// the claim moved on.
    fn claim(&self, id: &str, repo: &str, branch: &str) -> io::Result<bool> {
        self.ensure_loaded()?;
        let api = self.api()?;
        let Some(activity) = self.activity_id(id) else {
            return Ok(false);
        };
        let set = BTreeMap::from([
            ("repo".to_string(), repo.to_lowercase()),
            ("branch".to_string(), branch.to_string()),
        ]);
        match api.set_activity_meta(&activity, &set, &[], None)? {
            MetaOutcome::Ok { .. } => {
                if let Some(entry) = self.snap().entries.get_mut(&activity) {
                    entry.version = None;
                }
                Ok(true)
            }
            MetaOutcome::VersionConflict { version } => Err(io::Error::other(format!(
                "task {id}: Forge reported a version conflict (now {version}) on an unguarded meta write"
            ))),
        }
    }

    /// A comment on the activity behind task `id`.
    fn note(&self, id: &str, text: &str) -> io::Result<()> {
        let api = self.api()?;
        self.ensure_loaded()?;
        let activity = self.activity_id(id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("no Forge task {id}"))
        })?;
        api.create_comment(&activity, text)
    }

    fn kind(&self) -> TaskProviderKind {
        TaskProviderKind::ForgeApp
    }
}

#[cfg(test)]
mod tests;
