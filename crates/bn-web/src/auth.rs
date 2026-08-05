//! Web-token bootstrap (design §5).
//!
//! On first `bn serve`, generate 32 random bytes, hex-encode them, and persist to
//! `<config_dir>/web-token` (mode 0600 on unix). `--show-token` prints it so an external dev tunnel
//! can pair against it. The server itself no longer gates requests on the token (#523): a loopback
//! bind and a trusted LAN are the trust boundary, and external exposure is fronted by an
//! authenticated dev tunnel, so bn-web never requires a token — on loopback or `--lan`.

use rand::RngCore;
use std::path::PathBuf;

/// Path to the persisted web token.
pub fn token_path() -> PathBuf {
    bn_core::resolver::config_dir().join("web-token")
}

/// Return the existing web token, or generate + persist a fresh one. Idempotent: a non-empty
/// token file is reused so a paired UI keeps working across restarts.
pub fn ensure_token() -> anyhow::Result<String> {
    let path = token_path();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let token = random_token();

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, &token)?;
    restrict_permissions(&path);
    Ok(token)
}

/// A fresh 32-byte random token, hex-encoded — the credential's raw form. Used to seed the token
/// file, and as an in-memory fallback when the file can't be persisted (the server still gates
/// writes; the operator just can't pair a UI until the file is writable).
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

/// Lowercase hex, no external `hex` crate.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Best-effort 0600 on the token file (unix only) — it's a credential.
fn restrict_permissions(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encode_is_lowercase_and_padded() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xff, 0xa0]), "000fffa0");
        assert_eq!(hex_encode(&[0u8; 32]).len(), 64);
    }
}
