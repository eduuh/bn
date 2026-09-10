use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-env-changed=BN_RELEASE_VERSION");
    track_git_head();

    println!("cargo:rustc-env=BN_VERSION={}", release_version());
    println!("cargo:rustc-env=BN_GIT_SHA={}", git_short_sha());
    println!("cargo:rustc-env=BN_BUILD_DATE={}", build_date());
    println!(
        "cargo:rustc-env=BN_BUILD_TARGET={}",
        env::var("TARGET").unwrap_or_else(|_| "unknown".into())
    );
}

/// The version the binary reports. The git tag is the source of truth for a release, not a
/// commit: the release workflow computes the next version and passes it in as
/// `BN_RELEASE_VERSION`, so CI never has to push a version-bump commit back to main (which would
/// re-trigger CI and release itself, forever). `Cargo.toml`'s workspace version is only the
/// *floor* — bump it in an ordinary PR to cut a new minor or major; patches flow from the tags.
/// A local build has no `BN_RELEASE_VERSION` and reports that floor, as it always did.
fn release_version() -> String {
    env::var("BN_RELEASE_VERSION")
        .ok()
        .filter(|version| !version.is_empty())
        .or_else(|| env::var("CARGO_PKG_VERSION").ok())
        .unwrap_or_else(|| "0.0.0".into())
}

fn git_short_sha() -> String {
    env::var("GITHUB_SHA")
        .ok()
        .filter(|sha| !sha.is_empty())
        .map(|sha| sha.chars().take(12).collect())
        .or_else(|| git_output(&["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".into())
}

fn build_date() -> String {
    let epoch_seconds = env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_secs())
        })
        .unwrap_or(0);
    civil_date(epoch_seconds / 86_400)
}

fn civil_date(days_since_epoch: u64) -> String {
    let z = days_since_epoch as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn track_git_head() {
    if let Some(head) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={}", head.display());
    }
    if let Some(reference) = git_output(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git_path(&reference) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn git_path(name: &str) -> Option<PathBuf> {
    git_output(&["rev-parse", "--git-path", name]).map(|path| {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            manifest_dir().join(path)
        }
    })
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest_dir())
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn manifest_dir() -> PathBuf {
    env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(".").to_path_buf())
}
