use crate::*;

mod automation;
mod coord;
mod git_pr;
mod insights;
mod intake;
mod misc;
mod planning;
mod review;
mod shell_ui;
mod staff;
mod tracking;

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Intake(_) => "intake",
        Command::Review(_) => "review",
        Command::Planning(_) => "planning",
        Command::Tracking(_) => "tracking",
        Command::Insights(_) => "insights",
        Command::Automation(_) => "automation",
        Command::Staff(_) => "staff",
        Command::ShellUi(_) => "shell-ui",
        Command::GitPr(_) => "git-pr",
        Command::Misc(_) => "misc",
        Command::Coord(_) => "coord",
    }
}

pub(crate) fn route_command(cfg: &Config, command: Command, json: bool) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    // Capture the command name before the match consumes `command` (each arm moves its inner
    // action into the sub-route), so the completion record below can still name it.
    let name = command_name(&command);
    bn_core::log::action(
        "dispatch",
        "route_command",
        serde_json::json!({
            "command": name,
            "json": json,
        }),
    );
    match command {
        Command::Intake(action) => intake::route(cfg, action, json),
        Command::Review(action) => review::route(cfg, action, json),
        Command::Planning(action) => planning::route(cfg, action, json),
        Command::Tracking(action) => tracking::route(cfg, action, json),
        Command::Insights(action) => insights::route(cfg, action, json),
        Command::Automation(action) => automation::route(cfg, action, json),
        Command::Staff(action) => staff::route(cfg, action, json),
        Command::ShellUi(action) => shell_ui::route(cfg, action, json),
        Command::GitPr(action) => git_pr::route(cfg, action, json),
        Command::Misc(action) => misc::route(cfg, action, json),
        Command::Coord(action) => coord::route(cfg, action, json),
    }
    if let Some(timer) = timer {
        bn_core::log::record(
            bn_core::log::LogLevel::Info,
            "dispatch",
            "route_command_done",
            serde_json::json!({ "command": name, "json": json }),
            serde_json::json!({ "ok": true }),
            Some(timer.elapsed_ms()),
        );
    }
}

pub(crate) fn route_default(cfg: &Config, json: bool) {
    let timer = bn_core::log::enabled().then(bn_core::log::Timer::start);
    let ctx = resolve_or_exit(cfg);
    // Detect fresh creation *before* ensure_note so the auto-surface only fires once, on
    // the note's first birth — never on every subsequent `bn`.
    let is_new = !cfg.note_file(&ctx).exists();
    bn_core::log::action(
        "dispatch",
        "route_default",
        serde_json::json!({
            "repo": ctx.repo,
            "branch": ctx.branch,
            "json": json,
            "new_note": is_new,
        }),
    );
    let dir = commands::ensure_note(cfg, &ctx, &today()).unwrap_or_else(|e| fail(&e.message()));
    if json {
        match bn_core::store::read(&cfg.note_file(&ctx)) {
            Ok(note) => {
                let data = serde_json::json!({ "front": note.front, "body": note.body });
                println!("{}", Envelope::success("note", data).to_json());
            }
            Err(e) => fail(&format!("bn: {e}")),
        }
    } else {
        println!("{}", dir.display());
        // stdout is the note dir (consumed by `cd "$(bn)"`); the recall advisory goes to
        // stderr so it never corrupts that path.
        if is_new {
            surface_recall_on_new_note(cfg, &ctx);
        }
    }
    if let Some(timer) = timer {
        bn_core::log::record(
            bn_core::log::LogLevel::Info,
            "dispatch",
            "route_default_done",
            serde_json::json!({
                "repo": ctx.repo,
                "branch": ctx.branch,
                "new_note": is_new,
                "json": json,
            }),
            serde_json::json!({ "ok": true }),
            Some(timer.elapsed_ms()),
        );
    }
}
