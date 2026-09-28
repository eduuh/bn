//! A cheap, offline token estimate for text bn is about to hand an agent — a seed prompt, a
//! reference section, a context profile. It is a budget signal, not a meter: the harness's own
//! usage figures stay the truth for what a turn actually cost.

/// Estimated tokens in `text`: characters ÷ 4, rounded up. The usual rule of thumb for English
/// prose and code under current Claude/GPT tokenizers; close enough to warn when a bundle is
/// about to blow its budget, and cheap enough to run on every serve. Counts chars, not bytes, so
/// non-ASCII text isn't inflated.
pub fn estimate(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::estimate;

    #[test]
    fn rounds_up_and_counts_chars_not_bytes() {
        assert_eq!(estimate(""), 0);
        assert_eq!(estimate("abc"), 1);
        assert_eq!(estimate("abcd"), 1);
        assert_eq!(estimate("abcde"), 2);
        // Four 3-byte chars are still one token's worth, not three.
        assert_eq!(estimate("日本語字"), 1);
    }
}
