use super::*;
use crate::model::Note;
use crate::resolver::{Config, RepoBranch, Resolved};

fn setup(temp: &std::path::Path) -> (Config, Resolved, std::path::PathBuf) {
    let cfg = crate::test_support::config(temp);
    let ctx = Resolved {
        repo: "bn".into(),
        branch: "feat".into(),
    };
    std::fs::create_dir_all(cfg.note_dir(&ctx)).unwrap();
    let note = Note::new(
        RepoBranch {
            repo: "bn",
            branch: "feat",
        },
        "2026-09-25",
        "testhost",
    );
    crate::store::save(&note, &cfg.note_file(&ctx)).unwrap();
    crate::commands::set_goal(&cfg, &ctx, "ship profiles", "2026-09-25").unwrap();
    crate::commands::add(
        &cfg,
        &ctx,
        "directive",
        "never touch vendor/",
        None,
        "2026-09-25",
    )
    .unwrap();
    let root = temp.join("repo");
    std::fs::create_dir_all(root.join(".bn")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("docs/ARCH.md"),
        "# Arch\n\n## Overview\nthree crates\n\n## Details\nlots\n",
    )
    .unwrap();
    (cfg, ctx, root)
}

// `-` keeps tests off the person's real standards store.
const NO_GLOBAL: &[String] = &[];

fn inputs<'a>(
    cfg: &'a Config,
    ctx: &'a Resolved,
    root: &'a std::path::Path,
    g: &'a [String],
) -> AtomInputs<'a> {
    AtomInputs {
        cfg,
        ctx,
        root: Some(root),
        global_refs: g,
    }
}

#[test]
fn a_manifest_declares_atoms_and_profiles_and_the_local_file_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let (_, _, root) = setup(temp.path());
    std::fs::write(
        root.join(".bn/context.toml"),
        "[atoms.arch]\nkind = \"file\"\npath = \"docs/ARCH.md#Overview\"\n\n\
         [profiles.review]\natoms = [\"goal\", \"arch\"]\nbudget = 100\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".bn/context.local.toml"),
        "[profiles.review]\natoms = [\"arch\"]\n",
    )
    .unwrap();
    let m = load(Some(&root)).unwrap();
    assert_eq!(m.profile("review").unwrap().atoms, ["arch"]);
    assert!(m.profile("ship").is_some(), "built-ins stay available");
    assert!(m.profile_names().contains(&"review".to_string()));
    std::fs::write(root.join(".bn/context.toml"), "[profiles.x\n").unwrap();
    assert!(load(Some(&root)).is_err());
}

#[test]
fn composing_renders_in_order_and_counts_tokens() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx, root) = setup(temp.path());
    std::fs::write(
        root.join(".bn/context.toml"),
        "[atoms.arch]\nkind = \"file\"\npath = \"docs/ARCH.md#Overview\"\n\
         [profiles.p]\natoms = [\"directives\", \"goal\", \"recap\", \"arch\", \"nope\"]\n",
    )
    .unwrap();
    let m = load(Some(&root)).unwrap();
    let g = vec!["-".to_string()];
    let c = compose(&m, "p", &inputs(&cfg, &ctx, &root, &g), None, false).unwrap();
    let atoms: Vec<&str> = c.pieces.iter().map(|p| p.atom.as_str()).collect();
    assert_eq!(atoms, ["directives", "goal", "arch"]);
    assert_eq!(c.empty, ["recap"]);
    assert_eq!(c.missing, ["nope: no such atom"]);
    let text = c.text();
    assert!(
        text.contains("never touch vendor/")
            && text.contains("Goal for this branch: ship profiles")
    );
    assert!(text.contains("three crates") && !text.contains("lots"));
    assert_eq!(c.total, c.pieces.iter().map(|p| p.tokens).sum::<usize>());
    assert_eq!((c.budget, c.overage), (None, 0));
}

#[test]
fn over_budget_warns_and_fit_drops_from_the_end() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx, root) = setup(temp.path());
    let m = load(Some(&root)).unwrap();
    let g = vec!["-".to_string()];
    let i = inputs(&cfg, &ctx, &root, &g);
    let full = compose(&m, "orient", &i, Some(5), false).unwrap();
    assert!(full.overage > 0 && full.dropped.is_empty());
    let fit = compose(&m, "orient", &i, Some(full.pieces[0].tokens), true).unwrap();
    assert_eq!(fit.pieces.len(), 1);
    assert_eq!(fit.overage, 0);
    assert!(!fit.dropped.is_empty());
    assert!(compose(&m, "nope", &i, None, false).is_err());
    let _ = NO_GLOBAL;
}

#[test]
fn file_atoms_stay_inside_the_repo_and_name_missing_sections() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx, root) = setup(temp.path());
    let g = vec!["-".to_string()];
    let i = inputs(&cfg, &ctx, &root, &g);
    let file = |p: &str| render_atom(&Atom::File { path: p.into() }, &i);
    assert!(file("../secret").is_err());
    assert!(file("/etc/passwd").is_err());
    assert!(file("docs/ARCH.md#Nope")
        .unwrap_err()
        .contains("no section"));
    assert!(file("docs/ARCH.md").unwrap().unwrap().contains("lots"));
    assert_eq!(
        render_atom(&Atom::GlobalRefs, &i).unwrap(),
        None,
        "`-` attaches none"
    );
}

#[test]
fn serving_and_reading_are_separate_facts() {
    let temp = tempfile::tempdir().unwrap();
    let (cfg, ctx, _) = setup(temp.path());
    let d = served::record(
        &cfg,
        &ctx,
        "ship",
        &["directives".into()],
        "hello",
        "dispatch",
    )
    .unwrap();
    assert_eq!(d, served::digest("hello"));
    assert_ne!(d, served::digest("hello!"));
    let s = served::list(&cfg, &ctx);
    assert_eq!((s.len(), s[0].acked_by.clone()), (1, None));
    assert!(served::ack(&cfg, &ctx, "ffffffffffff", "bn:feat").is_err());
    served::ack(&cfg, &ctx, &d, "bn:feat").unwrap();
    assert_eq!(
        served::list(&cfg, &ctx)[0].acked_by.as_deref(),
        Some("bn:feat")
    );
}
