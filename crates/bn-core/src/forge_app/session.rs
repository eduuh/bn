//! One Forge client per endpoint and key, for the life of the process.
//!
//! Every `task_store(cfg)` builds a fresh [`super::store::ForgeTaskStore`], and the pipeline builds
//! several per step (the gate, the stage note, close-out). A fresh [`HttpForgeApi`] per store would
//! pay `initialize` + `notifications/initialized` before every list: three HTTP requests where one
//! will do. [`api_for`] hands every store for the same `(url, key)` the same client, so the MCP
//! session opened by the first call is reused by the rest. A session the server has dropped is
//! re-opened inside [`super::client::McpHttp`] (HTTP 404), so a long-lived process never holds a
//! dead one.
//!
//! [`register`] puts a client of the caller's choosing under a `(url, key)`, which is how tests
//! stand a fake Forge behind the ordinary `task_store(cfg)` path.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use super::client::{ForgeApi, HttpForgeApi};

type Registry = Mutex<HashMap<(String, u64), Arc<dyn ForgeApi>>>;

fn registry() -> &'static Registry {
    static APIS: OnceLock<Registry> = OnceLock::new();
    APIS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The registry key. The key is hashed so the map never holds a second copy of it.
fn key(url: &str, token: &str) -> (String, u64) {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut h);
    (url.trim().to_string(), h.finish())
}

/// The shared client for `url` and `token`, created on first use.
pub fn api_for(url: &str, token: &str) -> Arc<dyn ForgeApi> {
    let mut apis = registry().lock().unwrap_or_else(|p| p.into_inner());
    apis.entry(key(url, token))
        .or_insert_with(|| Arc::new(HttpForgeApi::new(url.trim(), token)))
        .clone()
}

/// Serve `api` for `url` and `token` from now on, replacing any client already there.
pub fn register(url: &str, token: &str, api: Arc<dyn ForgeApi>) {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key(url, token), api);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_client_per_url_and_key() {
        let a = api_for("http://127.0.0.1:9/session-test", "fam_one");
        let b = api_for("http://127.0.0.1:9/session-test ", "fam_one");
        let c = api_for("http://127.0.0.1:9/session-test", "fam_two");
        assert!(Arc::ptr_eq(&a, &b), "same endpoint and key share a client");
        assert!(
            !Arc::ptr_eq(&a, &c),
            "a different key is a different client"
        );
    }
}
