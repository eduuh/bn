//! Build script: guarantee a folder for `rust-embed` to bake in, so `cargo build` never needs
//! node.
//!
//! `rust-embed` embeds a directory at compile time; a missing directory is a hard compile error.
//! We point the `Assets` derive at `$OUT_DIR/ui-dist` (interpolated in `assets.rs`) and populate
//! it here: copy the real `ui/dist` when the frontend has been built (`npm run build` in `ui/`, or
//! CI before the release build), otherwise write a single "UI not built" placeholder page. Either
//! way the embedded folder always exists, so a source build with no node still compiles and serves
//! a usable page.

use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ui_dist = manifest_dir.join("ui").join("dist");
    let embed_dir = out_dir.join("ui-dist");

    // Rebuild whenever the built UI changes (or first appears / disappears).
    println!("cargo:rerun-if-changed={}", ui_dist.display());
    println!("cargo:rerun-if-changed=build.rs");

    // Start from a clean embed dir so a stale placeholder never lingers beside real assets.
    let _ = std::fs::remove_dir_all(&embed_dir);
    std::fs::create_dir_all(&embed_dir).expect("create embed dir");

    if ui_dist.join("index.html").is_file() {
        copy_dir(&ui_dist, &embed_dir);
        println!("cargo:rustc-env=BN_WEB_UI_BUILT=1");
    } else {
        std::fs::write(embed_dir.join("index.html"), PLACEHOLDER_HTML)
            .expect("write placeholder index.html");
        println!("cargo:rustc-env=BN_WEB_UI_BUILT=0");
    }
}

/// Recursively copy `src` into `dst` (both must exist). Skeleton-simple: the built UI is a handful
/// of hashed asset files, so no need for anything cleverer.
fn copy_dir(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).expect("read ui/dist") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            std::fs::create_dir_all(&target).expect("create subdir");
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copy asset");
        }
    }
}

/// Served when the frontend hasn't been built. Self-contained (no external assets) so it renders
/// even though nothing else is embedded.
const PLACEHOLDER_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>bn-web — UI not built</title>
    <style>
      :root { color-scheme: dark; }
      body {
        margin: 0; min-height: 100vh; display: grid; place-items: center;
        background: #0d1117; color: #c9d1d9;
        font: 16px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace;
      }
      main { max-width: 42rem; padding: 2rem; }
      h1 { color: #58a6ff; font-size: 1.4rem; margin: 0 0 1rem; }
      code { background: #161b22; padding: 0.15em 0.4em; border-radius: 4px; }
      a { color: #58a6ff; }
    </style>
  </head>
  <body>
    <main>
      <h1>bn-web is running — the UI isn't built yet</h1>
      <p>
        The server and its JSON API (e.g. <code>GET /api/health</code>) are live, but the
        embedded single-page app wasn't compiled into this binary.
      </p>
      <p>Build it, then rebuild <code>bn-web</code>:</p>
      <p><code>cd crates/bn-web/ui &amp;&amp; npm ci &amp;&amp; npm run build</code></p>
      <p>Released binaries bake the UI in via CI, so this page only shows on a bare source build.</p>
    </main>
  </body>
</html>
"#;
