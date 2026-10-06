//! `bn links check`: run each link's probe ([`check::probe_for`]) and report what it found.
//! Read-only — it never writes the store; deleting stays a deliberate `rm` or picker `ctrl-d`.

use super::*;
use bn_core::links::check::{self, Health, Probe};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// How many probes run at once. Each is bounded by its own timeout, so a big store costs about
/// `links / WORKERS × 5s` at worst.
const WORKERS: usize = 8;

/// `bn links check [--tag T]`.
pub(super) fn check_links(cfg: &Config, tag: Option<String>, json: bool) {
    let items = links::read(cfg);
    let here = current_repo(cfg);
    let targets: Vec<Link> = with_tag(
        links::ranked(&items, here.as_deref(), &today()),
        tag.as_deref(),
    )
    .into_iter()
    .cloned()
    .collect();
    let found = run_all(&targets);
    if json {
        let data: Vec<_> = targets
            .iter()
            .zip(&found)
            .map(|(l, h)| {
                let mut v = link_json(l);
                v["health"] = serde_json::json!(h.as_str());
                v
            })
            .collect();
        println!(
            "{}",
            Envelope::success("links", serde_json::json!({ "items": data })).to_json()
        );
        return;
    }
    if targets.is_empty() {
        println!("No links to check.");
        return;
    }
    for (l, h) in targets.iter().zip(&found) {
        println!("{:<8} {:<40} {}", h.as_str(), truncate(&l.name, 40), l.url);
    }
    let summary: Vec<String> = [
        Health::Ok,
        Health::Merged,
        Health::Closed,
        Health::Dead,
        Health::SignIn,
        Health::Unknown,
    ]
    .iter()
    .filter_map(|h| {
        let n = found.iter().filter(|f| *f == h).count();
        (n > 0).then(|| format!("{n} {}", h.as_str()))
    })
    .collect();
    println!("\n{}", summary.join(" · "));
}

/// Probe every link, `WORKERS` at a time, keeping the input order in the result.
fn run_all(targets: &[Link]) -> Vec<Health> {
    let queue: Arc<Mutex<VecDeque<(usize, String)>>> = Arc::new(Mutex::new(
        targets.iter().map(|l| l.url.clone()).enumerate().collect(),
    ));
    let found = Arc::new(Mutex::new(vec![Health::Unknown; targets.len()]));
    let workers: Vec<_> = (0..WORKERS.min(targets.len()))
        .map(|_| {
            let queue = Arc::clone(&queue);
            let found = Arc::clone(&found);
            std::thread::spawn(move || loop {
                let next = queue.lock().ok().and_then(|mut q| q.pop_front());
                let Some((i, url)) = next else { break };
                let h = probe(&url);
                if let Ok(mut f) = found.lock() {
                    f[i] = h;
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    Arc::try_unwrap(found)
        .ok()
        .and_then(|m| m.into_inner().ok())
        .unwrap_or_default()
}

fn probe(url: &str) -> Health {
    match check::probe_for(url) {
        Probe::Github(path) => github(&path),
        Probe::Http => http(url),
    }
}

/// `gh api` for a PR, issue or release. An issue has no `merged` and a release no `state`,
/// hence the defaults.
fn github(path: &str) -> Health {
    let Ok(out) = Command::new("gh")
        .args(["api", path, "--jq", "[(.state // \"exists\"), (.merged // false)] | @tsv"])
        .output()
    else {
        return Health::Unknown;
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let not_found = err.contains("HTTP 404") || err.contains("Not Found");
        return check::classify_github(None, false, not_found);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.trim().split('\t');
    let state = fields.next();
    let merged = fields.next() == Some("true");
    check::classify_github(state, merged, false)
}

/// A GET (not HEAD: some servers refuse HEAD and would read as dead) following redirects, with
/// the body thrown away. curl prints `000` when the request never completed.
fn http(url: &str) -> Health {
    let Ok(out) = Command::new("curl")
        .args([
            "-sS",
            "-L",
            "-o",
            "/dev/null",
            "--max-time",
            "5",
            "-w",
            "%{http_code} %{url_effective}",
            url,
        ])
        .output()
    else {
        return Health::Unknown;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let (code, effective) = text.trim().split_once(' ').unwrap_or((text.trim(), ""));
    let status = code.parse::<u16>().ok().filter(|c| *c != 0);
    check::classify_http(status, (!effective.is_empty()).then_some(effective))
}
