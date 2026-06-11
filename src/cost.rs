/// Per-model token rates from the published Anthropic rate card (June 2026).
struct ModelRates {
    input_per_mtok: f64,
    output_per_mtok: f64,
}

fn rates_for_model(model: &str) -> ModelRates {
    let m = model.to_lowercase();
    if m.contains("fable") {
        ModelRates { input_per_mtok: 10.0, output_per_mtok: 50.0 }
    } else if m.contains("opus") {
        ModelRates { input_per_mtok: 5.0, output_per_mtok: 25.0 }
    } else if m.contains("sonnet") {
        ModelRates { input_per_mtok: 3.0, output_per_mtok: 15.0 }
    } else if m.contains("haiku") {
        ModelRates { input_per_mtok: 1.0, output_per_mtok: 5.0 }
    } else {
        // Fall back to Sonnet rates for unknown models.
        ModelRates { input_per_mtok: 3.0, output_per_mtok: 15.0 }
    }
}

/// Estimate cost in USD from token counts.
///
/// Cache read is billed at 10% of the input rate.
/// Cache creation is billed at the full input rate.
pub fn compute_cost_usd(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    let rates = rates_for_model(model);
    let cache_read_rate = rates.input_per_mtok * 0.1;

    (input_tokens as f64 / 1_000_000.0) * rates.input_per_mtok
        + (output_tokens as f64 / 1_000_000.0) * rates.output_per_mtok
        + (cache_creation_tokens as f64 / 1_000_000.0) * rates.input_per_mtok
        + (cache_read_tokens as f64 / 1_000_000.0) * cache_read_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sonnet_basic_cost() {
        // 1M input + 100k output = $3.00 + $1.50 = $4.50
        let cost = compute_cost_usd("claude-sonnet-4-6", 1_000_000, 100_000, 0, 0);
        assert!((cost - 4.50).abs() < 1e-9, "expected $4.50, got ${}", cost);
    }

    #[test]
    fn haiku_with_cache() {
        // 1M cache_read @ $0.10/MTok = $0.10
        let cost = compute_cost_usd("claude-haiku-4-5", 0, 0, 0, 1_000_000);
        assert!((cost - 0.10).abs() < 1e-9, "expected $0.10, got ${}", cost);
    }

    #[test]
    fn opus_output_only() {
        // 1M output @ $25/MTok = $25
        let cost = compute_cost_usd("claude-opus-4-8", 0, 1_000_000, 0, 0);
        assert!((cost - 25.0).abs() < 1e-9, "expected $25.00, got ${}", cost);
    }

    #[test]
    fn fable_rates() {
        // 500k input @ $10/MTok = $5, 500k output @ $50/MTok = $25 → $30
        let cost = compute_cost_usd("claude-fable-5", 500_000, 500_000, 0, 0);
        assert!((cost - 30.0).abs() < 1e-9, "expected $30.00, got ${}", cost);
    }

    #[test]
    fn cache_creation_at_input_rate() {
        // 1M cache_create + 1M cache_read @ Sonnet: $3 + $0.30 = $3.30
        let cost = compute_cost_usd("claude-sonnet-4-6", 0, 0, 1_000_000, 1_000_000);
        assert!((cost - 3.30).abs() < 1e-9, "expected $3.30, got ${}", cost);
    }

    #[test]
    fn unknown_model_falls_back_to_sonnet() {
        let cost_unknown = compute_cost_usd("claude-unknown-99", 1_000_000, 0, 0, 0);
        let cost_sonnet = compute_cost_usd("claude-sonnet-4-6", 1_000_000, 0, 0, 0);
        assert!((cost_unknown - cost_sonnet).abs() < 1e-9);
    }
}
