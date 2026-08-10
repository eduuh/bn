//! The suggestions read API (Today's agent, #577) — the proposal list, joined to what it produced.
//!
//! Its own module because a suggestion is no longer a standalone row: accepting one queues the work,
//! so serving it means joining the store against the backlog. That join is the interesting part and
//! it wants somewhere to live and be read.
use axum::extract::Query;
use axum::Json;
use bn_core::render::Envelope;
use serde::Deserialize;
use serde_json::json;

use super::nonempty;

#[derive(Debug, Deserialize)]
pub struct SuggestionsQuery {
    /// Restrict to one status: `suggested` | `accepted` | `dismissed` | `done` (else all).
    pub status: Option<String>,
    /// Most-recent-N cap (default 50, 0 = uncapped).
    pub limit: Option<usize>,
}

/// `GET /api/suggestions?status=&limit=` — Today's agent's suggested tasks, newest-first. Each
/// carries its source finding + rationale, so the Today page can render a reviewable proposal list.
///
/// Accepted ones also carry `queue`: the backlog entry accepting them produced, joined live through
/// the shared [`bn_core::suggestions::queue_view`] the CLI uses. Serving the suggestion's own cached
/// `queued_id` instead would let the page claim work is running after its entry was cancelled.
pub async fn suggestions(Query(q): Query<SuggestionsQuery>) -> Json<Envelope> {
    let limit = q.limit.unwrap_or(50);
    let status = nonempty(q.status).and_then(|s| bn_core::suggestions::SuggestionStatus::parse(&s));
    let items = bn_core::suggestions::list(status, limit);
    let backlog = bn_core::backlog::load(&bn_core::backlog::backlog_path());
    let rows: Vec<serde_json::Value> = items
        .iter()
        .map(|s| {
            let mut row = serde_json::to_value(s).unwrap_or_else(|_| json!({}));
            if let Some(obj) = row.as_object_mut() {
                obj.insert(
                    "queue".to_string(),
                    serde_json::to_value(bn_core::suggestions::queue_view(s, &backlog))
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            row
        })
        .collect();
    Json(Envelope::success(
        "suggestions",
        json!({ "count": rows.len(), "suggestions": rows }),
    ))
}
