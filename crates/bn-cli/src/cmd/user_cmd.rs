//! `bn cmd` — list, scaffold, and run your own commands (see [`bn_core::cmds`]).
use crate::*;

use bn_core::cmds;
use bn_core::resolver::JobScope;

pub(crate) fn emit_cmd(
    cfg: &Config,
    name: Option<String>,
    args: Vec<String>,
    new: Option<String>,
    scope: &str,
    which: bool,
    json: bool,
) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let ctx = resolver::resolve(cfg, &cwd).ok();
    if let Some(new) = new {
        return scaffold(cfg, ctx.as_ref(), &new, scope, json);
    }
    let Some(name) = name else {
        return list(cfg, ctx.as_ref(), json);
    };
    let Some(cmd) = cmds::find(cfg, ctx.as_ref(), &name) else {
        eprintln!(
            "bn cmd: no command '{name}' (looked branch → project → repo → global); \
             `bn cmd` lists them, `bn cmd --new {name}` makes one"
        );
        return 2;
    };
    if which {
        if !(json && json_ok("cmd-which", serde_json::to_value(&cmd).unwrap_or_default())) {
            println!("{}", cmd.path.display());
        }
        return 0;
    }
    if cfg!(windows) {
        eprintln!("bn cmd: commands are sh scripts; not supported on native Windows");
        return 2;
    }
    match cmds::command(&cmd, &args, &cwd, ctx.as_ref()).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("bn cmd: failed to run {}: {e}", cmd.path.display());
            127
        }
    }
}

fn list(cfg: &Config, ctx: Option<&Resolved>, json: bool) -> i32 {
    let all = cmds::list(cfg, ctx);
    if json && json_ok("cmd-list", serde_json::json!({ "commands": all })) {
        return 0;
    }
    if all.is_empty() {
        println!("No commands yet. Make one with `bn cmd --new <name>`.");
        return 0;
    }
    let width = all.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in &all {
        let description = if c.description.is_empty() {
            "(no # description:)"
        } else {
            &c.description
        };
        println!("{:width$}  {:7}  {description}", c.name, c.scope.label());
    }
    0
}

fn scaffold(cfg: &Config, ctx: Option<&Resolved>, name: &str, scope: &str, json: bool) -> i32 {
    let scope = match scope {
        "global" => JobScope::Global,
        "repo" => JobScope::Repo,
        "project" => JobScope::Project,
        "branch" => JobScope::Branch,
        other => {
            eprintln!("bn cmd: unknown --scope '{other}' (global, repo, project, branch)");
            return 2;
        }
    };
    let Some(dir) = cmds::scope_dir(cfg, ctx, scope) else {
        eprintln!(
            "bn cmd: no {} tier here (repo/project need a worktree; branch needs a non-main branch)",
            scope.label()
        );
        return 2;
    };
    match cmds::scaffold(&dir, name) {
        Ok(path) => {
            let shown = path.display().to_string();
            if !(json && json_ok("cmd-new", serde_json::json!({ "name": name, "path": shown }))) {
                println!("{shown}");
                println!("Edit it, then run `bn cmd {name}`.");
            }
            0
        }
        Err(e) => {
            eprintln!("bn cmd: {}: {e}", dir.join(format!("{name}.sh")).display());
            1
        }
    }
}

/// `bn cmd --pick` (tmux `prefix c`): choose a command in fzf, type its arguments, run it, and hold
/// the output until Enter so a popup does not close over it.
pub(crate) fn emit_pick(cfg: &Config) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let ctx = resolver::resolve(cfg, &cwd).ok();
    let all = cmds::list(cfg, ctx.as_ref());
    if all.is_empty() {
        println!("No commands yet. Make one with `bn cmd --new <name>`.");
        return hold(0);
    }
    let Some(row) = fzf_with_preview(&pick_rows(&all)) else {
        return 0;
    };
    let Some(cmd) = all.iter().find(|c| row.split('\t').next().map(str::trim) == Some(c.name.as_str())) else {
        return 0;
    };
    let usage = cmd.usage.as_deref().unwrap_or(&cmd.name);
    print!("{usage}\nargs> ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return 1;
    }
    let args = match split_args(line.trim()) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("bn cmd: {e}");
            return hold(2);
        }
    };
    let code = match cmds::command(cmd, &args, &cwd, ctx.as_ref()).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("bn cmd: failed to run {}: {e}", cmd.path.display());
            127
        }
    };
    hold(code)
}

/// One fzf row per command: `name<TAB>scope<TAB>description<TAB>path`. fzf shows the first three
/// fields and previews the fourth, so the path never has to be parsed back out of display text.
fn pick_rows(all: &[cmds::CmdRef]) -> Vec<String> {
    let width = all.iter().map(|c| c.name.len()).max().unwrap_or(0);
    all.iter()
        .map(|c| {
            format!(
                "{:width$}\t{:7}\t{}\t{}",
                c.name,
                c.scope.label(),
                c.description,
                c.path.display()
            )
        })
        .collect()
}

/// fzf over [`pick_rows`] with the highlighted script shown on the right (bat when installed).
fn fzf_with_preview(rows: &[String]) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("fzf")
        .args([
            "--reverse",
            "--delimiter",
            "\t",
            "--with-nth",
            "1..3",
            "--header",
            "bn cmd · enter runs, esc cancels",
            "--preview",
            "bat --style=plain --color=always {4} 2>/dev/null || cat {4}",
            "--preview-window",
            "right,55%,wrap",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let input = rows.join("\n");
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let out = child.wait_with_output().ok()?;
    let row = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    (out.status.success() && !row.is_empty()).then_some(row)
}

/// Split a typed argument line the way the shell would (quotes, `$HOME`), by letting `sh` do it.
fn split_args(line: &str) -> Result<Vec<String>, String> {
    if line.is_empty() {
        return Ok(Vec::new());
    }
    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("set -- {line}\nprintf '%s\\0' \"$@\""))
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "could not parse arguments: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .strip_suffix('\0')
        .map(|t| t.split('\0').map(str::to_string).collect())
        .unwrap_or_default())
}

fn hold(code: i32) -> i32 {
    print!("\n[exit {code}] press enter to close");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let _ = std::io::stdin().read_line(&mut String::new());
    code
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn typed_args_split_like_the_shell() {
        assert_eq!(
            super::split_args(r#"-n "two words" "" -- echo 'a b'"#).unwrap(),
            ["-n", "two words", "", "--", "echo", "a b"]
        );
        assert!(super::split_args("").unwrap().is_empty());
        assert!(super::split_args("\"unclosed").is_err());
    }
}
