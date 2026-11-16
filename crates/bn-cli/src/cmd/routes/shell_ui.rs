use crate::*;

pub(super) fn route(cfg: &Config, action: CmdShellUi, json: bool) {
    match action {
        CmdShellUi::Edit { action } => crate::edit::route(cfg, *action, json),
        CmdShellUi::TmuxStatus { path } => emit_tmux_status(cfg, path),
        CmdShellUi::TmuxPrune { force } => emit_tmux_prune(cfg, force, json),
        CmdShellUi::TmuxSnapshotFilter { path } => emit_snapshot_filter(cfg, &path),
        CmdShellUi::TmuxGit { path, refresh } => emit_tmux_git(path, refresh),
        CmdShellUi::TmuxAttnCount => emit_tmux_attn_count(),
        CmdShellUi::TmuxDoneBadge {
            window,
            status,
            exited,
        } => crate::cmd::tmux_done::emit(&window, status.as_deref(), exited),
        CmdShellUi::TmuxCockpit => emit_tmux_cockpit(cfg),
        CmdShellUi::TmuxHelp => emit_tmux_help(),
        CmdShellUi::TmuxKeys { since } => crate::cmd::tmux_keys::emit_tmux_keys(since),
        CmdShellUi::Glyphs => emit_glyphs(),
        CmdShellUi::TmuxSync => {
            let ctx = resolve_or_exit(cfg);
            sync_tmux(cfg, &ctx);
        }
        CmdShellUi::TmuxReconcile { dry_run } => emit_tmux_reconcile(cfg, dry_run),
        CmdShellUi::TmuxAgent { state } => set_tmux_agent(&state),
        CmdShellUi::Release {
            bump,
            dry_run,
            skip_verify,
        } => emit_release(cfg, bump, dry_run, skip_verify, json),
    }
}
