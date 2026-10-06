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
/// done when the agent stops — and, on stop, `links harvest` saves the PRs and work items the
/// turn mentioned, as the Claude Stop hook does.
fn copilot_tmux_agent_hook_config() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "hooks": {
            "userPromptSubmitted": [tmux_agent_hook_entry("working")],
            "permissionRequest": [tmux_agent_hook_entry("waiting")],
            "notification": [tmux_agent_hook_entry("waiting")],
            "agentStop": [tmux_agent_hook_entry("done"), hook_entry("links harvest", 10)],
        },
    })
}

/// The hooks a Claude launch carries in `--settings`: the tmux activity badge when it runs inside
/// tmux, the turn-end guard when it is a controller, `links harvest` on Stop unless
/// `BN_LINK_HARVEST=0`, and — unless `BN_COMPACT_HOOKS=0` — the
/// compaction pair: `compact prep` before a compaction and `compact audit` when the session comes
/// back from one. `None` when there are none. Claude merges these with the user's own settings
/// files, so hooks someone already wired by hand keep running.
fn claude_launch_settings(controller: bool, tmux: bool) -> Option<String> {
    let compact = compact_hooks_on(std::env::var("BN_COMPACT_HOOKS").ok().as_deref());
    let harvest = compact_hooks_on(std::env::var("BN_LINK_HARVEST").ok().as_deref());
    claude_launch_settings_with(controller, tmux, compact, harvest)
}

/// The compaction and link-harvest hooks are on by default; only `BN_COMPACT_HOOKS=0` /
/// `BN_LINK_HARVEST=0` turns each off.
fn compact_hooks_on(env: Option<&str>) -> bool {
    env != Some("0")
}

fn claude_launch_settings_with(
    controller: bool,
    tmux: bool,
    compact: bool,
    harvest: bool,
) -> Option<String> {
    let mut hooks = serde_json::Map::new();
    if compact {
        // A hook that stalls blocks the compaction, so both are short and neither touches the
        // network.
        hooks.insert(
            "PreCompact".to_string(),
            serde_json::json!([{ "hooks": [hook_entry("compact prep --hook", 30)] }]),
        );
        hooks.insert(
            "SessionStart".to_string(),
            serde_json::json!([{ "matcher": "compact",
                "hooks": [hook_entry("compact audit --hook", 15)] }]),
        );
    }
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
    if harvest {
        // Saves the PRs and work items the turn mentioned to `bn links`. Local file reads
        // only, and it swallows every failure, so it cannot stall or fail a turn.
        stop.push(hook_entry("links harvest", 10));
    }
    if !stop.is_empty() {
        hooks.insert("Stop".to_string(), serde_json::json!([{ "hooks": stop }]));
    }
    if hooks.is_empty() {
        return None;
    }
    let mut settings = serde_json::json!({ "hooks": hooks });
    if compact {
        // Tells the repo's own SessionStart memory hook that the audit is handled here.
        settings["env"] = serde_json::json!({ "BN_COMPACT_HOOKS": "1" });
    }
    Some(settings.to_string())
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
    fn a_plain_launch_outside_tmux_without_compact_hooks_passes_no_settings() {
        assert_eq!(claude_launch_settings_with(false, false, false, false), None);
    }

    /// Every launch brackets a compaction: prep before it, the audit when the session comes back,
    /// and the env marker that keeps the repo's own SessionStart hook from printing it twice.
    #[test]
    fn a_launch_carries_the_compaction_pair() {
        let settings = claude_launch_settings_with(false, false, true, false).expect("compact hooks");
        assert_eq!(events(&settings), ["PreCompact", "SessionStart"]);
        let parsed: serde_json::Value = serde_json::from_str(&settings).expect("JSON");
        assert_eq!(parsed["hooks"]["SessionStart"][0]["matcher"], "compact");
        assert!(settings.contains("compact prep --hook"), "{settings}");
        assert!(settings.contains("compact audit --hook"), "{settings}");
        assert_eq!(parsed["env"]["BN_COMPACT_HOOKS"], "1");
    }

    /// Inside tmux the window badge follows the agent's turns without anyone wiring hooks by hand.
    /// Unset (the default for every `bn agent launch`) means on; only an explicit `0` opts out.
    #[test]
    fn compact_hooks_default_on_and_only_zero_opts_out() {
        assert!(compact_hooks_on(None));
        assert!(compact_hooks_on(Some("1")));
        assert!(compact_hooks_on(Some("")));
        assert!(!compact_hooks_on(Some("0")));
    }

    /// The default launch settings (what `with_claude_launch_hooks` passes as `--settings`) carry
    /// the compaction pair plus the env marker the repo's memory hook reads.
    #[test]
    fn a_default_launch_carries_the_compaction_pair_and_env_marker() {
        let settings = claude_launch_settings_with(false, false, compact_hooks_on(None), false)
            .expect("compact hooks are on by default");
        let parsed: serde_json::Value = serde_json::from_str(&settings).unwrap();
        assert_eq!(parsed["env"]["BN_COMPACT_HOOKS"], "1");
        assert_eq!(events(&settings), vec!["PreCompact", "SessionStart"]);
        assert!(claude_launch_settings_with(false, false, compact_hooks_on(Some("0")), false).is_none());
    }

    #[test]
    fn a_launch_inside_tmux_carries_the_activity_hooks() {
        let settings = claude_launch_settings_with(false, true, false, false).expect("tmux hooks");
        assert_eq!(
            events(&settings),
            ["Notification", "Stop", "UserPromptSubmit"]
        );
        assert!(settings.contains("tmux-agent working"), "{settings}");
        assert!(settings.contains("tmux-agent done"), "{settings}");
        assert!(!settings.contains("turn-end-guard"), "{settings}");
    }

    /// Every default launch saves the links a turn mentioned; `BN_LINK_HARVEST=0` opts out.
    #[test]
    fn a_launch_harvests_links_on_stop() {
        let settings = claude_launch_settings_with(false, false, false, true).expect("harvest hook");
        assert_eq!(events(&settings), ["Stop"]);
        assert!(settings.contains("links harvest"), "{settings}");
    }

    /// A controller inside tmux keeps its turn-end guard beside the badge: one Stop list, both.
    #[test]
    fn a_controller_keeps_its_turn_end_guard_beside_the_badge() {
        let settings = claude_launch_settings_with(true, true, false, false).expect("hooks");
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
        assert!(written.contains("links harvest"), "{written}");
    }
}
