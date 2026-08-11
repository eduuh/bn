//! The per-model price table and the cache-aware **$ (API-equiv)** benchmark.
//!
//! Split out of [`super`] because pricing is the one part of the cost surface that goes stale on a
//! calendar rather than on a code change: published rates move and new model families ship, and
//! that edit should not mean reading the fold that consumes it.
//!
//! The benchmark is **not a bill** — it is what the same tokens would have cost at public
//! per-model API list prices. That number is only worth printing if each token is priced at the
//! rate it actually bills at, and prompt tokens reach a model four different ways at rates spanning
//! 20×:
//!
//! | prompt token | Anthropic rate, relative to fresh input |
//! |---|---|
//! | fresh input                   | 1×    |
//! | cache **read**                | 0.1×  |
//! | cache **write**, 5-minute TTL | 1.25× |
//! | cache **write**, 1-hour TTL   | 2×    |
//!
//! The two write rows are why this module has the shape it does. A Claude Code turn is mostly cache
//! traffic — a few *fresh* input tokens, tens of thousands of cache reads, and a cache write every
//! time the prefix grows — so the write column is a real share of the bill, and pricing a 1-hour
//! write at the 5-minute rate understates it by 60%. bn captured neither before this module
//! existed: cache writes were dropped outright, understating every Claude session, and the TTL
//! split had nowhere to live. [`TokenUsage`] carries all four buckets so [`cost_usd`] can charge
//! each at its own rate.

use serde::Serialize;

/// One model's list price in USD per 1M tokens, one field per rate the [`TokenUsage`] split bills
/// at. Cache reads are ~10× cheaper than fresh input on both Anthropic and OpenAI; cache writes are
/// *more* expensive than fresh input on Anthropic (the premium buys the entry) and are plain input
/// everywhere else. Output dominates on most runs, but on a long cached agent session the write
/// column can rival it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ModelPrice {
    pub input_per_1m: f64,
    pub cache_read_per_1m: f64,
    /// Writing a cache entry at the default 5-minute TTL.
    pub cache_write_5m_per_1m: f64,
    /// Writing a cache entry at the 1-hour TTL — the one Claude Code takes, and the reason the
    /// TTL split is carried this far rather than collapsed at parse time.
    pub cache_write_1h_per_1m: f64,
    pub output_per_1m: f64,
}

/// The token counts of one run, split the way the price table bills them. All four prompt figures
/// are **disjoint** — a token is fresh input, a cache read, or a cache write, never two of them —
/// which is what makes summing their separately-priced costs correct rather than double-counting.
/// See [`fresh_input`] for the harness normalization that establishes that disjointness.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TokenUsage {
    /// Fresh (non-cached) input, priced at the full rate.
    pub input_tokens: i64,
    /// Prompt tokens served from the provider's cache.
    pub cache_read_tokens: i64,
    /// Prompt tokens written into a 5-minute-TTL cache entry. Also where a harness that reports
    /// cache writes without a TTL lands, since 5 minutes is the providers' default.
    pub cache_write_5m_tokens: i64,
    /// Prompt tokens written into a 1-hour-TTL cache entry.
    pub cache_write_1h_tokens: i64,
    pub output_tokens: i64,
}

impl TokenUsage {
    /// Both cache-write buckets together — the display figure, for surfaces that show *how much*
    /// was written without splitting by TTL. Pricing must not use this: the two bill differently.
    pub fn cache_write_tokens(&self) -> i64 {
        self.cache_write_5m_tokens.max(0) + self.cache_write_1h_tokens.max(0)
    }

    /// Every prompt token this run sent, cached or not — the figure to compare against a context
    /// window, and the one `input_tokens` alone is *not* (it is only the uncached remainder).
    pub fn prompt_tokens(&self) -> i64 {
        self.input_tokens.max(0) + self.cache_read_tokens.max(0) + self.cache_write_tokens()
    }
}

/// Accumulate one turn's usage into a running total. Every bucket sums independently — a session
/// total is the sum of its turns bucket-by-bucket, never a re-derivation, so a parser folding a
/// transcript cannot quietly drop the bucket it forgot to name.
impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, rhs: TokenUsage) {
        self.input_tokens += rhs.input_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
        self.cache_write_5m_tokens += rhs.cache_write_5m_tokens;
        self.cache_write_1h_tokens += rhs.cache_write_1h_tokens;
        self.output_tokens += rhs.output_tokens;
    }
}

/// Anthropic prices the whole family off one input rate: a cache read bills at 0.1× it, a
/// 5-minute cache write at 1.25×, a 1-hour write at 2×. Deriving the three from `input` keeps a
/// price change to a two-number edit and states the multipliers once, where they can be checked,
/// instead of restating them per row where one can silently drift.
const fn anthropic(input: f64, output: f64) -> ModelPrice {
    ModelPrice {
        input_per_1m: input,
        cache_read_per_1m: input * 0.1,
        cache_write_5m_per_1m: input * 1.25,
        cache_write_1h_per_1m: input * 2.0,
        output_per_1m: output,
    }
}

/// A provider that bills a cache write as ordinary input. OpenAI and Google discount the *read*
/// side only: there is no separate write charge, so a miss that populates the cache is just a
/// normal input token. `cache_read` is passed rather than derived because that discount is the
/// provider's own number, not a shared multiplier.
const fn write_as_input(input: f64, cache_read: f64, output: f64) -> ModelPrice {
    ModelPrice {
        input_per_1m: input,
        cache_read_per_1m: cache_read,
        cache_write_5m_per_1m: input,
        cache_write_1h_per_1m: input,
        output_per_1m: output,
    }
}

/// The per-model price table — **the one place** to keep $ rates current. Matched by
/// case-insensitive substring so the many concrete ids of a family (`claude-opus-5`,
/// `claude-opus-5[1m]`, `gpt-5.3-codex`, `gemini-3.1-pro-preview`, …) all resolve to one row.
/// Order matters: the cheap tier needles (`nano`/`mini`/`flash`) come before their base family so
/// `gpt-5-mini` doesn't match the `gpt-5` row, and `flash`/`gemini` precede the generic `mini`
/// because "ge**mini**" contains it. An unrecognized model returns `None` — the caller prices it as
/// unknown (`cost_usd` null), never a panic.
///
/// Anthropic's 1M-token context carries **no long-context premium** as of the Opus 4.7 generation,
/// so a `[1m]` id prices identically to its base model and needs no row of its own.
pub fn model_price(model: &str) -> Option<ModelPrice> {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() {
        return None;
    }
    const TABLE: &[(&str, ModelPrice)] = &[
        ("nano", write_as_input(0.05, 0.005, 0.40)),
        // Google Gemini (before the generic `mini`, which "gemini" contains).
        ("flash", write_as_input(0.30, 0.03, 2.50)),
        ("gemini", write_as_input(1.25, 0.125, 10.00)),
        // OpenAI GPT-5 mini/base.
        ("mini", write_as_input(0.25, 0.025, 2.00)),
        // Anthropic Claude.
        ("haiku", anthropic(1.00, 5.00)),
        ("sonnet", anthropic(3.00, 15.00)),
        ("opus", anthropic(5.00, 25.00)),
        ("fable", anthropic(10.00, 50.00)),
        ("mythos", anthropic(10.00, 50.00)),
        // OpenAI GPT-5 family base + codex variants.
        ("gpt-5", write_as_input(1.25, 0.125, 10.00)),
    ];
    for (needle, price) in TABLE {
        if m.contains(needle) {
            return Some(*price);
        }
    }
    None
}

/// The $ (API-equiv) cost of one run on `model`, or `None` when the model is not in the
/// [`model_price`] table. Each of [`TokenUsage`]'s four prompt buckets is charged at its own rate;
/// negative counts clamp to zero.
pub fn cost_usd(model: &str, usage: TokenUsage) -> Option<f64> {
    let p = model_price(model)?;
    let per_1m = |tokens: i64, rate: f64| (tokens.max(0) as f64) / 1_000_000.0 * rate;
    Some(
        per_1m(usage.input_tokens, p.input_per_1m)
            + per_1m(usage.cache_read_tokens, p.cache_read_per_1m)
            + per_1m(usage.cache_write_5m_tokens, p.cache_write_5m_per_1m)
            + per_1m(usage.cache_write_1h_tokens, p.cache_write_1h_per_1m)
            + per_1m(usage.output_tokens, p.output_per_1m),
    )
}

/// Split a session's raw `input_tokens` into the **non-cached** input to price at the full rate.
/// The harnesses disagree on what `input_tokens` means, and getting it wrong is a silent multiple,
/// not a rounding error:
///
/// * a **Copilot** session reports the *total* prompt there, with cache reads and cache writes as
///   subsets of it, so the fresh part is `input - reads - writes`;
/// * a **Claude** transcript reports only the *fresh* input, with reads and writes counted in their
///   own fields (and typically far larger), so it is already the answer.
///
/// Normalizing to the fresh input is what keeps the four buckets disjoint — the precondition
/// [`cost_usd`] sums under.
pub fn fresh_input(
    harness: &str,
    input_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
) -> i64 {
    if harness.eq_ignore_ascii_case("copilot") {
        (input_tokens - cache_read_tokens.max(0) - cache_write_tokens.max(0)).max(0)
    } else {
        input_tokens.max(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: i64, read: i64, w5m: i64, w1h: i64, output: i64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            cache_read_tokens: read,
            cache_write_5m_tokens: w5m,
            cache_write_1h_tokens: w1h,
            output_tokens: output,
        }
    }

    #[test]
    fn anthropic_cache_multipliers_hold_across_the_family() {
        for id in ["claude-haiku-4-5", "claude-sonnet-5", "claude-opus-5"] {
            let p = model_price(id).expect("priced");
            assert!(
                (p.cache_read_per_1m - p.input_per_1m * 0.1).abs() < 1e-9,
                "{id}"
            );
            assert!(
                (p.cache_write_5m_per_1m - p.input_per_1m * 1.25).abs() < 1e-9,
                "{id}"
            );
            assert!(
                (p.cache_write_1h_per_1m - p.input_per_1m * 2.0).abs() < 1e-9,
                "{id}"
            );
        }
    }

    #[test]
    fn one_hour_cache_writes_cost_more_than_five_minute_ones() {
        // The whole reason the TTL split survives ingest: Claude Code writes at the 1h TTL, and
        // pricing those at the 5m rate understates the write column by 60%.
        let five = cost_usd("claude-opus-5", usage(0, 0, 1_000_000, 0, 0)).expect("priced");
        let hour = cost_usd("claude-opus-5", usage(0, 0, 0, 1_000_000, 0)).expect("priced");
        assert!((five - 6.25).abs() < 1e-9, "5m write: {five}");
        assert!((hour - 10.0).abs() < 1e-9, "1h write: {hour}");
    }

    #[test]
    fn a_cached_claude_turn_prices_every_bucket() {
        // 2 fresh input, 56_575 cache reads, 1_192 tokens written at the 1h TTL, 197 output —
        // a real turn out of a `~/.claude/projects` transcript.
        let got = cost_usd("claude-opus-5", usage(2, 56_575, 0, 1_192, 197)).expect("priced");
        let want =
            2.0 / 1e6 * 5.0 + 56_575.0 / 1e6 * 0.5 + 1_192.0 / 1e6 * 10.0 + 197.0 / 1e6 * 25.0;
        assert!((got - want).abs() < 1e-9, "got {got}, want {want}");
    }

    #[test]
    fn dropping_cache_writes_would_understate_a_claude_session() {
        // The behaviour before cache writes were captured, kept as a regression: writes free.
        let with_writes =
            cost_usd("claude-opus-5", usage(2, 56_575, 0, 35_163, 197)).expect("priced");
        let without = cost_usd("claude-opus-5", usage(2, 56_575, 0, 0, 197)).expect("priced");
        assert!(
            with_writes > without * 1.5,
            "writes should dominate a cached turn: {with_writes} vs {without}"
        );
    }

    #[test]
    fn cheap_tier_needles_win_over_their_base_family() {
        assert_eq!(model_price("gpt-5-mini"), model_price("o4-mini"));
        assert_ne!(model_price("gpt-5-mini"), model_price("gpt-5.3-codex"));
        // "gemini" contains "mini" — the Gemini row must match first.
        assert_eq!(model_price("gemini-3.1-pro"), model_price("gemini-2.5-pro"));
        assert_ne!(model_price("gemini-3.1-pro"), model_price("gpt-5-mini"));
    }

    #[test]
    fn the_one_million_context_variant_prices_as_its_base_model() {
        // No long-context premium since Opus 4.7 — `[1m]` needs no row, and must not fall off
        // the table into an unpriced null.
        assert_eq!(
            model_price("claude-opus-5[1m]"),
            model_price("claude-opus-5")
        );
    }

    #[test]
    fn an_unknown_model_is_unpriced_rather_than_free() {
        assert_eq!(model_price("some-future-model"), None);
        assert_eq!(model_price("  "), None);
        assert_eq!(cost_usd("some-future-model", usage(1, 1, 1, 1, 1)), None);
    }

    #[test]
    fn openai_and_google_bill_a_cache_write_as_plain_input() {
        for id in ["gpt-5.3-codex", "gemini-3.1-pro", "gpt-5-mini"] {
            let p = model_price(id).expect("priced");
            assert!(
                (p.cache_write_5m_per_1m - p.input_per_1m).abs() < 1e-9,
                "{id}"
            );
            assert!(
                (p.cache_write_1h_per_1m - p.input_per_1m).abs() < 1e-9,
                "{id}"
            );
        }
    }

    #[test]
    fn copilot_input_is_a_total_and_claude_input_is_already_fresh() {
        // Copilot: 156_985 total prompt, of which 111_104 read and 20_000 written.
        assert_eq!(fresh_input("copilot", 156_985, 111_104, 20_000), 25_881);
        // Claude: the fields are disjoint, so the fresh figure passes through untouched.
        assert_eq!(fresh_input("claude", 2, 56_575, 1_192), 2);
    }

    #[test]
    fn a_copilot_prompt_that_is_entirely_cached_has_no_fresh_input() {
        // Clamped rather than negative: a harness that reports reads exceeding the total must not
        // turn into a negative charge that silently discounts the rest of the fold.
        assert_eq!(fresh_input("copilot", 100, 90, 90), 0);
    }

    #[test]
    fn prompt_tokens_counts_every_bucket_and_input_tokens_alone_does_not() {
        let u = usage(2, 56_575, 0, 1_192, 197);
        assert_eq!(u.cache_write_tokens(), 1_192);
        assert_eq!(u.prompt_tokens(), 57_769);
    }
}
