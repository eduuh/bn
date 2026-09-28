//! `bn queue` — the CLI over [`bn_core::coord`]. The verbs and their rules live there; this file
//! resolves who is acting, runs the verb, mirrors the result into the destination's note, and
//! renders it.

use crate::cli::coord_group::{AsSeat, CloseArgs};
use crate::*;
use bn_core::clock::RealClock;
use bn_core::coord::{
    self, Block, CloseOutcome, CoordDb, CoordError, Handoff, ListFilter, MirrorOutcome, NewItem,
    QItem, QState, Tier, Trigger,
};
use serde_json::json;

mod render;
pub(crate) mod sweep;
use render::{print_item, print_rows, print_transitions};

const CMD: &str = "queue";

pub(crate) fn emit_queue(cfg: &Config, action: Option<QueueCmd>, json: bool) {
    let mut db = CoordDb::open_default().unwrap_or_else(|e| die(json, &e));
    let clock = RealClock;
    let action = action.unwrap_or(QueueCmd::List {
        seat: None,
        all: false,
        state: None,
        overdue: false,
        limit: None,
        who: AsSeat::default(),
    });
    match action {
        QueueCmd::Create {
            seat,
            body,
            tier,
            key,
            ref_,
            no_nudge,
            who,
        } => {
            let new = NewItem {
                dest: seat,
                source: acting(cfg, &who, json),
                body: body.join(" "),
                tier: tier_arg(&tier, json),
                idem_key: key,
                ref_,
            };
            let (item, created) = db.create(&clock, &new).unwrap_or_else(|e| die(json, &e));
            let mirror = if created {
                mirror_open(cfg, &mut db, &item)
            } else {
                None
            };
            let told = (created && !no_nudge).then(|| notify(cfg, &mut db, &item));
            let item = db.get(item.num).unwrap_or_else(|e| die(json, &e));
            let verb = if created { "created" } else { "exists" };
            done_one(json, verb, &item, mirror);
            told_line(json, told.flatten());
        }
        QueueCmd::Claim { id, who } => {
            let actor = acting(cfg, &who, json);
            let item = db
                .claim(&clock, qnum(&id, json), &actor)
                .unwrap_or_else(|e| die(json, &e));
            done_one(json, "claimed", &item, None);
        }
        QueueCmd::Unclaim { id, note, who } => {
            let actor = acting(cfg, &who, json);
            let item = db
                .unclaim(&clock, qnum(&id, json), &actor, &note)
                .unwrap_or_else(|e| die(json, &e));
            done_one(json, "unclaimed", &item, None);
        }
        QueueCmd::Handoff {
            id,
            to,
            note,
            complete,
            tier,
            ref_,
            no_nudge,
            who,
        } => {
            let actor = acting(cfg, &who, json);
            let h = Handoff {
                to,
                note,
                tier: tier.map(|t| tier_arg(&t, json)),
                ref_,
                complete,
            };
            let (src, new) = db
                .handoff(&clock, qnum(&id, json), &actor, &h)
                .unwrap_or_else(|e| die(json, &e));
            let closed = mirror_close(cfg, &mut db, &src);
            let opened = mirror_open(cfg, &mut db, &new);
            let told = (!no_nudge).then(|| notify(cfg, &mut db, &new)).flatten();
            let new = db.get(new.num).unwrap_or_else(|e| die(json, &e));
            if json {
                let data = json!({"from": src, "to": new, "mirror": {
                    "closed": closed.as_ref().map(MirrorOutcome::state),
                    "opened": opened.as_ref().map(MirrorOutcome::state),
                }});
                println!("{}", Envelope::success(CMD, data).to_json());
            } else {
                println!("{} {} → {} at {}", src.id, src.state, new.id, new.dest);
                mirror_line(opened.as_ref(), &new);
                told_line(json, told);
            }
        }
        QueueCmd::Done(a) => close(cfg, &mut db, a, CloseOutcome::Done, json),
        QueueCmd::Fail(a) => close(cfg, &mut db, a, CloseOutcome::Failed, json),
        QueueCmd::Deny(a) => close(cfg, &mut db, a, CloseOutcome::Denied, json),
        QueueCmd::Cancel(a) => close(cfg, &mut db, a, CloseOutcome::Canceled, json),
        QueueCmd::Block {
            id,
            on,
            wake_after,
            watchdog,
            summary,
            evidence,
            note,
            who,
        } => {
            let actor = acting(cfg, &who, json);
            let trigger = match (wake_after, watchdog, summary, evidence) {
                (Some(d), None, None, None) => Trigger::WakeAfter(
                    bn_core::focus::parse_duration(&d).unwrap_or_else(|| {
                        fail_json(json, CMD, "invalid_duration", &format!("'{d}' is not a duration"))
                    }),
                ),
                (None, Some(w), None, None) => Trigger::Watchdog(
                    bn_core::coord::watchdog::parse_wdid(&w).unwrap_or_else(|| {
                        fail_json(json, CMD, "invalid_id", &format!("'{w}' is not wd<N>"))
                    }),
                ),
                (None, None, Some(summary), Some(evidence)) => Trigger::Human { summary, evidence },
                _ => fail_json(
                    json,
                    CMD,
                    "no_wake_trigger",
                    "a park needs exactly one wake trigger: --wake-after, --watchdog, or \
                     --summary + --evidence for a person. A silent park looks like a crash.",
                ),
            };
            let block = Block { on, trigger, note };
            let item = db
                .block(&clock, qnum(&id, json), &actor, &block)
                .unwrap_or_else(|e| die(json, &e));
            let told = notify(cfg, &mut db, &item);
            done_one(json, "blocked", &item, None);
            told_line(json, told);
        }
        QueueCmd::Resolve { id, note, who } => {
            let actor = acting(cfg, &who, json);
            let item = db
                .resolve(&clock, qnum(&id, json), &actor, &note)
                .unwrap_or_else(|e| die(json, &e));
            let owner = item.owner.clone().unwrap_or_else(|| item.dest.clone());
            let line = format!("[bn queue {}] unparked by {actor}: {note}", item.id);
            let _ = crate::cmd::agent::try_deliver_live(cfg, &owner, &line);
            done_one(json, "resolved", &item, None);
        }
        QueueCmd::Update {
            id,
            note,
            ref_,
            who,
        } => {
            let actor = acting(cfg, &who, json);
            let item = db
                .update(&clock, qnum(&id, json), &actor, &note, ref_.as_deref())
                .unwrap_or_else(|e| die(json, &e));
            done_one(json, "noted", &item, None);
        }
        QueueCmd::List {
            seat,
            all,
            state,
            overdue,
            limit,
            who,
        } => {
            let seat = match (all, seat) {
                (true, _) => None,
                (false, Some(s)) => Some(coord::normalize_seat(&s).unwrap_or_else(|e| die(json, &e))),
                (false, None) => Some(acting(cfg, &who, json)),
            };
            let filter = ListFilter {
                seat,
                states: states_arg(state.as_deref(), json),
                overdue_at: overdue.then(|| clock_now(&clock)),
                limit,
            };
            let rows = db.list(&filter).unwrap_or_else(|e| die(json, &e));
            if json {
                let data = json!({"seat": filter.seat, "rows": rows});
                println!("{}", Envelope::success(CMD, data).to_json());
            } else {
                print_rows(&rows, filter.seat.as_deref(), clock_now(&clock));
            }
        }
        QueueCmd::Show { id } => {
            let item = db.get(qnum(&id, json)).unwrap_or_else(|e| die(json, &e));
            if json {
                println!("{}", Envelope::success(CMD, json!(item)).to_json());
            } else {
                print_item(&item, clock_now(&clock));
            }
        }
        QueueCmd::Transitions { id } => {
            let num = qnum(&id, json);
            let history = db.transitions(num).unwrap_or_else(|e| die(json, &e));
            if json {
                let data = json!({"id": coord::qid(num), "transitions": history});
                println!("{}", Envelope::success(CMD, data).to_json());
            } else {
                print_transitions(&history);
            }
        }
        QueueCmd::Sweep { dry_run } => {
            let (plan, report) = sweep::sweep_all(cfg, &mut db, clock_now(&clock), dry_run);
            if json {
                let data = json!({"dry_run": dry_run, "report": report, "plan": {
                    "nudge": plan.nudge.iter().map(|n| coord::qid(*n)).collect::<Vec<_>>(),
                    "raise": plan.raise.iter().map(|a| &a.key).collect::<Vec<_>>(),
                    "ack": plan.ack,
                    "mirror_retry": plan.mirror_retry.iter().map(|n| coord::qid(*n)).collect::<Vec<_>>(),
                }});
                println!("{}", Envelope::success(CMD, data).to_json());
            } else if dry_run {
                render::print_plan(&plan);
            } else {
                println!("{}", report.summary());
                for e in &report.errors {
                    eprintln!("  {e}");
                }
            }
        }
        QueueCmd::Whoami { who } => {
            let seat = acting(cfg, &who, json);
            if json {
                println!("{}", Envelope::success(CMD, json!({"seat": seat})).to_json());
            } else {
                println!("{seat}");
            }
        }
    }
}

fn close(cfg: &Config, db: &mut CoordDb, a: CloseArgs, outcome: CloseOutcome, json: bool) {
    let actor = acting(cfg, &a.who, json);
    let item = db
        .close(
            &RealClock,
            qnum(&a.id, json),
            &actor,
            outcome,
            &a.note,
            a.ref_.as_deref(),
        )
        .unwrap_or_else(|e| die(json, &e));
    let mirror = mirror_close(cfg, db, &item);
    done_one(json, item.state.as_str(), &item, mirror);
}

/// The seat this invocation acts as — see [`coord::acting_seat`].
pub(crate) fn acting(cfg: &Config, who: &AsSeat, json: bool) -> String {
    let here = std::env::current_dir()
        .ok()
        .and_then(|cwd| resolver::resolve(cfg, &cwd).ok())
        .map(|r| format!("{}:{}", r.repo, r.branch));
    let env = std::env::var("BN_SEAT").ok();
    let user = std::env::var("USER").unwrap_or_default();
    coord::acting_seat(who.as_seat.as_deref(), env.as_deref(), here.as_deref(), &user)
        .unwrap_or_else(|e| die(json, &e))
}

/// Nudge or alert for a row just opened, returning a line saying which. Best-effort: the row
/// already exists whatever happens here.
fn notify(cfg: &Config, db: &mut CoordDb, item: &QItem) -> Option<String> {
    let report = sweep::notify_new(cfg, db, item, clock_now(&RealClock));
    if !report.nudged.is_empty() {
        Some(format!("  nudged {} in its pane", item.dest))
    } else if !report.raised.is_empty() {
        Some(format!("  raised an attention item for {}", item.dest))
    } else {
        None
    }
}

fn told_line(json: bool, line: Option<String>) {
    if let Some(line) = line.filter(|_| !json) {
        println!("{line}");
    }
}

fn mirror_open(cfg: &Config, db: &mut CoordDb, item: &QItem) -> Option<MirrorOutcome> {
    let out = coord::open_mirror(cfg, item, &today());
    bn_core::util::best_effort(db.set_mirror(item.num, &out), "queue: record mirror");
    Some(out)
}

fn mirror_close(cfg: &Config, db: &mut CoordDb, item: &QItem) -> Option<MirrorOutcome> {
    item.mirror_todo.as_ref()?;
    let out = coord::close_mirror(cfg, item, &today());
    bn_core::util::best_effort(db.set_mirror(item.num, &out), "queue: record mirror");
    Some(out)
}

fn done_one(json: bool, verb: &str, item: &QItem, mirror: Option<MirrorOutcome>) {
    if json {
        let data = json!({"verb": verb, "row": item,
            "mirror": mirror.as_ref().map(MirrorOutcome::state)});
        println!("{}", Envelope::success(CMD, data).to_json());
        return;
    }
    println!("{} {verb} — {} ({})", item.id, item.dest, item.state);
    mirror_line(mirror.as_ref(), item);
}

fn mirror_line(mirror: Option<&MirrorOutcome>, item: &QItem) {
    match mirror {
        Some(MirrorOutcome::Ok(todo)) => println!("  mirrored as {todo} in {}'s note", item.dest),
        Some(MirrorOutcome::Failed(why)) => {
            eprintln!("  the row is saved, but its note todo was not written ({why}); the next sweep retries it")
        }
        _ => {}
    }
}

fn qnum(id: &str, json: bool) -> i64 {
    coord::parse_qid(id).unwrap_or_else(|| {
        fail_json(json, CMD, "invalid_id", &format!("'{id}' is not a queue row id (q<N>)"))
    })
}

fn tier_arg(tier: &str, json: bool) -> Tier {
    Tier::parse(tier).unwrap_or_else(|| {
        fail_json(
            json,
            CMD,
            "invalid_tier",
            &format!("unknown tier '{tier}' — fast, routine, deep or critical"),
        )
    })
}

fn states_arg(raw: Option<&str>, json: bool) -> Vec<QState> {
    match raw.map(str::trim) {
        None | Some("") => vec![],
        Some("any" | "all") => QState::ALL.to_vec(),
        Some(list) => list
            .split(',')
            .map(|s| {
                QState::parse(s).unwrap_or_else(|| {
                    fail_json(json, CMD, "invalid_state", &format!("unknown state '{s}'"))
                })
            })
            .collect(),
    }
}

fn clock_now(clock: &RealClock) -> i64 {
    bn_core::clock::Clock::now_epoch_secs(clock)
}

fn die(json: bool, e: &CoordError) -> ! {
    die_as(CMD, json, e)
}

/// Exit with `e` as `command`'s failure envelope.
pub(crate) fn die_as(command: &str, json: bool, e: &CoordError) -> ! {
    let code = match e {
        CoordError::Sqlite(_) => "db_error",
        CoordError::NotFound(_) => "not_found",
        CoordError::Illegal { .. } => "illegal_transition",
        CoordError::Forbidden(_) => "forbidden",
        CoordError::Invalid(_) => "invalid",
    };
    fail_json(json, command, code, &e.to_string())
}

/// Warn (on stderr) when `ctx`'s seat still holds open queue rows — for `bn agent done`, which
/// would otherwise leave them in-progress with nobody on them. Never fails.
pub(crate) fn warn_open_rows(ctx: &Resolved) {
    let Ok(seat) = coord::normalize_seat(&format!("{}:{}", ctx.repo, ctx.branch)) else {
        return;
    };
    let Ok(db) = CoordDb::open_default() else {
        return;
    };
    let rows = db
        .list(&ListFilter {
            seat: Some(seat.clone()),
            states: vec![QState::InProgress, QState::Blocked],
            ..Default::default()
        })
        .unwrap_or_default();
    let mine: Vec<&str> = rows
        .iter()
        .filter(|r| r.owner.as_deref() == Some(seat.as_str()))
        .map(|r| r.id.as_str())
        .collect();
    if !mine.is_empty() {
        eprintln!(
            "⚠ {seat} still holds queue rows {} — close each with a note (`bn queue done <q> --note …`) \
             or hand it on, or they'll read as abandoned",
            mine.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::*;

    /// Flags after the body must stay flags: a trailing-var-arg body once swallowed
    /// `--tier fast --as …` into the row's text.
    #[test]
    fn create_takes_flags_after_the_body() {
        let cli = Cli::try_parse_from([
            "bn", "queue", "create", "bn:y", "review", "the", "diff", "--tier", "fast", "--as",
            "bn:x",
        ])
        .expect("parses");
        let Some(Command::Coord(CmdCoord::Queue {
            action: Some(QueueCmd::Create {
                seat, body, tier, who, ..
            }),
        })) = cli.command
        else {
            panic!("not a queue create");
        };
        assert_eq!(seat, "bn:y");
        assert_eq!(body, ["review", "the", "diff"]);
        assert_eq!(tier, "fast");
        assert_eq!(who.as_seat.as_deref(), Some("bn:x"));
    }
}
