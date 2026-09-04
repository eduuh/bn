//! Stable, dependency-free fingerprint hashing for anything bn persists or derives a path from.

/// FNV-1a 64-bit — deterministic and stable across processes, platforms, and compiler versions,
/// unlike `std`'s `DefaultHasher` (SipHash), which carries **no** cross-version stability guarantee
/// and must never back anything persisted between runs. Dependency-free by design: this hash only
/// needs to be a stable fingerprint, not cryptographically strong.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}
