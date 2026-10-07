//! `bn watch` — supervise the fleet on a loop.
use crate::*;

/// The toast line for an attention item: staff briefing cards (#265) carry the agent name as a
/// prefix — `staff-sentinel: dependency CVE in …` — so a card-routed toast makes its source
/// obvious, while ordinary items pass through as their bare title. A card's urgent item lands in
/// attn keyed `staff-<agent>` via the framework's routing, so no new notifier is needed: it rides
/// this existing reactor, and the only change is this title formatting.
pub(crate) fn toast_label(source: &str, title: &str) -> String {
    if source.starts_with("staff-") {
        format!("{source}: {title}")
    } else {
        title.to_string()
    }
}



/// What one pass published from the attention list: how many items were relevant, and which of
/// their event ids are *blocking* (open + `urgent`).
///
/// The split matters because the queue and the toast channel want different things: **publish
/// broadly, toast narrowly.** Warn items, agent-sourced items and completions all belong in the
/// queue — `bn afk end`, `bn next` and the controller consumers read them — but only an open
/// urgent item is something that stopped and needs a decision now. Classification therefore
/// happens at the toast boundary in [`emit_watch`], not here.
struct PublishedAttention {
    relevant: usize,
    blocking: std::collections::HashSet<String>,
}

fn publish_attention_wakes(today: &str, now: i64) -> PublishedAttention {
    let queue = bn_core::wake::WakeQueue::default();
    let open = bn_core::attention::open_items(today)
        .into_iter()
        .filter(|item| {
            item.source == "agent" || item.severity == "urgent" || item.severity == "warn"
        })
        .map(|item| (item, bn_core::wake::FleetTransition::Attention));
    let completed = bn_core::attention::completed_items(today)
        .into_iter()
        .map(|item| (item, bn_core::wake::FleetTransition::Done));
    let mut out = PublishedAttention {
        relevant: 0,
        blocking: std::collections::HashSet::new(),
    };
    for (item, transition) in open.chain(completed) {
        out.relevant += 1;
        let event_id = format!("attn:{}", item.id);
        if transition == bn_core::wake::FleetTransition::Attention && item.severity == "urgent" {
            out.blocking.insert(event_id.clone());
        }
        let wake = bn_core::wake::ActionableWake::classify(
            event_id,
            now,
            transition,
            toast_label(&item.source, &item.title),
            bn_core::wake::WakeOwner::None,
        )
        .expect("attention and completion transitions are actionable");
        if let Err(error) = queue.publish(wake) {
            eprintln!("bn wake: publish attention wake: {error}");
        }
    }
    out
}

/// How loud a pending wake is. The queue carries genuinely actionable transitions *and* things
/// that merely happened, and before the policy gate every one of them toasted under the headline
/// "N need you" — including completions, which [`bn_core::attention::completed_items`] documents
/// as explicitly *not* attention ("they hold no action for you beyond a look").
fn wake_class(
    wake: &bn_core::wake::ActionableWake,
    blocking_attn: &std::collections::HashSet<String>,
) -> crate::toast::Class {
    use bn_core::wake::FleetTransition as T;
    match wake.transition {
        // Work has stopped and it is yours: parked, failed, stuck, or an escalation whose reason
        // did not map to a more specific transition.
        T::Blocked | T::Failed | T::Stuck | T::Waiting => crate::toast::Class::Blocking,
        // Finished work awaiting a look — the review inbox, not an interruption.
        T::Done | T::PrOpened => crate::toast::Class::Completion,
        // An attention item is blocking only when it is open and urgent; warn and agent-sourced
        // items stay in `bn next` and the tmux badge.
        T::Attention => {
            if blocking_attn.contains(&wake.event_id) {
                crate::toast::Class::Blocking
            } else {
                crate::toast::Class::Lifecycle
            }
        }
        // Never published — `FleetTransition::is_actionable` filters it out at classify time.
        T::Progress => crate::toast::Class::Lifecycle,
    }
}

/// `bn watch` — the cheap (no-LLM) wake reactor. Each pass classifies what needs you (fleet
/// escalations + urgent attention + agent completions), publishes actionable transitions to the
/// durable wake queue, and fires a desktop toast for new deliveries. Between passes the shared
/// event loop waits on queue and harness-log filesystem notifications; `interval` is only the
/// bounded fallback needed for time-based stuck transitions. `--once` retains the cron/manual
/// probe path.
pub(crate) fn emit_watch(cfg: &Config, once: bool, interval: u64, stuck: i64, as_json: bool) {
    use bn_core::agent::DEFAULT_IDLE_MINS;
    use std::time::Duration;

    let queue = bn_core::wake::WakeQueue::default();
    let target = bn_core::wake::WakeTarget::Watch;
    let (claude_root, copilot_root) = bn_core::agent::supervision_roots();
    let mut reactor = (!once).then(|| {
        crate::event_loop::EventLoop::with_wake_queue(
            "bn watch",
            vec![crate::event_loop::Watch::file(
                bn_core::events::log_path(),
            )],
            &[claude_root, copilot_root],
        )
    });
    loop {
        let now = bn_core::util::now_epoch_secs();
        let mut watching =
            match agent_live::publish_actionable_wakes(cfg, DEFAULT_IDLE_MINS, stuck, now) {
                Ok((_, current)) => current,
                Err(error) => {
                    eprintln!("bn watch: publish wake transitions: {error}");
                    0
                }
            };
        let attn = publish_attention_wakes(&today(), now);
        watching += attn.relevant;
        let new = match queue.pending(&target) {
            Ok(items) => items,
            Err(error) => {
                eprintln!("bn watch: read wake queue: {error}");
                Vec::new()
            }
        };

        if !new.is_empty() {
            // One interrupt per pass, never one per item: the survivors of the gate are folded
            // into a single toast, and the headline counts *those*, so "N need you" is finally
            // true. Everything held back stays in the queue's other consumers and in `bn next`.
            let candidates: Vec<_> = new
                .iter()
                .map(|item| {
                    bn_core::notify::policy::Notification::new(
                        wake_class(item, &attn.blocking),
                        &item.event_id,
                        "watch",
                        "bn",
                        &item.summary,
                    )
                })
                .collect();
            bn_core::notify::policy::deliver_batch(
                cfg,
                &candidates,
                &|n| format!("bn — {n} need you"),
                " · ",
                now,
            );
        }

        if as_json {
            let items: Vec<_> = new
                .iter()
                .map(|item| {
                    serde_json::json!({
                        "key": item.event_id,
                        "label": item.summary,
                        "transition": item.transition,
                    })
                })
                .collect();
            let data = serde_json::json!({ "new": items, "watching": watching });
            println!("{}", Envelope::success("watch", data).to_json());
        } else if !new.is_empty() {
            println!(
                "● {} new — {}",
                new.len(),
                new.iter()
                    .map(|item| item.summary.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ")
            );
        }
        if !new.is_empty() {
            let ids = new.iter().map(|item| item.event_id.clone()).collect();
            if let Err(error) = queue.acknowledge(&target, &ids) {
                eprintln!("bn watch: acknowledge wake queue: {error}");
            }
        }

        if once {
            break;
        }
        let timeout = Duration::from_secs(interval.max(1));
        if reactor.as_mut().is_some_and(|reactor| {
            reactor.wait(timeout) == crate::event_loop::LoopEvent::Shutdown
        }) {
            break;
        }
    }
}


