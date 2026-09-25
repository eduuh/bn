//! Fuzzy ranking, wrapping `nucleo-matcher` — the matcher Helix uses, and the closest thing to
//! fzf's scoring that exists as a library.
//!
//! # Why the empty query is a special case
//!
//! The caller hands us rows in a meaningful order: `tat`'s list arrives sorted by zoxide frecency,
//! `jump`'s by urgency. With no query typed there is nothing to score, so we return that order
//! untouched — the picker opens on the caller's ranking, not on an alphabetical accident.
//!
//! With a query, ties break by input index, which is the *same* ranking one level down: two rows
//! that fuzzy-match equally well appear most-frecent-first. That single `then_with` is the whole
//! "frecency survives filtering" contract.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// One row that matched, and how well.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Match {
    /// Index into the haystack slice that was ranked.
    pub(crate) idx: usize,
    pub(crate) score: u32,
}

/// A reusable matcher. Construct **once** per picker: `Matcher` owns scratch buffers, and building
/// one per keystroke is the classic way to make nucleo slow.
pub(crate) struct Ranker {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Ranker {
    /// `paths` turns on the path-aware bonus — the right choice when haystacks are `repo/branch`
    /// style, which is every row `tat` shows.
    pub(crate) fn new(paths: bool) -> Self {
        let mut config = Config::DEFAULT;
        if paths {
            config.set_match_paths();
        }
        Self {
            matcher: Matcher::new(config),
            buf: Vec::new(),
        }
    }

    /// Rank `haystacks` against `query`, best first.
    ///
    /// The query is parsed with nucleo's fzf-compatible grammar, so `'exact`, `^prefix`, `suffix$`
    /// and `!negate` keep working for anyone who has them in their fingers.
    pub(crate) fn rank(&mut self, query: &str, haystacks: &[String]) -> Vec<Match> {
        if query.trim().is_empty() {
            return haystacks
                .iter()
                .enumerate()
                .map(|(idx, _)| Match { idx, score: 0 })
                .collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut out: Vec<Match> = haystacks
            .iter()
            .enumerate()
            .filter_map(|(idx, h)| {
                let utf32 = Utf32Str::new(h, &mut self.buf);
                pattern
                    .score(utf32, &mut self.matcher)
                    .map(|score| Match { idx, score })
            })
            .collect();
        // Descending score; ties by input index, so the caller's ordering (frecency, urgency)
        // decides between equally-good matches instead of the sort being arbitrary.
        out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.idx.cmp(&b.idx)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hay(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn ranked(r: &mut Ranker, q: &str, items: &[&str]) -> Vec<String> {
        let h = hay(items);
        r.rank(q, &h)
            .into_iter()
            .map(|m| h[m.idx].clone())
            .collect()
    }

    /// The frecency contract: an empty query must not reorder anything.
    #[test]
    fn an_empty_query_preserves_the_callers_order() {
        let mut r = Ranker::new(true);
        assert_eq!(
            ranked(&mut r, "", &["zebra", "alpha", "middle"]),
            vec!["zebra", "alpha", "middle"]
        );
        assert_eq!(
            ranked(&mut r, "   ", &["zebra", "alpha"]),
            vec!["zebra", "alpha"]
        );
    }

    /// ...and the same contract one level down: equal matches stay in the caller's order.
    #[test]
    fn equal_matches_tie_break_by_input_order() {
        let mut r = Ranker::new(true);
        // Two identical haystacks can only be separated by their index.
        let out = ranked(&mut r, "bn", &["bn", "bn"]);
        assert_eq!(out.len(), 2);
        let h = hay(&["bn", "bn"]);
        let ms = r.rank("bn", &h);
        assert_eq!(ms[0].idx, 0);
        assert_eq!(ms[1].idx, 1);
    }

    #[test]
    fn a_subsequence_matches_and_a_non_match_is_dropped() {
        let mut r = Ranker::new(true);
        assert_eq!(
            ranked(&mut r, "btf", &["bn-tat-fix", "unrelated"]),
            vec!["bn-tat-fix"]
        );
        assert!(ranked(&mut r, "zzz", &["bn-tat-fix"]).is_empty());
    }

    #[test]
    fn matching_is_smart_case() {
        let mut r = Ranker::new(true);
        // Lowercase query is case-insensitive...
        assert_eq!(ranked(&mut r, "bn", &["BN"]), vec!["BN"]);
        // ...an uppercase one is not.
        assert!(ranked(&mut r, "BN", &["bn"]).is_empty());
    }

    #[test]
    fn a_closer_match_outranks_a_scattered_one() {
        let mut r = Ranker::new(true);
        let out = ranked(&mut r, "tat", &["a-t-a-t-here", "tat"]);
        assert_eq!(out.first().map(String::as_str), Some("tat"));
    }
}
