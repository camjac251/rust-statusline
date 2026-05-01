//! # Pricing Module
//!
//! Provides model-specific pricing information for cost calculations.
//!
//! ## Pricing Structure
//!
//! Each model has pricing for:
//! - Input tokens
//! - Output tokens
//! - Cache creation (typically 1.25x input price, with 5m/1h tiers)
//! - Cache reads (typically 0.1x input price)
//!
//! ## Pricing Resolution Order
//!
//! Prices are resolved in the following priority order:
//! 1. Environment variable overrides (if all four are set):
//!    - `CLAUDE_PRICE_INPUT`
//!    - `CLAUDE_PRICE_OUTPUT`
//!    - `CLAUDE_PRICE_CACHE_CREATE`
//!    - `CLAUDE_PRICE_CACHE_READ`
//! 2. Compile-time embedded pricing.json (from `pricing.json` at build time)
//! 3. Built-in static pricing fallback
//! 4. Family heuristics (opus/sonnet/haiku detection)

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::env;

use crate::provenance::PricingSource;

#[derive(Clone, Copy, Debug)]
pub struct Pricing {
    pub in_per_tok: f64,
    pub out_per_tok: f64,
    pub cache_create_per_tok: f64,
    pub cache_create_1h_per_tok: f64,
    pub cache_read_per_tok: f64,
}

impl Pricing {
    fn new(
        in_per_tok: f64,
        out_per_tok: f64,
        cache_create_per_tok: f64,
        cache_create_1h_per_tok: f64,
        cache_read_per_tok: f64,
    ) -> Self {
        Self {
            in_per_tok,
            out_per_tok,
            cache_create_per_tok,
            cache_create_1h_per_tok,
            cache_read_per_tok,
        }
    }

    fn from_model_pricing(model_pricing: &ModelPricing) -> Self {
        Self::new(
            model_pricing.input,
            model_pricing.output,
            model_pricing.cache_create,
            model_pricing
                .cache_create_1h
                .unwrap_or(model_pricing.cache_create),
            model_pricing.cache_read,
        )
    }

    fn from_input_multipliers(in_per_tok: f64, out_per_tok: f64) -> Self {
        Self::new(
            in_per_tok,
            out_per_tok,
            in_per_tok * 1.25,
            in_per_tok * 2.0,
            in_per_tok * 0.1,
        )
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct ModelPricing {
    name: String,
    input: f64,
    output: f64,
    cache_create: f64,
    #[serde(default)]
    cache_create_1h: Option<f64>,
    cache_read: f64,
    #[serde(default)]
    fast_mode_multiplier: Option<f64>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct PricingMultipliers {
    input: f64,
    output: f64,
    cache_create: f64,
    #[serde(default)]
    cache_create_1h: Option<f64>,
    cache_read: f64,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct PricingTier {
    name: String,
    #[serde(default)]
    description: Option<String>,
    threshold: u64,
    applies_to: Vec<String>,
    multipliers: PricingMultipliers,
}

#[derive(Deserialize, Serialize, Debug, Default)]
struct TieredPricing {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    tiers: Vec<PricingTier>,
}

#[derive(Deserialize, Serialize, Debug)]
struct PricingConfig {
    models: HashMap<String, ModelPricing>,
    #[serde(default)]
    additional_costs: AdditionalCosts,
    #[serde(default)]
    tiered_pricing: TieredPricing,
}

#[derive(Deserialize, Serialize, Debug, Default)]
struct AdditionalCosts {
    #[serde(default)]
    web_search_per_request: f64,
}

/// Compile-time embedded pricing configuration
static PRICING_CONFIG: Lazy<Option<PricingConfig>> = Lazy::new(|| {
    const EMBEDDED_PRICING: &str = include_str!("../pricing.json");
    serde_json::from_str::<PricingConfig>(EMBEDDED_PRICING).ok()
});

/// Get pricing from embedded config
fn pricing_from_config(model_id: &str) -> Option<Pricing> {
    let config = PRICING_CONFIG.as_ref()?;
    let m = model_id.to_lowercase();

    // Try exact match first
    if let Some(model_pricing) = config.models.get(&m) {
        return Some(Pricing::from_model_pricing(model_pricing));
    }

    // Try canonical model names before partial matching. Order matters: more
    // specific 4.x variants must win before their family prefix.
    if let Some(key) = canonical_pricing_key(&m) {
        if let Some(model_pricing) = config.models.get(key) {
            return Some(Pricing::from_model_pricing(model_pricing));
        }
    }

    // Try provider/date suffix matches and prefer the most specific key.
    if let Some((_, model_pricing)) = config
        .models
        .iter()
        .filter(|(key, _)| m.contains(key.as_str()))
        .max_by_key(|(key, _)| key.len())
    {
        return Some(Pricing::from_model_pricing(model_pricing));
    }

    // Try short user-provided fragments such as "opus-4-6".
    for (key, model_pricing) in &config.models {
        if key.contains(&m) {
            return Some(Pricing::from_model_pricing(model_pricing));
        }
    }

    None
}

pub(crate) fn static_pricing_lookup(model_id: &str) -> Option<Pricing> {
    // Prefer exact/known variants before family heuristics
    let m = model_id.to_lowercase();
    // Opus 4.5/4.6 (and catch generic "claude-4-5" as flagship/Opus)
    if m.contains("opus-4-5") || m.contains("opus-4-6") || m == "claude-4-5" {
        let in_pt = 5e-6; // $5 / 1M
        return Some(Pricing::from_input_multipliers(in_pt, 25e-6));
    }
    // Opus 4.1
    if m.contains("opus-4-1") {
        let in_pt = 15e-6; // $15 / 1M
        return Some(Pricing::from_input_multipliers(in_pt, 75e-6));
    }
    // Opus 4 (avoid matching 4.1 above)
    if m.contains("opus-4") {
        let in_pt = 15e-6;
        return Some(Pricing::from_input_multipliers(in_pt, 75e-6));
    }
    // Sonnet 4 (also catch "claude-4-sonnet")
    if m.contains("sonnet-4") || m.contains("4-sonnet") {
        let in_pt = 3e-6; // $3 / 1M
        return Some(Pricing::from_input_multipliers(in_pt, 15e-6));
    }
    // Claude 3.7 Sonnet
    if m.contains("3-7-sonnet") {
        let in_pt = 3e-6; // treat like sonnet family
        return Some(Pricing::from_input_multipliers(in_pt, 15e-6));
    }
    // Claude 3.5 Sonnet
    if m.contains("3-5-sonnet") {
        let in_pt = 3e-6;
        return Some(Pricing::from_input_multipliers(in_pt, 15e-6));
    }
    if m.contains("3-5-haiku") {
        return Some(Pricing::new(0.8e-6, 4.0e-6, 1.0e-6, 1.6e-6, 0.08e-6));
    }
    None
}

fn env_pricing_override() -> Option<Pricing> {
    if let (Ok(gi), Ok(go), Ok(gc), Ok(gr)) = (
        env::var("CLAUDE_PRICE_INPUT").map(|s| s.parse::<f64>()),
        env::var("CLAUDE_PRICE_OUTPUT").map(|s| s.parse::<f64>()),
        env::var("CLAUDE_PRICE_CACHE_CREATE").map(|s| s.parse::<f64>()),
        env::var("CLAUDE_PRICE_CACHE_READ").map(|s| s.parse::<f64>()),
    ) {
        if let (Ok(ii), Ok(oo), Ok(cc), Ok(cr)) = (gi, go, gc, gr) {
            return Some(Pricing::new(ii, oo, cc, cc, cr));
        }
    }
    None
}

pub fn pricing_for_model(model_id: &str) -> Option<Pricing> {
    let m = model_id.to_lowercase();

    // Priority 1: Environment variable overrides (when all four are provided)
    if let Some(p) = env_pricing_override() {
        return Some(p);
    }

    // Priority 2: Embedded pricing.json config
    if let Some(p) = pricing_from_config(&m) {
        return Some(p);
    }

    // Priority 3: Built-in static pricing
    if let Some(p) = static_pricing_lookup(&m) {
        return Some(p);
    }

    // Priority 4: Family heuristics fallback
    if m.contains("opus") {
        let in_pt = 5e-6; // $5 / 1M (current Opus 4.5/4.6 pricing)
        Some(Pricing::from_input_multipliers(in_pt, 25e-6))
    } else if m.contains("sonnet") {
        let in_pt = 3e-6; // $3 / 1M
        Some(Pricing::from_input_multipliers(in_pt, 15e-6))
    } else if m.contains("haiku") {
        let in_pt = 0.25e-6; // $0.25 / 1M
        Some(Pricing::from_input_multipliers(in_pt, 1.25e-6))
    } else {
        None
    }
}

pub fn pricing_source_for_model(model_id: &str) -> PricingSource {
    let m = model_id.to_lowercase();

    if env_pricing_override().is_some() {
        return PricingSource::EnvOverride;
    }
    if pricing_from_config(&m).is_some() {
        return PricingSource::Embedded;
    }
    if static_pricing_lookup(&m).is_some() {
        return PricingSource::StaticFallback;
    }
    if m.contains("opus") || m.contains("sonnet") || m.contains("haiku") {
        return PricingSource::FamilyHeuristic;
    }
    PricingSource::Unavailable
}

/// Get the fast mode multiplier for a model (e.g. 6x for Opus 4.6).
/// Returns 1.0 if the model has no fast mode pricing.
pub fn fast_mode_multiplier(model_id: &str) -> f64 {
    let config = match PRICING_CONFIG.as_ref() {
        Some(c) => c,
        None => return 1.0,
    };
    let m = model_id.to_lowercase();
    // Exact match: return its multiplier (or 1.0 if model exists but has no fast mode)
    if let Some(mp) = config.models.get(&m) {
        return mp.fast_mode_multiplier.unwrap_or(1.0);
    }
    if let Some(key) = canonical_pricing_key(&m) {
        if let Some(mp) = config.models.get(key) {
            return mp.fast_mode_multiplier.unwrap_or(1.0);
        }
    }
    // Partial match: only when no exact match was found
    if let Some((_, mp)) = config
        .models
        .iter()
        .filter(|(key, mp)| mp.fast_mode_multiplier.is_some() && m.contains(key.as_str()))
        .max_by_key(|(key, _)| key.len())
    {
        return mp.fast_mode_multiplier.unwrap_or(1.0);
    }
    1.0
}

fn canonical_pricing_key(model_id: &str) -> Option<&'static str> {
    let ordered = [
        ("opus-4-6", "claude-opus-4-6"),
        ("opus-4-5", "claude-opus-4-5"),
        ("opus-4-1", "claude-opus-4-1"),
        ("opus-4", "claude-opus-4"),
        ("sonnet-4-6", "claude-sonnet-4-6"),
        ("sonnet-4-5", "claude-sonnet-4-5"),
        ("sonnet-4", "claude-sonnet-4"),
        ("haiku-4-5", "claude-haiku-4-5"),
        ("3-7-sonnet", "claude-3-7-sonnet"),
        ("3-5-sonnet", "claude-3-5-sonnet"),
        ("3-5-haiku", "claude-3-5-haiku"),
        ("3-opus", "claude-3-opus"),
        ("3-haiku", "claude-3-haiku"),
    ];

    ordered
        .iter()
        .find_map(|(needle, key)| model_id.contains(needle).then_some(*key))
}

fn usage_u64(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(|n| n.as_u64()).unwrap_or(0)
}

fn usage_nested_u64(usage: &Value, parent: &str, key: &str) -> u64 {
    usage
        .get(parent)
        .and_then(|value| value.get(key))
        .and_then(|n| n.as_u64())
        .unwrap_or(0)
}

fn usage_speed(usage: &Value) -> Option<&str> {
    usage.get("speed").and_then(|s| s.as_str())
}

fn web_search_per_request() -> f64 {
    PRICING_CONFIG
        .as_ref()
        .map(|c| c.additional_costs.web_search_per_request)
        .filter(|v| *v > 0.0)
        .unwrap_or(0.01)
}

fn flat_cost_for_usage(model_id: &str, usage: &Value) -> f64 {
    let Some(base_p) = pricing_for_model(model_id) else {
        return 0.0;
    };

    let input = usage_u64(usage, "input_tokens");
    let output = usage_u64(usage, "output_tokens");
    let cache_create = usage_u64(usage, "cache_creation_input_tokens");
    let cache_create_1h = usage_nested_u64(usage, "cache_creation", "ephemeral_1h_input_tokens");
    let cache_create_5m = usage_nested_u64(usage, "cache_creation", "ephemeral_5m_input_tokens");
    let cache_create_nested = cache_create_1h + cache_create_5m;
    let cache_create_effective = cache_create.max(cache_create_nested);
    let cache_create_unknown = cache_create_effective.saturating_sub(cache_create_nested);
    let cache_read = usage_u64(usage, "cache_read_input_tokens");
    let web_search_requests = usage
        .get("server_tool_use")
        .and_then(|o| o.get("web_search_requests"))
        .and_then(|n| n.as_u64())
        .unwrap_or(0);

    let p = apply_tiered_pricing(
        base_p,
        model_id,
        input + cache_create_effective + cache_read,
    );
    let token_cost = (input as f64) * p.in_per_tok
        + (output as f64) * p.out_per_tok
        + ((cache_create_5m + cache_create_unknown) as f64) * p.cache_create_per_tok
        + (cache_create_1h as f64) * p.cache_create_1h_per_tok
        + (cache_read as f64) * p.cache_read_per_tok;
    let web_search_cost = (web_search_requests as f64) * web_search_per_request();

    let token_multiplier = if usage_speed(usage) == Some("fast") {
        fast_mode_multiplier(model_id)
    } else {
        1.0
    };

    token_cost * token_multiplier + web_search_cost
}

/// Calculate Claude Code-compatible cost for a usage object.
///
/// Mirrors `calculateUSDCost` plus `addToTotalSessionCost` in Claude Code:
/// token/cache costs are model-priced, Opus 4.6 fast mode affects token/cache
/// tiers only, web search remains a flat per-request charge, and advisor
/// iteration usage is charged recursively under its own model.
pub fn calculate_cost_for_usage(model_id: &str, usage: &Value) -> f64 {
    let advisor_cost = usage
        .get("iterations")
        .and_then(|v| v.as_array())
        .map(|iterations| {
            iterations
                .iter()
                .filter(|it| it.get("type").and_then(|s| s.as_str()) == Some("advisor_message"))
                .filter_map(|it| {
                    let model = it.get("model").and_then(|s| s.as_str())?;
                    Some(calculate_cost_for_usage(model, it))
                })
                .sum::<f64>()
        })
        .unwrap_or(0.0);

    flat_cost_for_usage(model_id, usage) + advisor_cost
}

/// Apply tiered pricing multipliers if applicable based on token count
/// Returns modified pricing if a tier applies, otherwise returns the input pricing unchanged
pub fn apply_tiered_pricing(
    base_pricing: Pricing,
    model_id: &str,
    total_input_tokens: u64,
) -> Pricing {
    // Check if tiered pricing is enabled and configured
    let config = match PRICING_CONFIG.as_ref() {
        Some(c) if c.tiered_pricing.enabled => c,
        _ => return base_pricing,
    };

    let model_lower = model_id.to_lowercase();

    // Find applicable tier
    for tier in &config.tiered_pricing.tiers {
        // Check if this tier applies using exact or date-suffix matching:
        // "claude-sonnet-4" matches "claude-sonnet-4" and "claude-sonnet-4-20250514"
        // but NOT "claude-sonnet-4-5" or "claude-sonnet-4-6" (different model versions)
        let applies = tier.applies_to.iter().any(|pattern| {
            let p = pattern.to_lowercase();
            if model_lower == p {
                return true;
            }
            // Only match date suffixes like -20250514 (8+ digits), not version suffixes like -5, -6
            if let Some(suffix) = model_lower.strip_prefix(&p) {
                if let Some(rest) = suffix.strip_prefix('-') {
                    return rest.len() >= 8 && rest.chars().all(|c| c.is_ascii_digit());
                }
            }
            false
        });

        if applies && total_input_tokens > tier.threshold {
            // Apply multipliers
            return Pricing {
                in_per_tok: base_pricing.in_per_tok * tier.multipliers.input,
                out_per_tok: base_pricing.out_per_tok * tier.multipliers.output,
                cache_create_per_tok: base_pricing.cache_create_per_tok
                    * tier.multipliers.cache_create,
                cache_create_1h_per_tok: base_pricing.cache_create_1h_per_tok
                    * tier
                        .multipliers
                        .cache_create_1h
                        .unwrap_or(tier.multipliers.cache_create),
                cache_read_per_tok: base_pricing.cache_read_per_tok * tier.multipliers.cache_read,
            };
        }
    }

    // No tier applies, return base pricing
    base_pricing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pricing_for_known_models() {
        // Test Sonnet pricing
        let sonnet_pricing = pricing_for_model("claude-3.5-sonnet").unwrap();
        assert!((sonnet_pricing.in_per_tok - 3e-6).abs() < 1e-10);
        assert!((sonnet_pricing.out_per_tok - 15e-6).abs() < 1e-10);
        assert!((sonnet_pricing.cache_create_per_tok - 3.75e-6).abs() < 1e-10);
        assert!((sonnet_pricing.cache_read_per_tok - 0.3e-6).abs() < 1e-10);

        // Test Opus pricing
        let opus_pricing = pricing_for_model("claude-opus-4").unwrap();
        assert!((opus_pricing.in_per_tok - 15e-6).abs() < 1e-10);
        assert!((opus_pricing.out_per_tok - 75e-6).abs() < 1e-10);

        // Test Haiku pricing
        let haiku_pricing = pricing_for_model("claude-3.5-haiku").unwrap();
        assert!((haiku_pricing.in_per_tok - 0.25e-6).abs() < 1e-10);
        assert!((haiku_pricing.out_per_tok - 1.25e-6).abs() < 1e-10);
    }

    #[test]
    fn test_pricing_family_fallback() {
        // Test opus family fallback (current gen: $5/$25)
        let opus_fallback = pricing_for_model("some-future-opus-model").unwrap();
        assert_eq!(opus_fallback.in_per_tok, 5e-6);
        assert_eq!(opus_fallback.out_per_tok, 25e-6);

        // Test sonnet family fallback
        let sonnet_fallback = pricing_for_model("some-future-sonnet-model").unwrap();
        assert_eq!(sonnet_fallback.in_per_tok, 3e-6);
        assert_eq!(sonnet_fallback.out_per_tok, 15e-6);
    }

    #[test]
    fn test_sonnet_46_pricing() {
        let p = pricing_for_model("claude-sonnet-4-6").unwrap();
        assert!((p.in_per_tok - 3e-6).abs() < 1e-10);
        assert!((p.out_per_tok - 15e-6).abs() < 1e-10);
    }

    #[test]
    fn test_opus_46_pricing() {
        let p = pricing_for_model("claude-opus-4-6").unwrap();
        assert!((p.in_per_tok - 5e-6).abs() < 1e-10);
        assert!((p.out_per_tok - 25e-6).abs() < 1e-10);
    }

    #[test]
    fn test_fast_mode_multiplier() {
        // Opus 4.6 has 6x fast mode
        assert!((fast_mode_multiplier("claude-opus-4-6") - 6.0).abs() < 1e-10);
        assert!((fast_mode_multiplier("us.anthropic.claude-opus-4-6-v1") - 6.0).abs() < 1e-10);
        // Other models have no fast mode (1x)
        assert!((fast_mode_multiplier("claude-sonnet-4-6") - 1.0).abs() < 1e-10);
        assert!((fast_mode_multiplier("claude-sonnet-4-5") - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_provider_model_uses_specific_pricing() {
        let p = pricing_for_model("us.anthropic.claude-opus-4-6-v1").unwrap();
        assert!((p.in_per_tok - 5e-6).abs() < 1e-10);
        assert!((p.out_per_tok - 25e-6).abs() < 1e-10);
    }

    #[test]
    fn test_fast_mode_does_not_multiply_web_search_cost() {
        let usage = serde_json::json!({
            "input_tokens": 1_000_000,
            "output_tokens": 1_000_000,
            "cache_creation_input_tokens": 1_000_000,
            "cache_read_input_tokens": 1_000_000,
            "server_tool_use": { "web_search_requests": 2 },
            "speed": "fast"
        });

        let cost = calculate_cost_for_usage("claude-opus-4-6", &usage);
        assert!((cost - 220.52).abs() < 1e-10);
    }

    #[test]
    fn test_cache_creation_1h_uses_1h_price_when_reported() {
        let usage = serde_json::json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_creation_input_tokens": 2_000_000,
            "cache_read_input_tokens": 0,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 1_000_000,
                "ephemeral_1h_input_tokens": 1_000_000
            }
        });

        let cost = calculate_cost_for_usage("claude-sonnet-4-6", &usage);
        assert!((cost - 9.75).abs() < 1e-10);
    }

    #[test]
    fn test_advisor_iterations_are_charged() {
        let usage = serde_json::json!({
            "input_tokens": 1_000_000,
            "output_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
            "iterations": [
                {
                    "type": "advisor_message",
                    "model": "claude-opus-4-6",
                    "input_tokens": 1_000_000,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "speed": "fast"
                }
            ]
        });

        let cost = calculate_cost_for_usage("claude-sonnet-4-6", &usage);
        assert!((cost - 33.0).abs() < 1e-10);
    }

    #[test]
    fn test_opus_46_no_long_context_tier() {
        // After Mar 13 GA, Opus 4.6 has standard pricing across full 1M window
        let base = pricing_for_model("claude-opus-4-6").unwrap();
        let tiered = apply_tiered_pricing(base, "claude-opus-4-6", 300_000);
        assert!((tiered.in_per_tok - base.in_per_tok).abs() < 1e-15);
        assert!((tiered.out_per_tok - base.out_per_tok).abs() < 1e-15);
    }

    #[test]
    fn test_sonnet_46_no_long_context_tier() {
        // After Mar 13 GA, Sonnet 4.6 has standard pricing across full 1M window
        let base = pricing_for_model("claude-sonnet-4-6").unwrap();
        let tiered = apply_tiered_pricing(base, "claude-sonnet-4-6", 300_000);
        assert!((tiered.in_per_tok - base.in_per_tok).abs() < 1e-15);
        assert!((tiered.out_per_tok - base.out_per_tok).abs() < 1e-15);
    }

    #[test]
    fn test_fast_mode_no_false_positive_on_opus_4() {
        // Opus 4 (not 4.6) should NOT get fast mode multiplier
        assert!((fast_mode_multiplier("claude-opus-4") - 1.0).abs() < 1e-10);
        assert!((fast_mode_multiplier("claude-opus-4-20250514") - 1.0).abs() < 1e-10);
        assert!((fast_mode_multiplier("claude-opus-4-1") - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_unknown_model() {
        assert!(pricing_for_model("unknown-model").is_none());
    }
}
