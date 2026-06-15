use crate::rates::{self, RateCard};

/// Compute cost in USD from token counts using a pre-loaded rate card.
///
/// `cache_creation_1h_tokens` is the 1-hour TTL subset of `cache_creation_tokens` and is billed
/// at 2× the input rate. The remainder (5-min TTL) is billed at the standard 1.25× rate.
/// Pass 0 for old DB records where the split is unknown — all cache creation falls back to 1.25×.
pub fn compute_cost_usd_with_card(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_1h_tokens: u64,
    card: &RateCard,
) -> f64 {
    let r = rates::resolve(card, model);
    let cache_creation_5m = cache_creation_tokens.saturating_sub(cache_creation_1h_tokens);
    (input_tokens as f64) * r.input_per_token
        + (output_tokens as f64) * r.output_per_token
        + (cache_creation_1h_tokens as f64) * r.input_per_token * 2.0
        + (cache_creation_5m as f64) * r.cache_creation_per_token
        + (cache_read_tokens as f64) * r.cache_read_per_token
}

/// Compute cost in USD from token counts, loading the rate card from disk (or fallback).
///
/// Prefer `compute_cost_usd_with_card` in tight loops — this loads the rate card on every call.
pub fn compute_cost_usd(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_1h_tokens: u64,
) -> f64 {
    let card = rates::load_rate_card();
    compute_cost_usd_with_card(
        model,
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
        cache_creation_1h_tokens,
        &card,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fallback_card() -> RateCard {
        rates::hardcoded_fallback()
    }

    #[test]
    fn sonnet_basic_cost() {
        // 1M input @ $3/MTok + 100k output @ $15/MTok = $3.00 + $1.50 = $4.50
        let cost = compute_cost_usd_with_card("claude-sonnet-4-6", 1_000_000, 100_000, 0, 0, 0, &fallback_card());
        assert!((cost - 4.50).abs() < 1e-9, "expected $4.50, got ${}", cost);
    }

    #[test]
    fn haiku_with_cache_read() {
        // 1M cache_read @ $0.10/MTok = $0.10
        let cost = compute_cost_usd_with_card("claude-haiku-4-5", 0, 0, 0, 1_000_000, 0, &fallback_card());
        assert!((cost - 0.10).abs() < 1e-9, "expected $0.10, got ${}", cost);
    }

    #[test]
    fn opus_output_only() {
        // 1M output @ $25/MTok = $25
        let cost = compute_cost_usd_with_card("claude-opus-4-8", 0, 1_000_000, 0, 0, 0, &fallback_card());
        assert!((cost - 25.0).abs() < 1e-9, "expected $25.00, got ${}", cost);
    }

    #[test]
    fn fable_rates() {
        // 500k input @ $10/MTok + 500k output @ $50/MTok = $5 + $25 = $30
        let cost = compute_cost_usd_with_card("claude-fable-5", 500_000, 500_000, 0, 0, 0, &fallback_card());
        assert!((cost - 30.0).abs() < 1e-9, "expected $30.00, got ${}", cost);
    }

    #[test]
    fn cache_creation_5m_at_1_25x_input_rate() {
        // Sonnet 5-min tier: 1M cache_create (all 5m) @ $3.75/MTok + 1M cache_read @ $0.30/MTok = $4.05
        let cost = compute_cost_usd_with_card("claude-sonnet-4-6", 0, 0, 1_000_000, 1_000_000, 0, &fallback_card());
        assert!((cost - 4.05).abs() < 1e-9, "expected $4.05, got ${}", cost);
    }

    #[test]
    fn cache_creation_1h_at_2x_input_rate() {
        // Sonnet 1h tier: 1M cache_create (all 1h) @ 2×$3/MTok = $6.00
        let cost = compute_cost_usd_with_card("claude-sonnet-4-6", 0, 0, 1_000_000, 0, 1_000_000, &fallback_card());
        assert!((cost - 6.00).abs() < 1e-9, "expected $6.00, got ${}", cost);
    }

    #[test]
    fn cache_creation_mixed_tiers() {
        // Sonnet: 800k 1h @ $6/MTok = $4.80, 200k 5m @ $3.75/MTok = $0.75, total = $5.55
        let cost = compute_cost_usd_with_card("claude-sonnet-4-6", 0, 0, 1_000_000, 0, 800_000, &fallback_card());
        assert!((cost - 5.55).abs() < 1e-9, "expected $5.55, got ${}", cost);
    }

    #[test]
    fn unknown_model_falls_back_to_sonnet() {
        let cost_unknown = compute_cost_usd_with_card("claude-unknown-99", 1_000_000, 0, 0, 0, 0, &fallback_card());
        let cost_sonnet = compute_cost_usd_with_card("claude-sonnet-4-6", 1_000_000, 0, 0, 0, 0, &fallback_card());
        assert!((cost_unknown - cost_sonnet).abs() < 1e-9);
    }
}
