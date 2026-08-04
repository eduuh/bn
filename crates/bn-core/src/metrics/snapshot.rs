//! Dated scorecard snapshots persisted under `snapshots/<date>.json`.

use super::scorecard::Scorecard;
use crate::resolver::Config;
use serde::{Deserialize, Serialize};

/// A dated scorecard, persisted under `snapshots/<date>.json`. Comparing today's card against the
/// most recent snapshot is the "am I improving?" trend — without a snapshot, metrics only ever
/// show *now*, never the direction.
#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub date: String,
    pub card: Scorecard,
}

/// Persist `card` as the snapshot for `date` (overwrites a same-day snapshot). Returns the path.
/// The weekly `metrics-snapshot` job calls this via `bn metrics --snapshot`.
pub fn write_snapshot(
    cfg: &Config,
    card: &Scorecard,
    date: &str,
) -> std::io::Result<std::path::PathBuf> {
    let dir = cfg.snapshots_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{date}.json"));
    let snap = serde_json::json!({ "date": date, "card": card });
    std::fs::write(&path, serde_json::to_string_pretty(&snap)?)?;
    Ok(path)
}

/// All persisted snapshots, oldest date first. Unparseable files are skipped. Backs the trend
/// history and the "vs <date>" delta line.
pub fn read_snapshots(cfg: &Config) -> Vec<Snapshot> {
    let mut out: Vec<Snapshot> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(cfg.snapshots_dir()) {
        for e in entries.flatten() {
            if let Ok(text) = std::fs::read_to_string(e.path()) {
                if let Ok(snap) = serde_json::from_str::<Snapshot>(&text) {
                    out.push(snap);
                }
            }
        }
    }
    out.sort_by(|a, b| a.date.cmp(&b.date));
    out
}

/// The most recent snapshot strictly before `date` — the baseline today's card is compared to.
pub fn latest_snapshot_before(cfg: &Config, date: &str) -> Option<Snapshot> {
    read_snapshots(cfg)
        .into_iter()
        .rfind(|s| s.date.as_str() < date)
}


