//! `bn notify status` — what the notification policy gate did today, and why.
//!
//! The gate's whole promise is that quiet never means lost: a toast it holds back is still in
//! `bn next`, `bn attn`, `bn agent live` and the tmux badges. This is the surface that makes the
//! holding *legible* — how much of today's budget is spent, which quiet window is open, and what
//! was withheld — so "I never heard about it" always has an answer.

use crate::*;
use bn_core::notify::policy;

/// `bn notify status` (and bare `bn notify`).
pub(crate) fn emit_notify_status(cfg: &Config, as_json: bool) {
    let now = bn_core::util::now_epoch_secs();
    let mut ledger = policy::load_ledger();
    ledger.roll(&today());
    let quiet = policy::Quiet::probe(cfg, now);
    let p = &cfg.notify;
    let cooling = ledger.in_cooldown(p.cooldown_secs, now);
    let held: usize = ledger.held_today.values().sum();

    if as_json {
        let data = serde_json::json!({
            "enabled": p.enabled,
            "bar": p.bar.label(),
            "sent_today": ledger.sent_today,
            "sent_via_tmux": ledger.sent_via_tmux,
            "daily_budget": p.daily_budget,
            "held_today": held,
            "held_by_reason": ledger.held_today,
            "sent_by_source": ledger.sent_by_source,
            "quiet": { "afk": quiet.afk, "focus": quiet.focus, "protected_block": quiet.protected_block },
            "cooldown": cooling
                .iter()
                .map(|(key, sent)| serde_json::json!({ "key": key, "quiet_for_secs": p.cooldown_secs - (now - sent) }))
                .collect::<Vec<_>>(),
        });
        println!("{}", Envelope::success("notify", data).to_json());
        return;
    }

    if !p.enabled {
        println!("notify · off ([notify].enabled = false) — nothing will toast");
    } else {
        // Say which channel carried them: "3/12 toasts today" reads as a healthy desktop backend,
        // and on a host where every OS backend is broken that would be a lie of omission.
        let via = if ledger.sent_via_tmux > 0 {
            format!(
                " ({} via tmux — no working desktop backend)",
                ledger.sent_via_tmux
            )
        } else {
            String::new()
        };
        println!(
            "notify · bar: {} · {}/{} toasts today{via}",
            p.bar.label(),
            ledger.sent_today,
            p.daily_budget
        );
    }

    let windows = [
        (quiet.afk, "away mode — everything batches into `bn afk end`"),
        (quiet.focus, "focus block — only blocking items interrupt"),
        (
            quiet.protected_block,
            "protected routine block — only blocking items interrupt",
        ),
    ];
    for (open, label) in windows {
        if open {
            println!("  quiet: {label}");
        }
    }

    if held > 0 {
        let mut reasons: Vec<_> = ledger.held_today.iter().collect();
        reasons.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        let summary = reasons
            .iter()
            .map(|(reason, count)| format!("{count} {reason}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  held today: {summary}");
        println!("  ↳ nothing is lost — `bn next` / `bn attn` still list every one");
    }

    if !cooling.is_empty() {
        println!("  quiet for now ({}):", cooling.len());
        for (key, sent) in cooling.iter().take(5) {
            let mins = (p.cooldown_secs - (now - sent)).max(0) / 60;
            println!("    · {key} — {mins}m");
        }
    }

    if ledger.sent_today == 0 && held == 0 && cooling.is_empty() {
        println!("  all quiet ✓");
    }
}
