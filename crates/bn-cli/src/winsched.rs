//! Reading Windows Task Scheduler tasks that run `bn run <job>` — the WSL-wake pattern, where a
//! Windows task runs `wsl.exe … bn run <job>` to both boot WSL and do the work. `bn cron` shows
//! these alongside systemd timers so a job scheduled from the Windows side doesn't read as
//! "not scheduled".
//!
//! The query goes over WSL interop via `powershell.exe`, whose cold start is ~2s — far too slow
//! for an interactive dashboard. So `registered()` serves a cached snapshot instantly and
//! refreshes in a detached background process when it's stale (synchronously only on a cold
//! miss). Off WSL (no interop) it's a no-op: empty, and no cache files are written.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A Windows Task Scheduler task that runs `bn run <job>`.
#[derive(Debug, Clone)]
pub struct WinTask {
    pub job: String,
    pub task_name: String,
    /// Next run, epoch seconds. `None` when the task has no scheduled next run.
    pub next_epoch: Option<i64>,
    /// Task Scheduler state, e.g. `Ready`, `Running`, `Disabled`.
    pub state: String,
    /// Last run's exit code (`0` = ok). `None` if the task has never run.
    pub last_result: Option<i64>,
}

impl WinTask {
    /// A disabled task won't fire — its schedule is effectively dead.
    pub fn disabled(&self) -> bool {
        self.state.eq_ignore_ascii_case("disabled")
    }
    /// Last run failed (non-zero exit). `false` if it has never run or succeeded.
    pub fn last_failed(&self) -> bool {
        matches!(self.last_result, Some(r) if r != 0)
    }
}

/// Whether we're on WSL with Windows interop — i.e. Windows Task Scheduler is reachable. Lets the
/// dashboard warn about a missing keepalive only where that warning makes sense.
pub fn on_wsl() -> bool {
    has_wsl_interop()
}

/// Cached snapshot older than this is refreshed (in the background). Windows tasks change rarely;
/// this only bounds how stale the shown "next run" can drift.
const CACHE_TTL_SECS: i64 = 3600;

// One TSV row per bn task: `job\tnext_epoch\tstate\tlast_result\ttaskname`. Maps each task to its
// job by matching `bn run <job>` in the action arguments — the mirror of how timers.rs reads
// ExecStart. Disabled tasks are included (not filtered) so the dashboard can flag a dead schedule.
const PS_QUERY: &str = "Get-ScheduledTask | Where-Object { ($_.Actions | ForEach-Object Arguments) -match 'bn run' } | ForEach-Object { $a=($_.Actions|ForEach-Object Arguments) -join ' '; $j=if($a -match 'bn run (\\S+)'){$Matches[1]}else{''}; $i=$_|Get-ScheduledTaskInfo; $n=$i.NextRunTime; $e=if($n){[int64]([DateTimeOffset]$n).ToUnixTimeSeconds()}else{0}; $lr=if($i.LastTaskResult -ne $null){$i.LastTaskResult}else{''}; \"$j`t$e`t$($_.State)`t$lr`t$($_.TaskName)\" }";

/// Windows tasks running a `bn run <job>`, served from cache for speed. Empty off WSL. May spawn
/// a detached background refresh when the cache is stale; pays the live query only on a cold miss.
pub fn registered() -> Vec<WinTask> {
    if !has_wsl_interop() {
        return Vec::new();
    }
    let now = bn_core::util::now_epoch_secs();
    match read_cache() {
        Some((ts, tasks)) if now - ts < CACHE_TTL_SECS => tasks,
        Some((_, stale)) => {
            spawn_background_refresh();
            stale
        }
        None => refresh(),
    }
}

/// Run the live (slow) query, write the cache, and return the tasks. The detached background
/// refresh process (`bn cron refresh-windows`) calls this; so does a cold-miss `registered()`.
pub fn refresh() -> Vec<WinTask> {
    let tasks = query();
    write_cache(bn_core::util::now_epoch_secs(), &tasks);
    tasks
}

/// WSL interop marker — present only when Windows binaries (powershell.exe) are launchable.
fn has_wsl_interop() -> bool {
    std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
}

fn query() -> Vec<WinTask> {
    let Some(out) = run_powershell(PS_QUERY) else {
        return Vec::new();
    };
    out.lines().filter_map(parse_row).collect()
}

fn parse_row(line: &str) -> Option<WinTask> {
    // splitn(5): task names may contain spaces (never tabs) and come last.
    let mut f = line.splitn(5, '\t');
    let job = f.next()?.trim().to_string();
    let epoch: i64 = f.next()?.trim().parse().ok()?;
    let state = f.next()?.trim().to_string();
    let last_result = f.next()?.trim().parse::<i64>().ok();
    let task_name = f.next().unwrap_or("").trim().to_string();
    if job.is_empty() {
        return None;
    }
    Some(WinTask {
        job,
        task_name,
        next_epoch: (epoch > 0).then_some(epoch),
        state,
        last_result,
    })
}

/// Run a PowerShell snippet by piping it to `powershell.exe -Command -` (stdin), which sidesteps
/// the script-file execution policy and all shell-quoting. `None` if powershell.exe is missing or
/// exits non-zero.
fn run_powershell(script: &str) -> Option<String> {
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(script.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Re-exec `bn cron refresh-windows` detached so the dashboard returns now and the cache is fresh
/// for the next call. Fire-and-forget: the handle is dropped, orphaning the child to finish alone.
fn spawn_background_refresh() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .args(["cron", "refresh-windows"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn cache_path() -> PathBuf {
    bn_core::resolver::state_dir().join("winsched-cache.json")
}

fn write_cache(ts: i64, tasks: &[WinTask]) {
    let rows: Vec<_> = tasks
        .iter()
        .map(|t| {
            serde_json::json!({
                "job": t.job,
                "task_name": t.task_name,
                "next_epoch": t.next_epoch,
                "state": t.state,
                "last_result": t.last_result,
            })
        })
        .collect();
    let body = serde_json::json!({ "ts": ts, "tasks": rows }).to_string();
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, body);
}

fn read_cache() -> Option<(i64, Vec<WinTask>)> {
    let data = std::fs::read_to_string(cache_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    let ts = v.get("ts")?.as_i64()?;
    let tasks = v
        .get("tasks")?
        .as_array()?
        .iter()
        .filter_map(|t| {
            Some(WinTask {
                job: t.get("job")?.as_str()?.to_string(),
                task_name: t.get("task_name")?.as_str()?.to_string(),
                next_epoch: t.get("next_epoch").and_then(|x| x.as_i64()),
                // Tolerate caches written before these fields existed.
                state: t
                    .get("state")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                last_result: t.get("last_result").and_then(|x| x.as_i64()),
            })
        })
        .collect();
    Some((ts, tasks))
}


