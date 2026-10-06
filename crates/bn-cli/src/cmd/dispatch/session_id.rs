//! Session ids for launched agents: a real UUID, from `uuidgen` or minted here.

/// A fresh lowercase UUID (via `uuidgen`, universal on macOS/Linux) to pin an agent's session id so
/// `bn agent send` can steer it via `-p --resume`. Falls back to a self-generated UUIDv4 when
/// `uuidgen` is absent (#646): copilot ≥1.0.71 rejects a `--session-id` that isn't a valid UUID, so
/// the old `bn-<pid>-<ts>` fallback broke steering entirely — the fallback must still be a real UUID.
pub(crate) fn gen_uuid() -> String {
    std::process::Command::new("uuidgen")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_lowercase())
        .filter(|s| is_uuid(s))
        .unwrap_or_else(fallback_uuid)
}

/// True if `s` is a canonical 8-4-4-4-12 hex UUID string (the shape copilot validates).
pub(crate) fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// A self-generated RFC 4122 v4 UUID for when `uuidgen` isn't on PATH. Draws entropy from
/// `/dev/urandom` when available, else derives 16 bytes from a splitmix64 seeded by wall-clock nanos
/// and pid — either way the version/variant bits are stamped so the result is a *valid* UUID.
fn fallback_uuid() -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut bytes)
        })
        .is_ok();
    if !filled {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut state = nanos ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
        for chunk in bytes.chunks_mut(8) {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            let b = z.to_le_bytes();
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
    }
    format_uuid_v4(bytes)
}

/// Stamp the version (4) and variant (RFC 4122) bits on 16 bytes and format as a canonical UUID.
fn format_uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let mut s = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}
