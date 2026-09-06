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
        CmdInsights::Reviews { path, fetch } => {
            if path {
                println!("{}", bn_core::reviews::cache_path().display());
            } else if fetch {
                std::process::exit(fetch_review_queue());
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
