//! Shared server state handed to every axum handler.
//!
//! The resolved [`Config`], the process start time (health uptime), and the SSE fan-out sender. The
//! server no longer holds a web-token credential: it never gates requests on a token (#523). The
//! single [`crate::watch`] task broadcasts frames on `tx`; each `GET /api/stream` client subscribes
//! its own receiver, so N browsers share ONE set of file reads (design §2.2).

use crate::view_cache::ViewCache;
use crate::watch::StreamFrame;
use bn_core::resolver::Config;
use std::sync::Arc;
use tokio::sync::broadcast;

/// How many frames a slow SSE client may lag behind before it starts dropping them (it then
/// reconnects and re-seeds from the REST endpoints). Generous for the 1–2s frame cadence.
const STREAM_CAPACITY: usize = 256;

/// Cloneable application state. `axum` clones it per request, so the heavy [`Config`] sits behind an
/// `Arc` — cloning is a refcount bump, not a deep copy. The broadcast [`broadcast::Sender`] is
/// itself cheap to clone and shared by the watcher and every handler.
#[derive(Clone)]
pub struct AppState {
    /// The resolved bn configuration (roots, hostname, `[web]` settings, …).
    pub cfg: Arc<Config>,
    /// Server start time (epoch seconds), for `GET /api/health`'s uptime.
    pub started_at: i64,
    /// SSE fan-out: the watcher sends [`StreamFrame`]s here; `/api/stream` clients subscribe.
    pub tx: broadcast::Sender<Arc<StreamFrame>>,
    /// Short-TTL cache for the expensive read-only views, so the shell's ten-read page load stops
    /// recomputing what it computed seconds ago (see [`crate::view_cache`]).
    pub views: ViewCache,
}

impl AppState {
    /// Build the shared state from a resolved config.
    pub fn new(cfg: Config) -> AppState {
        let (tx, _rx) = broadcast::channel(STREAM_CAPACITY);
        AppState {
            cfg: Arc::new(cfg),
            started_at: bn_core::util::now_epoch_secs(),
            tx,
            views: ViewCache::new(),
        }
    }
}
