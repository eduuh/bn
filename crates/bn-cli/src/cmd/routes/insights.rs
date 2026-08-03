use crate::*;

pub(super) fn route(cfg: &Config, action: CmdInsights, json: bool) {
    match action {
        CmdInsights::Search { query, limit } => emit_search(cfg, &query.join(" "), limit, json),
        CmdInsights::Recall {
            query,
            repo,
            index,
            limit,
        } => emit_recall(cfg, &query.join(" "), repo.as_deref(), index, limit, json),
        CmdInsights::Related { branch, limit } => emit_related(cfg, branch.as_deref(), limit, json),
        CmdInsights::Reviews { path } => {
            if path {
                println!("{}", bn_core::reviews::cache_path().display());
            } else {
                emit_reviews(cfg, json);
            }
        }
        CmdInsights::Feed { action } => match action {
            None => emit_feed(json),
            Some(FeedCmd::Ingest {
                source,
                attn,
                severity,
            }) => feed_ingest(&source, attn, &severity),
            Some(FeedCmd::Clear { source }) => feed_clear(&source),
            Some(FeedCmd::Rate { card, rating, note }) => {
                feed_rate(cfg, &card, &rating, note.as_deref(), json)
            }
        },
        CmdInsights::Findings { action } => match action {
            FindingsCmd::Record {
                source,
                run_id,
                raw,
            } => findings_record(&source, run_id.as_deref(), raw.as_deref()),
            FindingsCmd::Query {
                source,
                since,
                date,
                limit,
            } => findings_query(
                source.as_deref(),
                since.as_deref(),
                date.as_deref(),
                limit,
                json,
            ),
            FindingsCmd::Sources => findings_sources(json),
            FindingsCmd::Gather => findings_gather(cfg, json),
        },
        CmdInsights::Today { action } => match action {
            None
            | Some(TodayCmd::Run {
                lookback_days: None,
                per_source: None,
            }) => today_run(None, None, json),
            Some(TodayCmd::Run {
                lookback_days,
                per_source,
            }) => today_run(lookback_days, per_source, json),
            Some(TodayCmd::List { status, limit }) => today_list(status.as_deref(), limit, json),
            Some(TodayCmd::Accept { id }) => {
                today_set_status(&id, bn_core::suggestions::SuggestionStatus::Accepted, json)
            }
            Some(TodayCmd::Dismiss { id }) => {
                today_set_status(&id, bn_core::suggestions::SuggestionStatus::Dismissed, json)
            }
            Some(TodayCmd::Agent { action }) => match action {
                TodayAgentCmd::Start {
                    harness,
                    lookback_days,
                } => today_agent_start(cfg, harness.as_deref(), lookback_days, json),
                TodayAgentCmd::Stop => today_agent_stop(json),
                TodayAgentCmd::Status => today_agent_status(cfg, json),
            },
        },
        CmdInsights::Metrics {
            since,
            snapshot,
            trend,
        } => emit_metrics(cfg, since, snapshot, trend, json),
        CmdInsights::Event { kind, fields, ts } => emit_event(cfg, &kind, &fields, ts, json),
        CmdInsights::Doctor {
            stale_days,
            brief,
            pick,
            pick_stage,
            pick_fields,
        } => {
            if let Some(s) = pick_stage {
                picker::stage(cfg, s, &pick_fields)
            } else if pick {
                picker::run(cfg)
            } else {
                emit_doctor(cfg, stale_days, brief, json)
            }
        }
        CmdInsights::Next {
            limit,
            brief,
            cached,
            refresh,
            clear_cache,
            no_momentum,
            fit,
        } => {
            // #503: fold an opportunistic fleet-GC sweep into the interactive `bn next` — the
            // moment you're deciding what to do next is a good time to clear stale stragglers.
            // Skipped on the fast/automated paths (--cached picker, --brief hook, --refresh cron,
            // --clear-cache) so those stay instant and side-effect-free.
            if !brief && !cached && !refresh && !clear_cache {
                crate::reap::sweep(cfg);
            }
            emit_next(
                cfg,
                limit,
                brief,
                cached,
                refresh,
                clear_cache,
                no_momentum,
                fit,
                json,
            )
        }
        CmdInsights::Gap { action } => match action {
            Some(GapCmd::Ingest { date }) => gap_ingest(date, json),
            None => emit_gap(json),
        },
    }
}
