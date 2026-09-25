//! The Forge app (forge.wira360.com) as bn's task store.
//!
//! Named `forge_app`, not `forge`: [`crate::forge`] is the unrelated git-host (GitHub / Azure
//! DevOps) abstraction.
//!
//! Three layers, each replaceable on its own:
//!
//! - [`client`]: a small blocking MCP client over streamable HTTP ([`client::McpHttp`]) and the
//!   [`client::ForgeApi`] trait naming the Forge tools bn calls. Tests use a fake `ForgeApi`.
//! - [`store`]: [`store::ForgeTaskStore`], the [`crate::task_store::TaskStore`] that maps Forge
//!   activities to bn tasks on load and pushes only what changed on save.
//! - [`session`]: one client per endpoint and key per process, so stores share an MCP session.
//! - [`ids`]: the per-endpoint high-water mark that keeps bn ids from being reused.
//! - this module: where the credential comes from ([`resolve_token`], [`write_token`]).
//!
//! The credential is a `fam_` API key minted for the family's agent member (`bn forge login`). It
//! travels only in the `Authorization` header and is never logged or printed.

pub mod client;
pub mod ids;
mod mapping;
pub mod session;
pub mod store;

pub use mapping::repo_slug;

use std::io;
use std::path::{Path, PathBuf};

/// The production Forge MCP endpoint: `[tasks] forge_url` when unset.
pub const DEFAULT_FORGE_URL: &str = "https://api.wira360.com/mcp";

/// The environment variable that carries a Forge key and wins over the token file.
pub const TOKEN_ENV: &str = "BN_FORGE_TOKEN";

/// Where the Forge key came from. Carries the file path, never the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    /// `BN_FORGE_TOKEN`.
    Env,
    /// `<config_dir>/forge-token`.
    File(PathBuf),
}

impl TokenSource {
    /// A one-line description for status output: `env BN_FORGE_TOKEN` or `file <path>`.
    pub fn describe(&self) -> String {
        match self {
            TokenSource::Env => format!("env {TOKEN_ENV}"),
            TokenSource::File(p) => format!("file {}", p.display()),
        }
    }
}

/// `<config_dir>/forge-token`, the persisted Forge key (mode 0600 on unix).
pub fn token_path() -> PathBuf {
    crate::resolver::config_dir().join("forge-token")
}

/// The Forge key and where it came from: a non-blank `BN_FORGE_TOKEN` first, then a non-blank
/// token file. `None` when neither holds one.
pub fn resolve_token() -> Option<(String, TokenSource)> {
    if let Ok(v) = std::env::var(TOKEN_ENV) {
        let v = v.trim();
        if !v.is_empty() {
            return Some((v.to_string(), TokenSource::Env));
        }
    }
    let path = token_path();
    read_token_file(&path).map(|t| (t, TokenSource::File(path)))
}

fn read_token_file(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let t = raw.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// The error every Forge call returns when there is no key to send.
pub fn missing_token_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "no Forge key: run `bn forge login` (or `bn forge login --paste`), or set {TOKEN_ENV}"
        ),
    )
}

/// Persist `key` to [`token_path`], replacing any previous key. The file is created 0600 on unix
/// (and tightened to 0600 if it already existed), so the key is never world-readable, not even
/// for a moment.
pub fn write_token(key: &str) -> io::Result<PathBuf> {
    let path = token_path();
    write_token_at(&path, key)?;
    Ok(path)
}

fn write_token_at(path: &Path, key: &str) -> io::Result<()> {
    use std::io::Write;
    let key = key.trim();
    if key.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to write an empty Forge key",
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(key.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// `true` when `key` looks like a Forge family API key (`fam_…`). A shape check for friendlier
/// errors, not validation: the server is the authority.
pub fn looks_like_key(key: &str) -> bool {
    let k = key.trim();
    k.len() > 8 && k.starts_with("fam_") && !k.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_file_is_written_private_and_read_back_trimmed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfg").join("forge-token");
        write_token_at(&path, "  fam_abcdef123  \n").expect("write");
        assert_eq!(read_token_file(&path).as_deref(), Some("fam_abcdef123"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(write_token_at(&path, "   ").is_err());
    }

    #[test]
    fn a_blank_token_file_is_no_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("forge-token");
        std::fs::write(&path, "\n  \n").unwrap();
        assert_eq!(read_token_file(&path), None);
        assert_eq!(read_token_file(&dir.path().join("absent")), None);
    }

    #[test]
    fn key_shape() {
        assert!(looks_like_key("fam_0123456789abcdef"));
        assert!(!looks_like_key("wfm_0123456789abcdef"));
        assert!(!looks_like_key("fam_"));
        assert!(!looks_like_key("fam_abc def123"));
    }

    #[test]
    fn source_description_never_carries_the_key() {
        assert_eq!(TokenSource::Env.describe(), "env BN_FORGE_TOKEN");
        let d = TokenSource::File(PathBuf::from("/x/forge-token")).describe();
        assert_eq!(d, "file /x/forge-token");
    }
}
