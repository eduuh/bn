//! The one-time move of a machine's old `config.toml` into the notes tree.
//!
//! bn used to read a single `<config_dir>/config.toml` that each machine kept for itself, and that
//! named machines in `[machines.<name>]` tables. Settings now live in the synced notes tree: a
//! shared `<notes_root>/config.toml`, plus `<notes_root>/<machine>/config.toml` for what one machine
//! does differently. The first `bn` after the upgrade moves the old file across, keeps it as a
//! backup and says what moved, so nobody has to remember a migration step.
//!
//! It runs once per machine because it starts by renaming the old file out of the way: a second
//! run, or a second `bn` racing the first, finds nothing to move.

use crate::resolver;
use std::path::{Path, PathBuf};

/// What a migration did: the one-line notice, and the report file written beside the backup.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Where the old file was.
    pub legacy: PathBuf,
    /// Where the old file is now.
    pub backup: PathBuf,
    /// The shared file written, if this machine was the first to migrate.
    pub shared: Option<PathBuf>,
    /// This machine's file, if it has anything to say.
    pub machine: Option<PathBuf>,
    /// One line per setting that did not land where its name suggests.
    pub notes: Vec<String>,
}

impl Report {
    /// The full account, for `<config_dir>/config-migration.txt`.
    pub fn render(&self) -> String {
        let mut out = format!(
            "bn moved {} into the notes tree.\nold file kept at: {}\n",
            self.legacy.display(),
            self.backup.display()
        );
        if let Some(shared) = &self.shared {
            out.push_str(&format!("shared by every machine: {}\n", shared.display()));
        }
        if let Some(machine) = &self.machine {
            out.push_str(&format!("this machine only: {}\n", machine.display()));
        }
        for note in &self.notes {
            out.push_str(&format!("- {note}\n"));
        }
        out
    }
}

/// Sections that describe one machine rather than the person, so they go to the machine file:
/// what it may run, its capture folders, and which harnesses it can use.
const MACHINE_SECTIONS: &[&str] = &["machine", "capture", "harness"];

/// Every top-level section bn still reads. Anything else in an old file belonged to a feature that
/// is gone, and is dropped with a note rather than carried forward to mislead the next reader.
/// Kept equal to `ConfigFile`'s fields by a test in the resolver.
pub const KNOWN_SECTIONS: &[&str] = &[
    "agents",
    "attention",
    "calib",
    "capture",
    "dispatch",
    "dxi",
    "forge_agent",
    "harness",
    "hosts",
    "jobs",
    "links",
    "log",
    "machine",
    "managed_repos",
    "mcp",
    "nodes",
    "notify",
    "orchestrate",
    "pipeline",
    "pool",
    "proof",
    "repo",
    "review",
    "scout",
    "staff",
    "standup",
    "tasks",
    "work",
    "wrap",
];

/// Migrate this machine's old config, if it has one. `None` when there is nothing to do, which is
/// every run after the first. Skipped entirely when `$BN_CONFIG` names a file: that is a sandbox
/// saying exactly which config to read.
pub fn run() -> Option<Report> {
    if std::env::var_os("BN_CONFIG").is_some() {
        return None;
    }
    let relocated = relocate_legacy_config_home();
    let legacy_dir = resolver::config_dir();
    let legacy = legacy_dir.join("config.toml");
    std::fs::symlink_metadata(&legacy).ok()?;
    let text = std::fs::read_to_string(&legacy).unwrap_or_default();
    let backup = claim(&legacy)?;
    let mut report = migrate_text(
        &text,
        &resolver::config_root(),
        &legacy_dir.join("work-repos"),
    );
    report.legacy = legacy;
    report.backup = backup;
    if let Some(moved) = relocated {
        report.notes.insert(0, moved);
    }
    let _ = std::fs::write(legacy_dir.join("config-migration.txt"), report.render());
    Some(report)
}

/// Move the old config's settings into the layers under `root`. Pure apart from the files it
/// writes, so it is testable without a real machine; [`run`] supplies the paths and the backup.
pub fn migrate_text(text: &str, root: &Path, work_repos_file: &Path) -> Report {
    let mut report = Report::default();
    let mut table = match text.parse::<toml::Table>() {
        Ok(table) => table,
        Err(error) => {
            report.notes.push(format!(
                "the old file was not valid TOML ({error}), so nothing moved; it is kept as the backup"
            ));
            return report;
        }
    };

    // The machine name first: an old `hostname` key *was* this machine's identity, and moving its
    // settings under any other name would split its state in two.
    let machine = match table
        .remove("hostname")
        .and_then(|v| v.as_str().map(str::to_string))
    {
        Some(name) if std::env::var_os("BN_HOSTNAME").is_none() => {
            let _ = std::fs::write(resolver::machine_file_path(), format!("{name}\n"));
            report.notes.push(format!(
                "hostname = \"{name}\" is now pinned in {}",
                resolver::machine_file_path().display()
            ));
            name
        }
        _ => resolver::machine_name(),
    };

    let mut machine_layer = toml::Table::new();
    translate(&mut table, &machine, &mut machine_layer, &mut report.notes);
    fold_work_repos(&mut table, work_repos_file, &mut report.notes);
    for section in MACHINE_SECTIONS {
        if let Some(value) = table.remove(*section) {
            resolver::merge_tables(
                &mut machine_layer,
                toml::Table::from_iter([(section.to_string(), value)]),
            );
        }
    }

    let shared_path = root.join("config.toml");
    let machine_path = root.join(&machine).join("config.toml");
    match read_table(&shared_path) {
        // Another machine migrated first. Its shared file stays exactly as written, and this
        // machine carries only the settings where it disagrees.
        Some(existing) => {
            resolver::merge_tables(&mut machine_layer, difference(table, &existing));
        }
        None if !table.is_empty() => match write_layer(&shared_path, SHARED_HEADER, &table) {
            Ok(()) => report.shared = Some(shared_path),
            Err(error) => report.notes.push(format!(
                "could not write {} ({error}); the settings are still in the backup",
                shared_path.display()
            )),
        },
        None => {}
    }
    if let Some(existing) = read_table(&machine_path) {
        resolver::merge_tables(&mut machine_layer, existing);
    }
    if !machine_layer.is_empty() {
        let header = format!(
            "# bn settings for {machine} only. A key here overrides the shared ../config.toml,\n\
             # and bn records this machine's own choices here.\n\n"
        );
        match write_layer(&machine_path, &header, &machine_layer) {
            Ok(()) => report.machine = Some(machine_path),
            Err(error) => report.notes.push(format!(
                "could not write {} ({error}); the settings are still in the backup",
                machine_path.display()
            )),
        }
    }
    report
}

const SHARED_HEADER: &str = "# bn settings shared by every machine that syncs this notes tree.\n\
# A setting left out uses bn's built-in default, and docs/config.md lists every one. Put what a\n\
# single machine does differently in <machine>/config.toml beside this file.\n\n";

/// Rewrite the old file's names into the ones bn reads now, moving machine-specific pieces into
/// `machine_layer` and noting everything that changed shape or went away.
fn translate(
    table: &mut toml::Table,
    machine: &str,
    machine_layer: &mut toml::Table,
    notes: &mut Vec<String>,
) {
    if let Some(toml::Value::Table(mut machines)) = table.remove("machines") {
        if let Some(profile) = machines.remove(machine) {
            machine_layer.insert("machine".to_string(), profile);
            notes.push(format!(
                "[machines.\"{machine}\"] is now [machine] in this machine's file"
            ));
        }
        for other in machines.keys() {
            notes.push(format!(
                "[machines.\"{other}\"] was dropped: it describes another machine, and belongs in \
                 {other}/config.toml as [machine] if it still matters"
            ));
        }
    }

    if let Some(value) = table.remove("worktree_dir") {
        notes.push(format!(
            "worktree_dir = {value} is no longer a setting: bn uses ~/projects/worktree, or \
             $BN_WORKTREE_DIR when set"
        ));
    }
    if let Some(toml::Value::Table(personal)) = table.remove("personal") {
        for (key, value) in personal {
            notes.push(format!(
                "[personal].{key} = {value} is no longer a setting: bn reads the notes tree at its \
                 default path, or $BN_NOTES_DIR / $BN_OVERLAY_DIR when set"
            ));
        }
    }
    let mut work_emptied = false;
    if let Some(toml::Value::Table(work)) = table.get_mut("work") {
        for key in ["notes_root", "only"] {
            if let Some(value) = work.remove(key) {
                notes.push(format!(
                    "[work].{key} = {value} is no longer a setting: bn uses the default work notes \
                     tree, and treats a machine with no personal notes tree as work-only"
                ));
            }
        }
        work_emptied = work.is_empty();
    }
    if work_emptied {
        table.remove("work");
    }

    if let Some(toml::Value::Table(windows)) = table.remove("windows") {
        if cfg!(windows) {
            resolver::merge_tables(machine_layer, windows);
            notes.push("[windows] now lives in this machine's file".to_string());
        } else {
            notes.push(
                "[windows] was dropped: it only applied on native Windows, where it now belongs \
                 in that machine's own config.toml"
                    .to_string(),
            );
        }
    }

    let mut renamed = rename_pipeline(table.get_mut("pipeline"));
    if let Some(toml::Value::Table(repos)) = table.get_mut("repo") {
        for repo in repos.iter_mut().map(|(_, value)| value) {
            if let toml::Value::Table(repo) = repo {
                renamed |= rename_pipeline(repo.get_mut("pipeline"));
            }
        }
    }
    if renamed {
        notes.push(
            "pipeline `models` tables are now `stages`, and a stage's `agent` is now `harness`"
                .to_string(),
        );
    }

    if let Some(toml::Value::Table(switch)) = table.remove("attention_agent") {
        if switch.get("enabled").and_then(toml::Value::as_bool) == Some(true) {
            let agents = table
                .entry("agents")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(agents) = agents {
                let enabled = agents
                    .entry("enabled")
                    .or_insert_with(|| toml::Value::Array(Vec::new()));
                if let toml::Value::Array(list) = enabled {
                    let name = toml::Value::String("attention-agent".to_string());
                    if !list.contains(&name) {
                        list.push(name);
                    }
                }
            }
            notes.push(
                "[attention_agent] enabled = true is now \"attention-agent\" in [agents].enabled"
                    .to_string(),
            );
        }
    }

    let unknown: Vec<String> = table
        .keys()
        .filter(|key| !KNOWN_SECTIONS.contains(&key.as_str()))
        .cloned()
        .collect();
    for key in unknown {
        table.remove(&key);
        notes.push(format!(
            "[{key}] was dropped: nothing in bn reads it any more"
        ));
    }
}

/// `[pipeline.models]` → `[pipeline.stages]`, and each stage's `agent` → `harness`. Whether
/// anything changed, so the note is written once rather than per stage.
fn rename_pipeline(pipeline: Option<&mut toml::Value>) -> bool {
    let Some(toml::Value::Table(pipeline)) = pipeline else {
        return false;
    };
    let mut renamed = false;
    if let Some(models) = pipeline.remove("models") {
        pipeline.entry("stages").or_insert(models);
        renamed = true;
    }
    if let Some(toml::Value::Table(stages)) = pipeline.get_mut("stages") {
        for stage in stages.iter_mut().map(|(_, value)| value) {
            if let toml::Value::Table(stage) = stage {
                if let Some(agent) = stage.remove("agent") {
                    stage.entry("harness").or_insert(agent);
                    renamed = true;
                }
            }
        }
    }
    renamed
}

/// The plaintext `work-repos` allowlist becomes `[work].work_repos`, unless the config already
/// says. The file is moved aside either way, since nothing reads it now.
fn fold_work_repos(table: &mut toml::Table, file: &Path, notes: &mut Vec<String>) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let repos: Vec<toml::Value> = text
        .lines()
        .map(|line| {
            line.split('#')
                .next()
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        })
        .filter(|repo| !repo.is_empty())
        .map(toml::Value::String)
        .collect();
    let has_list = table
        .get("work")
        .and_then(|work| work.get("work_repos"))
        .is_some();
    if !repos.is_empty() && !has_list {
        let work = table
            .entry("work")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(work) = work {
            work.insert("work_repos".to_string(), toml::Value::Array(repos));
        }
        notes.push(format!("{} is now [work].work_repos", file.display()));
    }
    let _ = std::fs::rename(file, file.with_file_name("work-repos.bak"));
}

/// The keys of `candidate` whose value `existing` does not already have, recursing into tables.
fn difference(candidate: toml::Table, existing: &toml::Table) -> toml::Table {
    let mut out = toml::Table::new();
    for (key, value) in candidate {
        match (value, existing.get(&key)) {
            (toml::Value::Table(child), Some(toml::Value::Table(existing_child))) => {
                let child = difference(child, existing_child);
                if !child.is_empty() {
                    out.insert(key, toml::Value::Table(child));
                }
            }
            (value, Some(existing_value)) if &value == existing_value => {}
            (value, _) => {
                out.insert(key, value);
            }
        }
    }
    out
}

/// Rename the old file to its backup, which is also the lock: `None` when another `bn` got there
/// first, or when the rename cannot be done and it is safer to leave everything alone.
fn claim(legacy: &Path) -> Option<PathBuf> {
    let mut backup = legacy.with_file_name("config.toml.bak");
    if std::fs::symlink_metadata(&backup).is_ok() {
        backup =
            legacy.with_file_name(format!("config.toml.bak.{}", crate::util::now_epoch_secs()));
    }
    match std::fs::rename(legacy, &backup) {
        Ok(()) => Some(backup),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!(
                "bn: could not move {} aside to migrate it ({error}); it is no longer read",
                legacy.display()
            );
            None
        }
    }
}

fn read_table(path: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(path).ok()?.parse().ok()
}

fn write_layer(path: &Path, header: &str, table: &toml::Table) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(table).map_err(std::io::Error::other)?;
    crate::store::write_atomic(path, format!("{header}{body}").as_bytes())
}

/// Machines from before the `~/.bn` layout kept everything in a real `~/.config/bn` directory.
/// Move it to `~/.bn/config` and leave `~/.config/bn` pointing there — the layout install.sh has
/// made ever since — so the resolver has one place to look. Unix only: native Windows keeps its
/// platform config directory.
fn relocate_legacy_config_home() -> Option<String> {
    if !cfg!(unix)
        || std::env::var_os("BN_CONFIG_DIR").is_some()
        || std::env::var_os("BN_HOME").is_some()
    {
        return None;
    }
    let home = dirs::home_dir()?;
    let old = home.join(".config").join("bn");
    let new = home.join(".bn").join("config");
    let is_real_dir = std::fs::symlink_metadata(&old).is_ok_and(|meta| meta.is_dir());
    if !is_real_dir || std::fs::symlink_metadata(&new).is_ok() {
        return None;
    }
    std::fs::create_dir_all(new.parent()?).ok()?;
    std::fs::rename(&old, &new).ok()?;
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&new, &old);
    Some(format!(
        "{} moved to {}, with a link left behind",
        old.display(),
        new.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> toml::Value {
        toml::Value::Table(
            std::fs::read_to_string(path)
                .expect("layer written")
                .parse()
                .expect("layer is valid TOML"),
        )
    }

    fn with_machine<T>(name: &str, f: impl FnOnce() -> T) -> T {
        let _lock = crate::test_support::env_lock();
        let _host = crate::test_support::EnvGuard::set("BN_HOSTNAME", name);
        f()
    }

    #[test]
    fn the_first_machine_splits_its_file_into_shared_and_machine_layers() {
        let root = tempfile::tempdir().expect("tempdir");
        let report = with_machine("mac", || {
            migrate_text(
                "worktree_dir = \"/w\"\n\
                 [review]\nharness = \"claude\"\n\
                 [capture]\nscreenshot_dir = \"/shots\"\n\
                 [machines.mac]\nrepos = [\"*\"]\ncron = true\n\
                 [machines.old-laptop]\nweb = false\n",
                root.path(),
                &root.path().join("no-work-repos"),
            )
        });

        let shared = read(&root.path().join("config.toml"));
        let machine = read(&root.path().join("mac").join("config.toml"));
        assert_eq!(shared["review"]["harness"].as_str(), Some("claude"));
        assert!(shared.get("capture").is_none() && shared.get("machines").is_none());
        assert_eq!(
            machine["capture"]["screenshot_dir"].as_str(),
            Some("/shots")
        );
        assert_eq!(machine["machine"]["cron"].as_bool(), Some(true));
        let notes = report.notes.join("\n");
        assert!(notes.contains("worktree_dir"), "{notes}");
        assert!(notes.contains("old-laptop"), "{notes}");
    }

    #[test]
    fn a_later_machine_keeps_the_shared_file_and_carries_only_its_differences() {
        let root = tempfile::tempdir().expect("tempdir");
        let shared_path = root.path().join("config.toml");
        let shared_before = "[review]\nharness = \"claude\"\n";
        std::fs::write(&shared_path, shared_before).expect("seed shared");

        with_machine("fedora", || {
            migrate_text(
                "[review]\nharness = \"claude\"\n[pool]\nsize = 9\n",
                root.path(),
                &root.path().join("no-work-repos"),
            )
        });

        assert_eq!(
            std::fs::read_to_string(&shared_path).expect("shared kept"),
            shared_before
        );
        let machine = read(&root.path().join("fedora").join("config.toml"));
        assert_eq!(machine["pool"]["size"].as_integer(), Some(9));
        assert!(
            machine.get("review").is_none(),
            "an agreeing value stays shared"
        );
    }

    #[test]
    fn old_names_are_rewritten_to_the_ones_bn_reads() {
        let root = tempfile::tempdir().expect("tempdir");
        let report = with_machine("mac", || {
            migrate_text(
                "[pipeline.models.plan]\nmodel = \"m\"\nagent = \"claude\"\n\
                 [attention_agent]\nenabled = true\n\
                 [night]\nenabled = false\n",
                root.path(),
                &root.path().join("no-work-repos"),
            )
        });

        let shared = read(&root.path().join("config.toml"));
        assert_eq!(
            shared["pipeline"]["stages"]["plan"]["harness"].as_str(),
            Some("claude")
        );
        assert!(shared["pipeline"].get("models").is_none());
        assert_eq!(
            shared["agents"]["enabled"].as_array().map(|a| a.len()),
            Some(1)
        );
        assert!(shared.get("night").is_none());
        assert!(report.notes.iter().any(|n| n.contains("[night]")));
    }

    #[test]
    fn the_work_repos_file_becomes_a_setting_and_is_moved_aside() {
        let root = tempfile::tempdir().expect("tempdir");
        let file = root.path().join("work-repos");
        std::fs::write(&file, "# generated\nacme\n  internal-tool # note\n").expect("seed");

        with_machine("mac", || {
            migrate_text("[review]\nharness = \"copilot\"\n", root.path(), &file)
        });

        let shared = read(&root.path().join("config.toml"));
        assert_eq!(
            shared["work"]["work_repos"].as_array().map(|a| a.len()),
            Some(2)
        );
        assert!(!file.exists());
        assert!(root.path().join("work-repos.bak").exists());
    }

    #[test]
    fn an_unparseable_file_moves_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        let report = with_machine("mac", || {
            migrate_text("[review\n", root.path(), &root.path().join("none"))
        });

        assert!(report.shared.is_none() && report.machine.is_none());
        assert!(!root.path().join("config.toml").exists());
        assert!(report.notes[0].contains("not valid TOML"));
    }

    #[test]
    fn claiming_twice_finds_nothing_the_second_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy = dir.path().join("config.toml");
        std::fs::write(&legacy, "[review]\n").expect("seed");

        let backup = claim(&legacy).expect("first claim moves the file");
        assert!(backup.exists() && !legacy.exists());
        assert!(claim(&legacy).is_none());
    }
}
