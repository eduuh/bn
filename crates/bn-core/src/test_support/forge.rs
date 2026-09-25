//! An in-memory Forge for tests: activities with versions, the claim rule, comments, and a log
//! of every tool call. Shared by the store's own tests and by bn-cli's pipeline tests, which reach
//! it through the ordinary `task_store(cfg)` path via [`install`].

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::forge_app::client::{
    ActivityPatch, ActivityStatus, Assignee, BnTaskRow, ClaimOutcome, FamilyMemberRow, ForgeApi,
    GoalRow, MetaOutcome, NewActivity,
};
use crate::forge_app::store::COMPLETED_WINDOW_DAYS;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The fake. Build with [`FakeForge::with`], seed rows with [`row`] and [`agent_row`].
#[derive(Default)]
pub struct FakeForge {
    pub rows: Mutex<Vec<BnTaskRow>>,
    /// Completed long ago: listed only for a window wider than the store's default week.
    pub archived: Mutex<Vec<BnTaskRow>>,
    pub goals: Vec<GoalRow>,
    calls: Mutex<Vec<String>>,
    /// Bump this activity's version before the next guarded meta write, as another writer would.
    pub race: Mutex<Option<String>>,
    /// Make every `create_comment` fail, as an unreachable Forge would.
    pub fail_comments: AtomicBool,
}

impl FakeForge {
    /// A fake holding `rows`, with an Inbox goal and a `bn` project goal.
    pub fn with(rows: Vec<BnTaskRow>) -> Arc<FakeForge> {
        Arc::new(FakeForge {
            rows: Mutex::new(rows),
            goals: vec![
                goal("g-inbox", "Inbox", None),
                goal("g-bn", "bn", Some("https://github.com/eduuh/bn")),
            ],
            ..FakeForge::default()
        })
    }

    fn log(&self, call: String) {
        lock(&self.calls).push(call);
    }

    /// Every call since the last take, oldest first.
    pub fn take_calls(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.calls))
    }

    /// The comments posted so far, as `(activity, text)`, without consuming the call log.
    pub fn comments(&self) -> Vec<(String, String)> {
        lock(&self.calls)
            .iter()
            .filter_map(|c| c.strip_prefix("create_comment "))
            .filter_map(|c| c.split_once(' '))
            .map(|(a, t)| (a.to_string(), t.to_string()))
            .collect()
    }

    /// A copy of activity `id`, or a default row when there is none.
    pub fn row(&self, id: &str) -> BnTaskRow {
        lock(&self.rows)
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .unwrap_or_default()
    }

    /// Change activity `id` and bump its version.
    pub fn edit(&self, id: &str, f: impl FnOnce(&mut BnTaskRow)) -> io::Result<()> {
        let mut rows = lock(&self.rows);
        let row = rows
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no activity {id}")))?;
        f(row);
        row.version += 1;
        Ok(())
    }
}

/// Put `api` behind `url` for the key the test sets in `BN_FORGE_TOKEN`, so `task_store(cfg)`
/// with `cfg.tasks_forge_url = url` reaches it. Use a URL unique to the test.
pub fn install(url: &str, token: &str, api: Arc<FakeForge>) {
    crate::forge_app::session::register(url, token, api);
}

impl ForgeApi for FakeForge {
    fn list_bn_tasks(&self, days: Option<u32>) -> io::Result<Vec<BnTaskRow>> {
        self.log(format!("list_bn_tasks {days:?}"));
        let mut rows = lock(&self.rows).clone();
        if days.is_some_and(|d| d > COMPLETED_WINDOW_DAYS) {
            rows.extend(lock(&self.archived).iter().cloned());
        }
        if days == Some(0) {
            rows.retain(|r| !r.is_completed);
        }
        Ok(rows)
    }

    fn create_activity(&self, new: &NewActivity) -> io::Result<String> {
        self.log(format!(
            "create_activity goal={} title={} due={:?} priority={:?}",
            new.goal_id, new.title, new.due_date, new.priority
        ));
        let mut rows = lock(&self.rows);
        let id = format!("new-{}", rows.len());
        let repo_url = self
            .goals
            .iter()
            .find(|g| g.id == new.goal_id)
            .and_then(|g| g.repo_url.clone());
        rows.push(BnTaskRow {
            id: id.clone(),
            goal_id: new.goal_id.clone(),
            goal_repo_url: repo_url,
            title: new.title.clone(),
            priority: new.priority.clone().unwrap_or_else(|| "None".into()),
            due_date: new.due_date.clone(),
            status: "Active".into(),
            created_at: "2026-09-14T09:00:00Z".into(),
            version: 1,
            ..BnTaskRow::default()
        });
        Ok(id)
    }

    fn update_activity(&self, id: &str, patch: &ActivityPatch) -> io::Result<()> {
        self.log(format!("update_activity {id} {patch:?}"));
        self.edit(id, |r| {
            if let Some(t) = &patch.title {
                r.title = t.clone();
            }
            if let Some(p) = &patch.priority {
                r.priority = p.clone();
            }
            if let Some(d) = &patch.due_date {
                r.due_date = Some(d.clone());
            }
            if patch.clear_due_date {
                r.due_date = None;
            }
        })
    }

    fn set_activity_status(&self, id: &str, status: ActivityStatus) -> io::Result<()> {
        self.log(format!("set_activity_status {id} {}", status.word()));
        self.edit(id, |r| match status {
            ActivityStatus::Completed => {
                r.is_completed = true;
                r.completed_at = Some("2026-09-14T10:00:00Z".into());
            }
            ActivityStatus::Started => {
                r.is_completed = false;
                r.started_at = Some("2026-09-14T10:00:00Z".into());
            }
            ActivityStatus::Active => {
                r.is_completed = false;
                r.started_at = None;
            }
        })
    }

    fn set_activity_meta(
        &self,
        id: &str,
        set: &BTreeMap<String, String>,
        remove: &[String],
        expected: Option<i64>,
    ) -> io::Result<MetaOutcome> {
        self.log(format!(
            "set_activity_meta {id} set={set:?} remove={remove:?} expected={expected:?}"
        ));
        if lock(&self.race).as_deref() == Some(id) {
            self.edit(id, |r| r.title = "edited elsewhere".into())?;
            *lock(&self.race) = None;
        }
        let current = self.row(id).version;
        if expected.is_some_and(|v| v != current) {
            return Ok(MetaOutcome::VersionConflict { version: current });
        }
        self.edit(id, |r| {
            r.agent_meta
                .extend(set.iter().map(|(k, v)| (k.clone(), v.clone())));
            for k in remove {
                r.agent_meta.remove(k);
            }
        })?;
        Ok(MetaOutcome::Ok {
            version: self.row(id).version,
        })
    }

    /// Forge's rule: an agent member is assigned, nothing has started it, and it is not complete.
    fn claim_activity(&self, id: &str, claimer: &str) -> io::Result<ClaimOutcome> {
        self.log(format!("claim_activity {id} {claimer}"));
        let r = self.row(id);
        if r.is_completed {
            return Ok(ClaimOutcome::Refused("completed".into()));
        }
        if !r.assignees.iter().any(|a| a.is_agent) {
            return Ok(ClaimOutcome::Refused("not_agent_assigned".into()));
        }
        if r.started_at.is_some() {
            return Ok(ClaimOutcome::Refused("already_claimed".into()));
        }
        self.edit(id, |r| {
            r.started_at = Some("2026-09-14T11:00:00Z".into());
            r.agent_meta.insert("claimer".into(), claimer.into());
        })?;
        Ok(ClaimOutcome::Claimed(Box::new(self.row(id))))
    }

    fn assign_activity(&self, id: &str, user_ids: &[String]) -> io::Result<BnTaskRow> {
        self.log(format!("assign_activity {id} {user_ids:?}"));
        Ok(self.row(id))
    }

    fn create_comment(&self, activity_id: &str, text: &str) -> io::Result<()> {
        if self.fail_comments.load(Ordering::Relaxed) {
            return Err(io::Error::other("Forge MCP: connection refused"));
        }
        self.log(format!("create_comment {activity_id} {text}"));
        Ok(())
    }

    fn get_goals(&self) -> io::Result<Vec<GoalRow>> {
        self.log("get_goals".into());
        Ok(self.goals.clone())
    }

    fn get_family_members(&self) -> io::Result<Vec<FamilyMemberRow>> {
        self.log("get_family_members".into());
        Ok(vec![])
    }
}

/// A goal row.
pub fn goal(id: &str, name: &str, url: Option<&str>) -> GoalRow {
    GoalRow {
        id: id.into(),
        name: name.into(),
        repo_url: url.map(str::to_string),
        status: "active".into(),
        parent_goal_id: None,
    }
}

/// An open activity under the `bn` project goal, with `meta` as its agent meta.
pub fn row(id: &str, title: &str, created: &str, meta: &[(&str, &str)]) -> BnTaskRow {
    BnTaskRow {
        id: id.into(),
        goal_id: "g-bn".into(),
        goal_name: "bn".into(),
        goal_repo_url: Some("https://github.com/eduuh/bn.git".into()),
        title: title.into(),
        priority: "None".into(),
        status: "Active".into(),
        created_at: created.into(),
        version: 1,
        agent_meta: meta
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..BnTaskRow::default()
    }
}

/// [`row`], assigned to the family's agent member.
pub fn agent_row(id: &str, title: &str, created: &str, meta: &[(&str, &str)]) -> BnTaskRow {
    BnTaskRow {
        assignees: vec![Assignee {
            user_id: "u-agent".into(),
            display_name: Some("Agents".into()),
            is_agent: true,
        }],
        ..row(id, title, created, meta)
    }
}

/// The key every [`ForgeEnv`] sets in `BN_FORGE_TOKEN`.
pub const TEST_TOKEN: &str = "fam_test_support_key";

/// A config whose task provider is a [`FakeForge`], reached through `task_store(cfg)`, with the
/// state directory in a temp dir. Holds the env lock for its whole life, so keep it alive for the
/// test and let it drop at the end.
pub struct ForgeEnv {
    pub cfg: crate::resolver::Config,
    pub fake: Arc<FakeForge>,
    pub dir: tempfile::TempDir,
    _guards: (super::EnvGuard, super::EnvGuard),
    _env: MutexGuard<'static, ()>,
}

impl ForgeEnv {
    /// `name` must be unique to the test: it names the fake's endpoint.
    pub fn new(name: &str, rows: Vec<BnTaskRow>) -> io::Result<ForgeEnv> {
        let env = super::env_lock();
        let dir = tempfile::tempdir()?;
        let guards = (
            super::EnvGuard::set(crate::forge_app::TOKEN_ENV, TEST_TOKEN),
            super::EnvGuard::set("BN_STATE_DIR", dir.path().join("state")),
        );
        let url = format!("http://forge.test/{name}");
        let fake = FakeForge::with(rows);
        install(&url, TEST_TOKEN, fake.clone());
        let cfg = crate::resolver::Config {
            personal_root: dir.path().to_path_buf(),
            work_root: dir.path().to_path_buf(),
            managed_repos: vec!["https://github.com/eduuh/bn.git".to_string()],
            tasks_provider: crate::task_store::TaskProviderKind::ForgeApp,
            tasks_forge_url: url,
            ..crate::resolver::Config::default()
        };
        Ok(ForgeEnv {
            cfg,
            fake,
            dir,
            _guards: guards,
            _env: env,
        })
    }
}
