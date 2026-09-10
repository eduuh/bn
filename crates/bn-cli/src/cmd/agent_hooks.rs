//! The hooks `bn agent launch` hands a harness: the controller's turn-end guard and the tmux
//! activity badge. Split out of `agent.rs` under the 1000-line ratchet.
use crate::*;

/// A hook command that runs this bn with `args`, quoted for the harness's shell.
fn bn_hook_command(args: &str) -> String {
    let executable = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "bn".to_string());
    #[cfg(windows)]
    {
        format!("& '{}' {args}", executable.replace('\'', "''"))
    }
    #[cfg(not(windows))]
    {
        format!("{} {args}", wt::shell_single_quote(&executable))
    }
}

fn hook_entry(args: &str, timeout_secs: u64) -> serde_json::Value {
    serde_json::json!({
        "type": "command",
        "command": bn_hook_command(args),
        "timeout": timeout_secs,
        "timeoutSec": timeout_secs,
    })
}

fn controller_hook_entry() -> serde_json::Value {
    hook_entry("control turn-end-guard", 15)
}

/// The activity badge on this agent's tmux window: `bn tmux-agent <state>`.
fn tmux_agent_hook_entry(state: &str) -> serde_json::Value {
    hook_entry(&format!("tmux-agent {state}"), 5)
}

fn copilot_controller_hook_config() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "hooks": {
            "agentStop": [controller_hook_entry()],
        },
    })
}

/// Copilot's tmux activity hooks: working on a prompt, waiting on a permission or a notification,
/// done when the agent stops.
fn copilot_tmux_agent_hook_config() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "hooks": {
            "userPromptSubmitted": [tmux_agent_hook_entry("working")],
            "permissionRequest": [tmux_agent_hook_entry("waiting")],
            "notification": [tmux_agent_hook_entry("waiting")],
            "agentStop": [tmux_agent_hook_entry("done")],
        },
    })
}

/// The hooks a Claude launch carries in `--settings`: the tmux activity badge when it runs inside
/// tmux, and the turn-end guard when it is a controller. `None` when there are none, so a plain
/// launch outside tmux passes no settings at all. Claude merges these with the user's own settings
/// files, so hooks someone already wired by hand keep running.
fn claude_launch_settings(controller: bool, tmux: bool) -> Option<String> {
    let mut hooks = serde_json::Map::new();
    if tmux {
        hooks.insert(
            "UserPromptSubmit".to_string(),
            serde_json::json!([{ "hooks": [tmux_agent_hook_entry("working")] }]),
        );
        hooks.insert(
            "Notification".to_string(),
            serde_json::json!([{ "hooks": [tmux_agent_hook_entry("waiting")] }]),
        );
    }
    let mut stop = Vec::new();
    if tmux {
        stop.push(tmux_agent_hook_entry("done"));
    }
    if controller {
        stop.push(controller_hook_entry());
    }
    if !stop.is_empty() {
        hooks.insert("Stop".to_string(), serde_json::json!([{ "hooks": stop }]));
    }
    (!hooks.is_empty()).then(|| serde_json::json!({ "hooks": hooks }).to_string())
}

pub(super) fn with_claude_launch_hooks(
    mut args: Vec<String>,
    controller: bool,
    tmux: bool,
) -> Vec<String> {
    if let Some(settings) = claude_launch_settings(controller, tmux) {
        args.splice(0..0, ["--settings".to_string(), settings]);
    }
    args
}

fn copilot_hooks_dir() -> std::io::Result<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("COPILOT_HOME") {
        return Ok(std::path::PathBuf::from(home).join("hooks"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(std::path::PathBuf::from(home)
        .join(".copilot")
        .join("hooks"))
}

/// Write one bn-owned Copilot hooks file, `<hooks_dir>/<name>.json`, atomically — and leave it
/// alone when it already says the same thing.
fn install_copilot_hook_file_at(
    hooks_dir: &std::path::Path,
    name: &str,
    config: &serde_json::Value,
) -> std::io::Result<()> {
    std::fs::create_dir_all(hooks_dir)?;
    let path = hooks_dir.join(format!("{name}.json"));
    let body = serde_json::to_vec_pretty(config).map_err(std::io::Error::other)?;
    if std::fs::read(&path).ok().as_deref() == Some(body.as_slice()) {
        return Ok(());
    }
    let temp_path = hooks_dir.join(format!(".{name}.{}.tmp", gen_uuid()));
    let mut temp = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    if let Err(error) = std::io::Write::write_all(&mut temp, &body)
        .and_then(|()| temp.sync_all())
        .and_then(|()| std::fs::rename(&temp_path, &path))
    {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

pub(super) fn install_copilot_controller_hook() -> std::io::Result<()> {
    install_copilot_hook_file_at(
        &copilot_hooks_dir()?,
        "bn-controller",
        &copilot_controller_hook_config(),
    )
}

/// The Copilot tmux activity hooks, written where Copilot loads user hooks. Best-effort at the call
/// site: a badge that does not update is not a reason to refuse a launch.
pub(super) fn install_copilot_tmux_agent_hook() -> std::io::Result<()> {
    install_copilot_hook_file_at(
        &copilot_hooks_dir()?,
        "bn-agent",
        &copilot_tmux_agent_hook_config(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(settings: &str) -> Vec<String> {
        let parsed: serde_json::Value = serde_json::from_str(settings).expect("settings are JSON");
        let mut keys: Vec<String> = parsed["hooks"]
            .as_object()
            .expect("a hooks table")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn a_plain_launch_outside_tmux_passes_no_settings() {
        assert_eq!(claude_launch_settings(false, false), None);
        assert_eq!(
            with_claude_launch_hooks(vec!["--x".to_string()], false, false),
            vec!["--x".to_string()]
        );
    }

    /// Inside tmux the window badge follows the agent's turns without anyone wiring hooks by hand.
    #[test]
    fn a_launch_inside_tmux_carries_the_activity_hooks() {
        let settings = claude_launch_settings(false, true).expect("tmux hooks");
        assert_eq!(
            events(&settings),
            ["Notification", "Stop", "UserPromptSubmit"]
        );
        assert!(settings.contains("tmux-agent working"), "{settings}");
        assert!(settings.contains("tmux-agent done"), "{settings}");
        assert!(!settings.contains("turn-end-guard"), "{settings}");
    }

    /// A controller inside tmux keeps its turn-end guard beside the badge: one Stop list, both.
    #[test]
    fn a_controller_keeps_its_turn_end_guard_beside_the_badge() {
        let settings = claude_launch_settings(true, true).expect("hooks");
        let parsed: serde_json::Value = serde_json::from_str(&settings).expect("JSON");
        let stop = parsed["hooks"]["Stop"][0]["hooks"]
            .as_array()
            .expect("stop hooks");
        assert_eq!(stop.len(), 2);
        assert!(settings.contains("control turn-end-guard"), "{settings}");
    }

    /// The Copilot hooks file is rewritten only when its content changes, and names the events the
    /// badge follows.
    #[test]
    fn the_copilot_hook_file_is_written_once_and_names_every_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = copilot_tmux_agent_hook_config();
        install_copilot_hook_file_at(dir.path(), "bn-agent", &config).expect("first write");
        let path = dir.path().join("bn-agent.json");
        let first = std::fs::metadata(&path).and_then(|m| m.modified()).expect("mtime");
        install_copilot_hook_file_at(dir.path(), "bn-agent", &config).expect("second write");
        let second = std::fs::metadata(&path).and_then(|m| m.modified()).expect("mtime");
        assert_eq!(first, second, "an unchanged file is left alone");

        let written = std::fs::read_to_string(&path).expect("read hooks");
        for event in ["userPromptSubmitted", "permissionRequest", "notification", "agentStop"] {
            assert!(written.contains(event), "{event} missing: {written}");
        }
    }
}
