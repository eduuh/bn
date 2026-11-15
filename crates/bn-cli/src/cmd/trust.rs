//! Harness folder-trust settings shared by agent launchers and `bn trust-folder`.
use crate::*;

/// Apply harness folder trust for `path` (claude | copilot | both).
/// Returns which harnesses were trusted; a missing config is not fatal.
pub(crate) fn apply_folder_trust(path: &str, harness: &str) -> Vec<&'static str> {
    let home = std::env::var("HOME").unwrap_or_default();
    let want = |h: &str| harness == "both" || harness == h;
    let mut trusted: Vec<&'static str> = Vec::new();
    if want("claude") {
        let p = format!("{home}/.claude.json");
        let cur = std::fs::read_to_string(&p).unwrap_or_default();
        match bn_core::trust::claude_trust(&cur, path) {
            Ok(new) => {
                if new != cur {
                    if let Err(e) = std::fs::write(&p, new) {
                        eprintln!("bn trust-folder: writing {p}: {e}");
                    }
                }
                trusted.push("claude");
            }
            Err(e) => eprintln!("bn trust-folder: {e}"),
        }
    }
    if want("copilot") {
        let p = format!("{home}/.copilot/config.json");
        let cur = std::fs::read_to_string(&p).unwrap_or_default();
        match bn_core::trust::copilot_trust(&cur, path) {
            Ok((new, changed)) => {
                if changed {
                    if let Err(e) = std::fs::write(&p, new) {
                        eprintln!("bn trust-folder: writing {p}: {e}");
                    }
                }
                trusted.push("copilot");
            }
            Err(e) => eprintln!("bn trust-folder: {e}"),
        }
    }
    trusted
}

pub(crate) fn emit_trust_folder(path: Option<String>, harness: &str, as_json: bool) {
    let path = path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    // Only persist absolute paths: a nonexistent relative path could be a misplaced harness name.
    if !path.is_absolute() {
        let msg = format!(
            "bn trust-folder: '{}' is not an absolute folder path — pass a real directory (folder trust only accepts absolute paths; the harness goes in --harness)",
            path.display()
        );
        if as_json {
            println!(
                "{}",
                Envelope::failure("trust_folder", "not_absolute", msg).to_json()
            );
            return;
        }
        fail(&msg);
    }
    let path = path.to_string_lossy().to_string();
    let trusted = apply_folder_trust(&path, harness);

    if as_json {
        let env = Envelope::success(
            "trust_folder",
            serde_json::json!({ "path": path, "harnesses": trusted }),
        );
        println!("{}", env.to_json());
    } else {
        println!("trusted {path} for: {}", trusted.join(", "));
    }
}
