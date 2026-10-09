//! `bn cs connect` / `bn cs disconnect`: a GitHub Codespace as an ordinary ssh host.
//!
//! `gh cs ssh --config -c <name>` prints a Host stanza whose ProxyCommand is `gh cs ssh --stdio`.
//! bn writes it into `~/.ssh/config` as a managed `# >>> bn codespace <alias> >>>` block, so every
//! alias-based command (`bn connect`, `bn agent launch --host`, `bn edit remote`, plain `ssh`)
//! works on the codespace. Three changes to gh's stanza:
//! - the Host is renamed to a short alias (`cs-<word>`);
//! - the ProxyCommand runs under `GODEBUG=netdns=cgo`, because Go's own resolver can pick a NAT64
//!   IPv6 address on WSL and fail with "network is unreachable";
//! - `ControlPath`/`ControlPersist` are added (gh sets `ControlMaster auto` without a path, so every
//!   connect paid the ~10s gh handshake) and `ConnectTimeout` is raised for a slow cold start.
//!
//! Connecting never wakes a stopped codespace. Both an ssh and `gh cs ssh --config` start one, so
//! for a codespace that is not `Available` the block is built from gh's fixed template and no
//! connection check runs. `bn tunnels` lists these blocks.

use std::path::PathBuf;
use std::process::Command;

const MARK_OPEN: &str = "# >>> bn codespace ";
const MARK_CLOSE: &str = "# <<< bn codespace ";
const NAME_LINE: &str = "# bn: codespace ";

fn ssh_dir() -> PathBuf {
    match std::env::var("BN_SSH_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::home_dir()
            .unwrap_or_else(|| crate::fail("bn cs: could not determine the home directory"))
            .join(".ssh"),
    }
}

/// Default alias: `cs-` + the last word of the codespace's display name, else of its name
/// (`opulent umbrella` → `cs-umbrella`, `bug-free potato` → `cs-potato`). Lowercased; only
/// characters `safe_ssh_host` accepts.
pub(crate) fn default_alias(name: &str, display_name: &str) -> String {
    let src = if display_name.trim().is_empty() {
        name
    } else {
        display_name
    };
    let words: Vec<&str> = src
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit()))
        .collect();
    // A generated name ends in a random id (`…-v4rgjr95jvphpwvg`); skip a long mixed tail.
    let pick = words
        .iter()
        .rev()
        .find(|w| !(w.len() >= 12 && w.chars().any(|c| c.is_ascii_digit())))
        .copied()
        .unwrap_or("codespace");
    format!("cs-{}", pick.to_ascii_lowercase())
}

/// The managed block for `alias`, built from `gh cs ssh --config` output (pure; unit-tested).
pub(crate) fn render_block(alias: &str, codespace: &str, gh_config: &str) -> String {
    let mut lines = vec![
        format!("{MARK_OPEN}{alias} >>>"),
        format!("{NAME_LINE}{codespace}"),
    ];
    let mut body: Vec<String> = Vec::new();
    for raw in gh_config.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let t = line.trim_start();
        if t.to_ascii_lowercase().starts_with("host ") {
            body.push(format!("Host {alias}"));
        } else if t.to_ascii_lowercase().starts_with("proxycommand ") && !t.contains("GODEBUG=") {
            let rest = t["ProxyCommand ".len()..].trim_start();
            body.push(format!("\tProxyCommand env GODEBUG=netdns=cgo {rest}"));
        } else {
            body.push(format!("\t{t}"));
        }
    }
    for (key, line) in [
        ("ControlPath", "\tControlPath ~/.ssh/cm-%C"),
        ("ControlPersist", "\tControlPersist 10m"),
        ("ConnectTimeout", "\tConnectTimeout 120"),
    ] {
        if !has_key(&body, key) {
            body.push(line.to_string());
        }
    }
    lines.extend(body);
    lines.push(format!("{MARK_CLOSE}{alias} <<<"));
    lines.join("\n") + "\n"
}

fn has_key(body: &[String], key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    body.iter()
        .any(|l| l.trim_start().to_ascii_lowercase().starts_with(&key))
}

/// What `gh cs ssh --config -c <name>` prints, for a codespace that must not be started (pure).
/// gh's stanza is fixed apart from the name: user `vscode`, a `gh cs ssh --stdio` proxy and the
/// `codespaces.auto` key gh creates on first use.
pub(crate) fn gh_template(name: &str, gh: &str, key: &std::path::Path) -> String {
    let key = key.display();
    format!(
        "Host cs.{name}\n\tUser vscode\n\tProxyCommand {gh} cs ssh -c {name} --stdio -- -i {key}\n\tUserKnownHostsFile=/dev/null\n\tStrictHostKeyChecking no\n\tLogLevel quiet\n\tControlMaster auto\n\tIdentityFile {key}\n"
    )
}

/// Absolute path of `gh` on PATH (what gh itself writes into its ProxyCommand), else `gh`.
fn gh_path() -> String {
    std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("gh"))
                .find(|f| f.is_file())
        })
        .map(|f| f.display().to_string())
        .unwrap_or_else(|| "gh".to_string())
}

/// `config` with the block for `alias` removed (pure). Also drops one blank line left before it.
pub(crate) fn remove_block(config: &str, alias: &str) -> String {
    let open = format!("{MARK_OPEN}{alias} >>>");
    let close = format!("{MARK_CLOSE}{alias} <<<");
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in config.lines() {
        if line.trim_end() == open {
            skipping = true;
            if out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            continue;
        }
        if skipping {
            if line.trim_end() == close {
                skipping = false;
            }
            continue;
        }
        out.push(line);
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// `config` with `block` replacing any existing block for `alias`, appended at the end (pure).
pub(crate) fn upsert_block(config: &str, alias: &str, block: &str) -> String {
    let mut s = remove_block(config, alias);
    if !s.is_empty() {
        s.push('\n');
    }
    s.push_str(block);
    s
}

/// `(alias, codespace)` for every managed block in `config` (pure).
pub(crate) fn managed_blocks(config: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in config.lines() {
        if let Some(rest) = line.strip_prefix(MARK_OPEN) {
            current = rest.strip_suffix(" >>>").map(str::to_string);
        } else if let (Some(alias), Some(name)) = (&current, line.strip_prefix(NAME_LINE)) {
            out.push((alias.clone(), name.trim().to_string()));
            current = None;
        }
    }
    out
}

#[derive(serde::Deserialize)]
struct Cs {
    name: String,
    #[serde(rename = "displayName")]
    display: Option<String>,
    state: String,
}

fn list_codespaces() -> Vec<Cs> {
    let out = crate::gh::gh_command(&["cs", "list", "--json", "name,displayName,state"])
        .output()
        .unwrap_or_else(|error| crate::fail(&format!("bn cs: could not run gh: {error}")));
    if !out.status.success() {
        crate::fail(&format!(
            "bn cs: gh cs list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|error| crate::fail(&format!("bn cs: invalid gh cs list output: {error}")))
}

/// Resolve a codespace by full name, name prefix, or display name.
fn resolve(query: &str, all: &[Cs]) -> Result<usize, String> {
    let q = query.to_ascii_lowercase();
    let hits: Vec<usize> = (0..all.len())
        .filter(|&i| {
            let c = &all[i];
            c.name.to_ascii_lowercase() == q
                || c.name.to_ascii_lowercase().starts_with(&q)
                || c.display.as_deref().unwrap_or("").to_ascii_lowercase() == q
        })
        .collect();
    match hits.as_slice() {
        [i] => Ok(*i),
        [] => Err(format!("no codespace matches '{query}'")),
        _ => Err(format!(
            "'{query}' matches {} codespaces: {}",
            hits.len(),
            hits.iter()
                .map(|&i| all[i].name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn print_choices(all: &[Cs], cfg_text: &str) {
    let blocks = managed_blocks(cfg_text);
    eprintln!("codespaces (connect one with: bn cs connect <name> [--alias cs-x]):");
    for c in all {
        let alias = blocks
            .iter()
            .find(|(_, n)| n == &c.name)
            .map(|(a, _)| format!("connected as {a}"))
            .unwrap_or_else(|| {
                format!(
                    "would be {}",
                    default_alias(&c.name, c.display.as_deref().unwrap_or(""))
                )
            });
        eprintln!("  {:<40} {:<10} {}", c.name, c.state, alias);
    }
}

fn read_config(path: &std::path::Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => crate::fail(&format!(
            "bn cs: could not read {}: {error}",
            path.display()
        )),
    }
}

fn write_config(path: &std::path::Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir).unwrap_or_else(|error| {
        crate::fail(&format!(
            "bn cs: could not create {}: {error}",
            dir.display()
        ))
    });
    let backup = dir.join("config.bak-bn-cs");
    if backup.exists() && !backup.is_file() {
        crate::fail(&format!("bn cs: backup {} is not a file", backup.display()));
    }
    if path.exists() && !backup.exists() {
        std::fs::copy(path, &backup)
            .and_then(|_| std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600)))
            .unwrap_or_else(|error| {
                crate::fail(&format!(
                    "bn cs: could not backup {}: {error}",
                    path.display()
                ))
            });
    }
    let tmp = dir.join(".config.bn-cs.tmp");
    if let Err(e) = std::fs::write(&tmp, text)
        .and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)))
        .and_then(|_| std::fs::rename(&tmp, path))
    {
        let _ = std::fs::remove_file(&tmp);
        crate::fail(&format!("bn cs: could not write {}: {e}", path.display()));
    }
}

pub(crate) fn connect(query: Option<&str>, alias: Option<&str>) {
    let cfg_path = ssh_dir().join("config");
    let cfg_text = read_config(&cfg_path);
    let all = list_codespaces();
    if all.is_empty() {
        crate::fail("bn cs connect: `gh cs list` returned no codespaces (gh auth status; gh auth refresh -s codespace)");
    }
    let Some(query) = query else {
        print_choices(&all, &cfg_text);
        std::process::exit(2);
    };
    let cs = match resolve(query, &all) {
        Ok(i) => &all[i],
        Err(e) => {
            eprintln!("bn cs connect: {e}");
            print_choices(&all, &cfg_text);
            std::process::exit(2);
        }
    };
    let alias = alias
        .map(str::to_string)
        .unwrap_or_else(|| default_alias(&cs.name, cs.display.as_deref().unwrap_or("")));
    if !crate::cmd::agent::safe_ssh_host(&alias) {
        crate::fail(&format!(
            "bn cs connect: '{alias}' is not a usable ssh alias"
        ));
    }
    if let Some((other, _)) = managed_blocks(&cfg_text)
        .into_iter()
        .find(|(a, n)| a == &alias && n != &cs.name)
    {
        crate::fail(&format!(
            "bn cs connect: alias '{other}' already points at another codespace; pick one with --alias or run bn cs disconnect {other}"
        ));
    }
    let running = cs.state == "Available";
    let stopped_note =
        || {
            println!(
            "  it is {}; not connecting, so it stays stopped. `ssh {alias} true` starts it (~20s).",
            if cs.state.is_empty() { "not running" } else { &cs.state }
        )
        };
    // `gh cs ssh --config` STARTS a stopped codespace (verified), so a stopped one never goes
    // through gh: an existing block is kept as is, a new one is built from gh's fixed template.
    if !running {
        if managed_blocks(&cfg_text)
            .iter()
            .any(|(a, n)| a == &alias && n == &cs.name)
        {
            println!(
                "bn cs connect: {} is already `ssh {alias}`; left unchanged",
                cs.name
            );
        } else {
            let tpl = gh_template(&cs.name, &gh_path(), &ssh_dir().join("codespaces.auto"));
            let block = render_block(&alias, &cs.name, &tpl);
            write_config(&cfg_path, &upsert_block(&cfg_text, &alias, &block));
            println!(
                "bn cs connect: {} is now `ssh {alias}` ({})",
                cs.name,
                cfg_path.display()
            );
        }
        stopped_note();
        return;
    }
    let out = crate::gh::gh_command(&["cs", "ssh", "--config", "-c", &cs.name]).output();
    let gh_config = match out {
        Ok(o)
            if o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("ProxyCommand") =>
        {
            String::from_utf8_lossy(&o.stdout).into_owned()
        }
        Ok(o) => crate::fail(&format!(
            "bn cs connect: gh cs ssh --config failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => crate::fail(&format!("bn cs connect: could not run gh: {e}")),
    };
    let block = render_block(&alias, &cs.name, &gh_config);
    write_config(&cfg_path, &upsert_block(&cfg_text, &alias, &block));
    println!(
        "bn cs connect: {} is now `ssh {alias}` ({})",
        cs.name,
        cfg_path.display()
    );
    let check = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=120",
            &alias,
            "hostname",
        ])
        .output();
    match check {
        Ok(o) if o.status.success() => println!(
            "  checked: reached {} (later connects reuse the connection)",
            String::from_utf8_lossy(&o.stdout).trim()
        ),
        Ok(o) => {
            eprintln!(
                "  the block is written but ssh failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("  the block is written but ssh could not run: {e}");
            std::process::exit(1);
        }
    }
}

pub(crate) fn disconnect(alias: &str) {
    let cfg_path = ssh_dir().join("config");
    let cfg_text = read_config(&cfg_path);
    if !managed_blocks(&cfg_text).iter().any(|(a, _)| a == alias) {
        crate::fail(&format!(
            "bn cs disconnect: no `{MARK_OPEN}{alias} >>>` block in {}",
            cfg_path.display()
        ));
    }
    write_config(&cfg_path, &remove_block(&cfg_text, alias));
    println!(
        "bn cs disconnect: removed {alias} from {} (the codespace itself is untouched)",
        cfg_path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const GH: &str = "Host cs.opulent-umbrella-v4rgjr95jvphpwvg.codespace_mono_fullclone\n\tUser vscode\n\tProxyCommand /usr/bin/gh cs ssh -c opulent-umbrella-v4rgjr95jvphpwvg --stdio -- -i /home/u/.ssh/codespaces.auto\n\tUserKnownHostsFile=/dev/null\n\tStrictHostKeyChecking no\n\tLogLevel quiet\n\tControlMaster auto\n\tIdentityFile /home/u/.ssh/codespaces.auto\n\n";

    #[test]
    fn default_alias_uses_last_display_word() {
        assert_eq!(
            default_alias("opulent-umbrella-v4rgjr95jvphpwvg", "opulent umbrella"),
            "cs-umbrella"
        );
        assert_eq!(
            default_alias("bug-free-potato-x6p9gp74g9gc6xvj", "bug-free potato"),
            "cs-potato"
        );
        assert_eq!(
            default_alias("laughing-broccoli-g9r5vrjpwwxh9jgx", ""),
            "cs-broccoli"
        );
    }

    #[test]
    fn render_renames_host_and_hardens_proxy() {
        let b = render_block("cs-umbrella", "opulent-umbrella-v4rgjr95jvphpwvg", GH);
        assert!(b.starts_with("# >>> bn codespace cs-umbrella >>>\n# bn: codespace opulent-umbrella-v4rgjr95jvphpwvg\nHost cs-umbrella\n"));
        assert!(b.contains("\tProxyCommand env GODEBUG=netdns=cgo /usr/bin/gh cs ssh -c opulent-umbrella-v4rgjr95jvphpwvg --stdio"));
        assert!(
            b.contains("\tControlPath ~/.ssh/cm-%C\n\tControlPersist 10m\n\tConnectTimeout 120\n")
        );
        assert!(b.ends_with("# <<< bn codespace cs-umbrella <<<\n"));
        // idempotent on its own output's proxy line
        let again = render_block("cs-umbrella", "x", &b.replace("# ", "#x "));
        assert_eq!(again.matches("GODEBUG").count(), 1);
    }

    #[test]
    fn template_matches_gh_output_shape() {
        let key = std::path::Path::new("/home/u/.ssh/codespaces.auto");
        let tpl = gh_template("opulent-umbrella-v4rgjr95jvphpwvg", "/usr/bin/gh", key);
        let from_tpl = render_block("cs-umbrella", "opulent-umbrella-v4rgjr95jvphpwvg", &tpl);
        let from_gh = render_block("cs-umbrella", "opulent-umbrella-v4rgjr95jvphpwvg", GH);
        assert_eq!(
            from_tpl, from_gh,
            "a stopped codespace gets the same block gh would give"
        );
    }

    #[test]
    fn upsert_replaces_only_its_own_block() {
        let other = "Host home\n\tHostName 10.0.0.2\n";
        let b1 = render_block("cs-umbrella", "n1", GH);
        let cfg = upsert_block(other, "cs-umbrella", &b1);
        assert!(cfg.starts_with(other));
        let b2 = render_block("cs-umbrella", "n2", GH);
        let cfg2 = upsert_block(&cfg, "cs-umbrella", &b2);
        assert_eq!(
            cfg2.matches("# >>> bn codespace cs-umbrella >>>").count(),
            1
        );
        assert!(cfg2.contains("# bn: codespace n2") && !cfg2.contains("# bn: codespace n1"));
        assert!(cfg2.starts_with(other));
        assert_eq!(
            upsert_block(&cfg2, "cs-umbrella", &b2),
            cfg2,
            "upsert is idempotent"
        );
    }

    #[test]
    fn remove_and_list_blocks() {
        let cfg = upsert_block(
            &upsert_block("Host a\n", "cs-one", &render_block("cs-one", "n-one", GH)),
            "cs-two",
            &render_block("cs-two", "n-two", GH),
        );
        assert_eq!(
            managed_blocks(&cfg),
            vec![
                ("cs-one".into(), "n-one".into()),
                ("cs-two".into(), "n-two".into())
            ]
        );
        let left = remove_block(&cfg, "cs-one");
        assert_eq!(
            managed_blocks(&left),
            vec![("cs-two".into(), "n-two".into())]
        );
        assert!(left.starts_with("Host a\n") && !left.contains("cs-one"));
        assert_eq!(remove_block("Host a\n", "cs-x"), "Host a\n");
    }
}
