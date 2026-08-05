//! The embedded UI bundle.
//!
//! `build.rs` populates `$OUT_DIR/ui-dist` with either the real `ui/dist` (when the frontend was
//! built) or a single placeholder page, so this derive always has a directory to bake in and a
//! source build never needs node. `debug-embed` (in Cargo.toml) forces embedding in every profile,
//! so an installed / copied `bn-web` serves its UI without the source tree present.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$OUT_DIR/ui-dist"]
pub struct Assets;

/// Look up an embedded asset by path (re-exported through the [`Embed`] trait). A thin wrapper so
/// callers don't each need to import the trait.
pub fn get(path: &str) -> Option<rust_embed::EmbeddedFile> {
    Assets::get(path)
}

/// Whether the real built UI (not the "UI not built" placeholder) is embedded in this binary.
/// Set by `build.rs` from the presence of `ui/dist/index.html` at compile time.
pub fn ui_built() -> bool {
    option_env!("BN_WEB_UI_BUILT") == Some("1")
}
