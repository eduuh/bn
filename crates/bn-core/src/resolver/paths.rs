//! [`Config`]'s path-builder block: every note/daily/metrics/state location derived from the
//! resolved roots and the machine subtree layout.

use super::{Config, Resolved};
use std::path::{Path, PathBuf};

impl Config {
    /// Root for person-scoped stores that promise cross-machine sync.
    ///
    /// These stores are global to the person rather than a single repo, but they still need to
    /// live in a git-backed notes root so `commit-notes` can carry them between machines. Fall
    /// back to `personal_root` only in minimal test configs that never populated `work_root`.
    fn synced_person_store_root(&self) -> &Path {
        if self.work_root.as_os_str().is_empty() {
            &self.personal_root
        } else {
            &self.work_root
        }
    }

    fn migrate_synced_person_store_file(&self, name: &str) {
        if self.work_root.as_os_str().is_empty() || self.personal_root == self.work_root {
            return;
        }
        let src = self.personal_root.join(name);
        let dst = self.work_root.join(name);
        if !src.is_file() || dst.exists() {
            return;
        }
        let Some(parent) = dst.parent() else {
            return;
        };
        let _ = std::fs::create_dir_all(parent);
        if std::fs::rename(&src, &dst).is_ok() {
            return;
        }
        let Ok(bytes) = std::fs::read(&src) else {
            return;
        };
        if std::fs::write(&dst, bytes).is_ok() {
            let _ = std::fs::remove_file(&src);
        } else {
            let _ = std::fs::remove_file(&dst);
        }
    }

    fn migrate_synced_person_store_dir(&self, name: &str) {
        if self.work_root.as_os_str().is_empty() || self.personal_root == self.work_root {
            return;
        }
        let src = self.personal_root.join(name);
        let dst = self.work_root.join(name);
        if !src.is_dir() {
            return;
        }
        let _ = migrate_dir_contents(&src, &dst);
        let _ = std::fs::remove_dir(&src);
    }

    /// Storage root for a repo: work dir if classified work (exact, case-sensitive),
    /// else personal. `repo` is expected already-lowercased (see [`resolve`]).
    pub fn notes_dir_for(&self, repo: &str) -> &Path {
        if self.is_work_repo(repo) {
            &self.work_root
        } else {
            &self.personal_root
        }
    }

    /// `<notes_dir_for(repo)>/<machine>/<repo>/<branch>` — the note directory. The `<machine>`
    /// segment keeps each laptop's notes in its own subtree so the synced note repos never
    /// git-conflict on shared (e.g. `main`) notes.
    pub fn note_dir(&self, ctx: &Resolved) -> PathBuf {
        self.notes_dir_for(&ctx.repo)
            .join(self.machine())
            .join(&ctx.repo)
            .join(&ctx.branch)
    }

    /// `<note_dir>/note.md` — the note file.
    pub fn note_file(&self, ctx: &Resolved) -> PathBuf {
        self.note_dir(ctx).join("note.md")
    }

    /// `<note_dir>/plans` — branch-scoped plan artifacts. The directory is durable bn state,
    /// not repo worktree state, so plan files follow the branch note across machines.
    pub fn plan_dir(&self, ctx: &Resolved) -> PathBuf {
        self.note_dir(ctx).join("plans")
    }

    /// `<note_dir>/plans/active.md` — the approved/current execution contract.
    pub fn active_plan_file(&self, ctx: &Resolved) -> PathBuf {
        self.plan_dir(ctx).join("active.md")
    }

    /// `<note_dir>/plans/agents` — per-agent draft plans keyed by session id.
    pub fn agent_plan_dir(&self, ctx: &Resolved) -> PathBuf {
        self.plan_dir(ctx).join("agents")
    }

    /// `<note_dir>/plans/agents/<session-id>.md` — one agent's draft plan.
    pub fn agent_plan_file(&self, ctx: &Resolved, session_id: &str) -> PathBuf {
        let safe = session_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>();
        self.agent_plan_dir(ctx).join(format!(
            "{}.md",
            if safe.is_empty() { "agent" } else { &safe }
        ))
    }

    /// Convert an absolute artifact path under this note dir to a portable note-relative path.
    pub fn note_relative_path(&self, ctx: &Resolved, path: &Path) -> Option<String> {
        let note_dir = self.note_dir(ctx);
        path.strip_prefix(&note_dir)
            .ok()
            .map(|p| p.to_path_buf())
            .or_else(|| {
                let path = std::fs::canonicalize(path).ok()?;
                let note_dir = std::fs::canonicalize(note_dir).ok()?;
                path.strip_prefix(note_dir).ok().map(|p| p.to_path_buf())
            })
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    /// `<work_root>/tasks.txt` — **the** task store (#995): one file holding every task, for
    /// every repo, on every machine.
    ///
    /// **Person-scoped, not machine-scoped**, following [`Config::staff_feed_dir`] (#270) rather
    /// than [`Config::note_dir`]. The `<machine>` segment exists so synced note repos never
    /// git-conflict on a shared `main` note — but a task list that differs per laptop is not a
    /// single source of truth, which is the whole point of the store. The file is `merge=union`
    /// (see [`crate::todo::ensure_union_merge`]) and every line carries `uid:`/`mod:` so a
    /// concatenating merge is repaired on read rather than conflicting.
    pub fn tasks_file(&self) -> PathBuf {
        self.migrate_synced_person_store_file("tasks.txt");
        self.synced_person_store_root().join("tasks.txt")
    }

    /// `<work_root>/tasks` — the staging area for task context docs, before a task is picked
    /// up into a worktree and its doc moves next to the branch note. Person-scoped like
    /// [`Config::tasks_file`], and syncs with it.
    pub fn tasks_dir(&self) -> PathBuf {
        self.migrate_synced_person_store_dir("tasks");
        self.synced_person_store_root().join("tasks")
    }

    /// `<personal_root>/<machine>/daily` — the daily-goals subtree. Day-scoped and
    /// cross-branch (never keyed by repo/branch), it lives beside the per-repo note dirs
    /// in this machine's subtree so it syncs with — but never collides with — branch notes.
    pub fn daily_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("daily")
    }

    /// `<daily_dir>/<date>.md` — one file per day (`date` is a `YYYY-MM-DD` string).
    pub fn daily_file(&self, date: &str) -> PathBuf {
        self.daily_dir().join(format!("{date}.md"))
    }

    /// `<personal_root>/staff/feed` — the agent-staff briefing-card store
    /// (`feed/<YYYY-MM-DD>/<agent>.json` + rendered `.md`). **Person-scoped, not machine-scoped**
    /// (#270): the staff is one team that runs on a single home machine (see [`Config::staff_home`]),
    /// so the cards live once under the synced notes root and every box reads the same feed — no
    /// duplicate reviews, no per-machine git-conflict. Append-only by date; read-marks stay in
    /// machine-local state, so the cards travel but "seen" doesn't.
    pub fn staff_feed_dir(&self) -> PathBuf {
        self.personal_root.join("staff").join("feed")
    }

    /// The pre-#270 machine-scoped feed location (`<personal_root>/<machine>/feed`). Only the
    /// best-effort one-time migration in [`crate::staff::migrate_feed_layout`] reads this, to
    /// fold any cards written under the old layout into the person-scoped [`staff_feed_dir`].
    pub fn staff_feed_legacy_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("feed")
    }

    /// `<personal_root>/<machine>/inbox.md` — the global capture inbox (`bn q`). Cross-branch
    /// and machine-scoped like `daily/`, so captures ride the notes sync without git conflicts.
    pub fn inbox_file(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("inbox.md")
    }

    /// `<personal_root>/<machine>/dojo` — the `bn dojo` learning store (#253). Cross-branch and
    /// machine-scoped like `daily/`, it holds the append-only `cards.jsonl` / `reviews.jsonl` /
    /// `sessions.jsonl` logs so they sync across devices without git-colliding.
    pub fn dojo_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("dojo")
    }

    /// `<personal_root>/<machine>/quarters` — the quarter-goals subtree. Like `daily/`, it's
    /// cross-branch and lives in this machine's notes subtree so it syncs without git-colliding.
    /// Holds one file per quarter (`<YYYY-Qn>.md`): the layer above daily/weekly goals.
    pub fn quarters_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("quarters")
    }

    /// `<quarters_dir>/<YYYY-Qn>.md` — one file per quarter (e.g. `2026-Q3.md`).
    pub fn quarter_file(&self, quarter: &str) -> PathBuf {
        self.quarters_dir().join(format!("{quarter}.md"))
    }

    /// `<personal_root>/<machine>/track` — the time-tracking subtree. Like `daily/`, it's
    /// day-scoped and cross-branch, living in this machine's notes subtree so it syncs without
    /// git-colliding across devices.
    pub fn track_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("track")
    }

    /// `<track_dir>/<date>.jsonl` — one append-only punch log per day (`date` is `YYYY-MM-DD`).
    pub fn track_file(&self, date: &str) -> PathBuf {
        self.track_dir().join(format!("{date}.jsonl"))
    }

    /// `<personal_root>/<machine>/metrics` — the **synced, durable** productivity-metrics
    /// substrate (pr_*, activity, note_closed). A sibling of `state_dir` (cron telemetry +
    /// caches) under the same per-machine notes subtree; kept separate so productivity history
    /// and runtime telemetry stay distinct. Like `daily_dir`, the per-machine segment keeps each
    /// device's writes conflict free; the whole-person view reads every machine's copy (see
    /// `metrics_logs_all_machines`).
    pub fn metrics_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("metrics")
    }

    /// `<metrics_dir>/events.jsonl` — this machine's productivity event log (pr_*, activity,
    /// note_closed). The append target for `bn event`; the per-machine read source.
    pub fn metrics_log_path(&self) -> PathBuf {
        self.metrics_dir().join("events.jsonl")
    }

    /// `<metrics_dir>/snapshots` — dated weekly scorecard snapshots; the trend/"growth" store.
    pub fn snapshots_dir(&self) -> PathBuf {
        self.metrics_dir().join("snapshots")
    }

    /// `<metrics_dir>/scoreboard-baseline.json` — the single fixed baseline anchor the scoreboard
    /// (#276) compares against: the pre-staff snapshot of the success metrics, recorded once via
    /// `bn insights --set-baseline`. A sibling of `snapshots/` under the synced metrics subtree, so
    /// the anchor syncs across machines. Unlike the rolling per-week snapshots this is one durable
    /// point-in-time comparison anchor.
    pub fn scoreboard_baseline_path(&self) -> PathBuf {
        self.metrics_dir().join("scoreboard-baseline.json")
    }

    /// `<personal_root>/<machine>/insights` — the weekly auto-retro archive. Like `daily/`, it's
    /// cross-branch and per-machine so each device's snapshots sync without git-colliding; history
    /// accumulates one file per ISO week (see [`insights_file`](Self::insights_file)).
    pub fn insights_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("insights")
    }

    /// `<insights_dir>/<YYYY-Www>.md` — one rendered retro per ISO week (`week` is a `2026-W26`
    /// label from [`crate::util::iso_week`]). The `weekly-insights` cron writes here.
    pub fn insights_file(&self, week: &str) -> PathBuf {
        self.insights_dir().join(format!("{week}.md"))
    }

    /// `<personal_root>/<machine>/chronicle` — the Archivist's private weekly-chronicle archive
    /// (#263). Like `insights/` it's cross-branch and per-machine so each device's chronicles sync
    /// without git-colliding; history accumulates one file per ISO week (see
    /// [`chronicle_file`](Self::chronicle_file)). Inside the private notes repo by construction — the
    /// story of the week is never published.
    pub fn chronicle_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("chronicle")
    }

    /// `<chronicle_dir>/<YYYY-Www>.md` — one weekly chronicle per ISO week (`week` is a `2026-W26`
    /// label from [`crate::util::iso_week`]). The `staff-archivist` cron writes here Fridays.
    pub fn chronicle_file(&self, week: &str) -> PathBuf {
        self.chronicle_dir().join(format!("{week}.md"))
    }

    /// `<personal_root>/<machine>/retro` — the Interviewer's private subjective-retro transcript
    /// store (#261). Like `insights/`, it's cross-branch and per-machine so each device's
    /// transcripts sync without git-colliding; one file per ISO week. The transcript never leaves
    /// the notes repo — only the synthesized card is mirrored to the feed.
    pub fn retro_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("retro")
    }

    /// `<retro_dir>/<YYYY-Www>.md` — one Interviewer transcript per ISO week (`week` is a
    /// `2026-W28` label from [`crate::util::iso_week`]).
    pub fn retro_file(&self, week: &str) -> PathBuf {
        self.retro_dir().join(format!("{week}.md"))
    }

    /// Every machine's metrics log under `personal_root` (`<personal_root>/*/metrics/events.jsonl`).
    /// This is the whole-person view: each device writes only its own `<machine>/` subtree, so
    /// unioning these siblings reconstructs you-as-an-engineer across all machines. Missing logs
    /// are simply absent. Sorted for determinism; this machine is included when its log exists.
    pub fn metrics_logs_all_machines(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.personal_root) {
            for e in entries.flatten() {
                let p = e.path().join("metrics").join("events.jsonl");
                if p.is_file() {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }

    /// `<personal_root>/<machine>/state` — runtime state (the cron event log, caches, per-session
    /// model captures). Lives in this machine's subtree of the synced notes repo, beside `daily/`
    /// and `metrics/`, so it syncs across machines without ever git-colliding (each host owns its
    /// own subtree). The free [`state_dir`] delegates here once `$BN_STATE_DIR` is unset.
    pub fn state_dir(&self) -> PathBuf {
        self.personal_root.join(self.machine()).join("state")
    }

    /// The bare-repos dir, sibling of the worktree dir (`$PROJECT_ROOT/{bare,worktree}`;
    /// lib: config/scripts/tmux-lib.zsh:9-10). A repo is worktree-managed iff `<bare_dir>/<repo>.git` exists —
    /// the guard `prune` uses so it only auto-closes notes for repos backed by a bare clone.
    pub fn bare_dir(&self) -> PathBuf {
        self.worktree_dir
            .parent()
            .map(|p| p.join("bare"))
            .unwrap_or_else(|| PathBuf::from("bare"))
    }

    /// `<notes_dir_for(repo)>/<machine>/<repo>/skills` — per-repo **personal** skills:
    /// private, synced with your notes, a sibling of the branch note dirs (so they aren't
    /// tied to one branch). `repo` should be lowercased (as [`resolve`] returns it). Served
    /// by bn-mcp as MCP prompts alongside the global and repo-shared skills.
    pub fn repo_skills_dir(&self, repo: &str) -> PathBuf {
        self.notes_dir_for(repo)
            .join(self.machine())
            .join(repo)
            .join("skills")
    }

    /// `<notes_dir_for(repo)>/<machine>/<repo>/reference` — per-repo **reference notes**: what
    /// past sessions discovered about this repository (how it builds, where the seams are, which
    /// failures are misleading). A sibling of [`repo_skills_dir`](Config::repo_skills_dir) and of
    /// the branch note dirs, so the knowledge outlives any one branch. Unlike the person-scoped
    /// [`references_dir`](Config::references_dir) (coding standards, which apply everywhere), repo
    /// notes are keyed by `<machine>` like the branch notes they sit beside. Holds nested topic
    /// dirs (`architecture/runtime-concepts.md`) and an `images/` sibling for screenshots.
    /// `repo` should be lowercased (as [`resolve`](super::resolve) returns it).
    pub fn repo_references_dir(&self, repo: &str) -> PathBuf {
        self.notes_dir_for(repo)
            .join(self.machine())
            .join(repo)
            .join("reference")
    }

    /// `<work_root>/references` — the reusable standards/preferences store (`bn ref`, #623).
    /// **Person-scoped, cross-machine** (mirrors `staff/`, NOT machine-scoped like `daily`/`state`):
    /// coding standards apply on every host, so the files live once under the synced notes root and
    /// every box reads the same references — no per-machine duplication. One markdown file per topic
    /// (`references/<name>.md`), synced via the branch-notes git repo like everything else. The free
    /// [`references_dir`](super::references_dir) delegates here once `$BN_REFERENCES_DIR` is unset.
    pub fn references_dir(&self) -> PathBuf {
        self.migrate_synced_person_store_dir("references");
        self.synced_person_store_root().join("references")
    }

    /// `<work_root>/links.md` — the store of links worth going back to (`bn links`).
    /// **Person-scoped, cross-machine**, for the same reason as [`references_dir`](Self::references_dir):
    /// a PBI or a portal blade is yours, not one laptop's. Sits beside `tasks.txt` in the synced
    /// notes root rather than under `<machine>/`.
    pub fn links_file(&self) -> PathBuf {
        self.migrate_synced_person_store_file("links.md");
        self.synced_person_store_root().join("links.md")
    }

    /// Existing-roots in oracle order: personal first, then work (only if they exist).
    /// Used by cross-repo scans (list/summary). Dedups when both roots collapse. On a
    /// [`work_only`](Config::work_only) machine the personal root is skipped entirely, so a
    /// scan never even reads personal notes into memory.
    pub fn each_notes_root(&self) -> Vec<&Path> {
        let candidates: &[&Path] = if self.work_only {
            &[self.work_root.as_path()]
        } else {
            &[self.personal_root.as_path(), self.work_root.as_path()]
        };
        let mut roots: Vec<&Path> = Vec::new();
        for &r in candidates {
            if r.is_dir() && !roots.contains(&r) {
                roots.push(r);
            }
        }
        roots
    }
}

fn migrate_dir_contents(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            migrate_dir_contents(&src_path, &dst_path)?;
            let _ = std::fs::remove_dir(&src_path);
            continue;
        }
        if dst_path.exists() {
            continue;
        }
        if std::fs::rename(&src_path, &dst_path).is_ok() {
            continue;
        }
        std::fs::copy(&src_path, &dst_path)?;
        std::fs::remove_file(&src_path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synced_person_stores_use_work_root() {
        let cfg = Config {
            personal_root: PathBuf::from("/personal"),
            work_root: PathBuf::from("/work"),
            ..Default::default()
        };

        assert_eq!(cfg.tasks_file(), PathBuf::from("/work/tasks.txt"));
        assert_eq!(cfg.tasks_dir(), PathBuf::from("/work/tasks"));
        assert_eq!(cfg.references_dir(), PathBuf::from("/work/references"));
        assert_eq!(cfg.links_file(), PathBuf::from("/work/links.md"));
    }

    #[test]
    fn synced_person_stores_fall_back_to_personal_root_without_work_root() {
        let cfg = Config {
            personal_root: PathBuf::from("/personal"),
            ..Default::default()
        };

        assert_eq!(cfg.tasks_file(), PathBuf::from("/personal/tasks.txt"));
        assert_eq!(cfg.tasks_dir(), PathBuf::from("/personal/tasks"));
        assert_eq!(cfg.references_dir(), PathBuf::from("/personal/references"));
        assert_eq!(cfg.links_file(), PathBuf::from("/personal/links.md"));
    }

    #[test]
    fn synced_person_store_getters_migrate_legacy_files_and_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let personal = tmp.path().join("personal");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(personal.join("tasks")).expect("personal tasks dir");
        std::fs::create_dir_all(personal.join("references")).expect("personal refs dir");
        std::fs::write(personal.join("tasks.txt"), "t1").expect("legacy tasks file");
        std::fs::write(personal.join("links.md"), "k1").expect("legacy links file");
        std::fs::write(personal.join("tasks").join("t1.md"), "task").expect("legacy task doc");
        std::fs::write(personal.join("references").join("style.md"), "ref")
            .expect("legacy reference");

        let cfg = Config {
            personal_root: personal.clone(),
            work_root: work.clone(),
            ..Default::default()
        };

        assert_eq!(cfg.tasks_file(), work.join("tasks.txt"));
        assert_eq!(cfg.tasks_dir(), work.join("tasks"));
        assert_eq!(cfg.references_dir(), work.join("references"));
        assert_eq!(cfg.links_file(), work.join("links.md"));

        assert_eq!(
            std::fs::read_to_string(work.join("tasks.txt")).expect("migrated tasks"),
            "t1"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("links.md")).expect("migrated links"),
            "k1"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("tasks").join("t1.md")).expect("migrated task doc"),
            "task"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("references").join("style.md"))
                .expect("migrated reference"),
            "ref"
        );
        assert!(!personal.join("tasks.txt").exists());
        assert!(!personal.join("links.md").exists());
        assert!(!personal.join("tasks").exists());
        assert!(!personal.join("references").exists());
    }
}
