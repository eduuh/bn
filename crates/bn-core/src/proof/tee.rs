//! A bounded capture of a command's output, for attaching to a proof. Keeps the head (how it
//! started: what ran, with what config) and the tail (how it ended: the failure, the summary),
//! and drops the middle with a marker saying how much, so a runaway build log can't bloat the
//! notes tree.

/// Bytes kept from the start of the output.
pub const HEAD_BYTES: usize = 64 * 1024;
/// Bytes kept from the end.
pub const TAIL_BYTES: usize = 192 * 1024;

#[derive(Debug, Default, Clone)]
pub struct CappedLog {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    total: u64,
}

impl CappedLog {
    pub fn push(&mut self, mut bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let room = HEAD_BYTES.saturating_sub(self.head.len());
        if room > 0 {
            let take = room.min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        self.tail.extend(bytes);
        let over = self.tail.len().saturating_sub(TAIL_BYTES);
        self.tail.drain(..over);
    }

    /// Everything seen, in bytes.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The kept bytes, and whether anything was dropped.
    pub fn finish(&self) -> (Vec<u8>, bool) {
        let kept = (self.head.len() + self.tail.len()) as u64;
        let dropped = self.total - kept;
        let mut out = self.head.clone();
        if dropped > 0 {
            out.extend_from_slice(format!("\n… {dropped} bytes truncated …\n").as_bytes());
        }
        out.extend(self.tail.iter());
        (out, dropped > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_is_kept_whole() {
        let mut log = CappedLog::default();
        log.push(b"hello ");
        log.push(b"world");
        assert_eq!(log.finish(), (b"hello world".to_vec(), false));
    }

    #[test]
    fn big_output_keeps_head_and_tail_and_says_what_it_dropped() {
        let mut log = CappedLog::default();
        log.push(&vec![b'a'; HEAD_BYTES]);
        log.push(&vec![b'b'; 1000]);
        log.push(&vec![b'c'; TAIL_BYTES]);
        let (out, truncated) = log.finish();
        assert!(truncated);
        assert_eq!(log.total(), (HEAD_BYTES + 1000 + TAIL_BYTES) as u64);
        assert!(out.starts_with(&vec![b'a'; HEAD_BYTES]));
        assert!(out.ends_with(&vec![b'c'; TAIL_BYTES]));
        let marker = String::from_utf8_lossy(&out[HEAD_BYTES..HEAD_BYTES + 40]).to_string();
        assert!(marker.contains("1000 bytes truncated"), "{marker}");
    }
}
