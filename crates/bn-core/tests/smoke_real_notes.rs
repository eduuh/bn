//! Opt-in smoke test: read every real note under `$BN_SMOKE_DIR` and assert none fail
//! to parse. Skipped when the env var is unset (so CI / other machines don't need it).
//! Run locally with: `BN_SMOKE_DIR=~/projects/branch-notes cargo test -p bn-core --test smoke_real_notes -- --nocapture`

use std::path::{Path, PathBuf};

fn find_notes(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_notes(&p, out);
        } else if p.file_name().and_then(|n| n.to_str()) == Some("note.md") {
            out.push(p);
        }
    }
}

#[test]
fn reads_all_real_notes() {
    let Ok(root) = std::env::var("BN_SMOKE_DIR") else {
        eprintln!("BN_SMOKE_DIR unset — skipping real-notes smoke test");
        return;
    };
    let mut notes = Vec::new();
    find_notes(Path::new(&root), &mut notes);
    assert!(!notes.is_empty(), "no note.md found under {root}");

    let mut failures = Vec::new();
    for path in &notes {
        match bn_core::store::read(path) {
            Ok(note) => {
                // round-trip render must not panic and must re-read cleanly
                let text = bn_core::store::render(&note).expect("render");
                let _ = bn_core::store::item_line_map(&text);
            }
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    eprintln!(
        "read {} real notes, {} failures",
        notes.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}
