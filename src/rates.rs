use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const LITELLM_PRICES_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRates {
    pub input_per_token: f64,
    pub output_per_token: f64,
    pub cache_creation_per_token: f64,
    pub cache_read_per_token: f64,
}

pub type RateCard = HashMap<String, ModelRates>;

#[derive(Serialize, Deserialize)]
struct CacheFile {
    fetched_at: String,
    model_count: usize,
    rates: RateCard,
}

pub fn cache_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".trakr").join("rates.json"))
}

/// Fetch Claude model rates from the LiteLLM price list.
pub fn fetch_rates() -> Result<RateCard> {
    let body: serde_json::Value = ureq::get(LITELLM_PRICES_URL)
        .call()
        .map_err(|e| anyhow::anyhow!("fetching LiteLLM prices: {}", e))?
        .into_json()
        .map_err(|e| anyhow::anyhow!("parsing LiteLLM prices: {}", e))?;

    let mut card: RateCard = HashMap::new();

    if let Some(obj) = body.as_object() {
        for (name, data) in obj {
            if !name.contains("claude") {
                continue;
            }
            let inp = data.get("input_cost_per_token").and_then(|v| v.as_f64());
            let out = data.get("output_cost_per_token").and_then(|v| v.as_f64());
            let (Some(inp), Some(out)) = (inp, out) else { continue };

            let cache_create = data
                .get("cache_creation_input_token_cost")
                .and_then(|v| v.as_f64())
                .unwrap_or(inp * 1.25);
            let cache_read = data
                .get("cache_read_input_token_cost")
                .and_then(|v| v.as_f64())
                .unwrap_or(inp * 0.1);

            card.insert(
                name.clone(),
                ModelRates {
                    input_per_token: inp,
                    output_per_token: out,
                    cache_creation_per_token: cache_create,
                    cache_read_per_token: cache_read,
                },
            );
        }
    }

    Ok(card)
}

pub fn save_rate_card(card: &RateCard) -> Result<()> {
    let path = cache_path()?;
    let file = CacheFile {
        fetched_at: chrono::Utc::now().to_rfc3339(),
        model_count: card.len(),
        rates: card.clone(),
    };
    std::fs::write(&path, serde_json::to_string_pretty(&file)?)?;
    Ok(())
}

/// Fetch and persist rates. Returns the number of Claude models found.
pub fn refresh_rates() -> Result<usize> {
    let card = fetch_rates()?;
    let n = card.len();
    save_rate_card(&card)?;
    Ok(n)
}

/// Load the cached rate card from disk. Falls back to hardcoded if not available.
pub fn load_rate_card() -> RateCard {
    if let Some(card) = try_load_cached() {
        if !card.is_empty() {
            return card;
        }
    }
    hardcoded_fallback()
}

fn try_load_cached() -> Option<RateCard> {
    let path = cache_path().ok()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    let file: CacheFile = serde_json::from_str(&contents).ok()?;
    Some(file.rates)
}

/// Return the timestamp of the last successful rates fetch, if any.
pub fn last_fetched_at() -> Option<chrono::DateTime<chrono::Utc>> {
    let path = cache_path().ok()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    let file: CacheFile = serde_json::from_str(&contents).ok()?;
    chrono::DateTime::parse_from_rfc3339(&file.fetched_at)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Hardcoded fallback rates (Anthropic published pricing, June 2026).
pub fn hardcoded_fallback() -> RateCard {
    let entries = [
        ("fable",  10e-6, 50e-6,  12.5e-6, 1.0e-6),
        ("opus",    5e-6, 25e-6,   6.25e-6, 0.5e-6),
        ("sonnet",  3e-6, 15e-6,   3.75e-6, 0.3e-6),
        ("haiku",   1e-6,  5e-6,   1.25e-6, 0.1e-6),
    ];
    entries
        .iter()
        .map(|&(name, inp, out, cc, cr)| {
            (
                name.to_string(),
                ModelRates {
                    input_per_token: inp,
                    output_per_token: out,
                    cache_creation_per_token: cc,
                    cache_read_per_token: cr,
                },
            )
        })
        .collect()
}

/// Resolve the best-matching rates for a model from a rate card.
///
/// Search order: exact match → substring (longest key wins) → family keyword → Sonnet fallback.
pub fn resolve(card: &RateCard, model: &str) -> ModelRates {
    let m = model.to_lowercase();

    if let Some(r) = card.get(&m) {
        return r.clone();
    }

    // Longest substring match: prefer e.g. "claude-sonnet-4-6" over "claude-sonnet".
    let best = card
        .iter()
        .filter(|(k, _)| m.contains(k.as_str()))
        .max_by_key(|(k, _)| k.len());
    if let Some((_, r)) = best {
        return r.clone();
    }

    // Keyword fallback against hardcoded family names.
    let fallback = hardcoded_fallback();
    for keyword in &["fable", "opus", "haiku", "sonnet"] {
        if m.contains(keyword) {
            return fallback[*keyword].clone();
        }
    }
    fallback["sonnet"].clone()
}
