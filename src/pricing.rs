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
use std::collections::HashMap;
use std::env;

#[derive(Clone, Copy, Debug)]
pub struct Pricing {
    pub in_per_tok: f64,
    pub out_per_tok: f64,
    pub cache_create_per_tok: f64,
    pub cache_read_per_tok: f64,
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
        return Some(Pricing {
            in_per_tok: model_pricing.input,
            out_per_tok: model_pricing.output,
            cache_create_per_tok: model_pricing.cache_create,
            cache_read_per_tok: model_pricing.cache_read,
        });
    }

    // Try partial matches
    for (key, model_pricing) in &config.models {
        if m.contains(key) || key.contains(&m) {
            return Some(Pricing {
                in_per_tok: model_pricing.input,
                out_per_tok: model_pricing.output,
                cache_create_per_tok: model_pricing.cache_create,
                cache_read_per_tok: model_pricing.cache_read,
            });
        }
    }

    None
}

pub(crate) fn static_pricing_lookup(model_id: &str) -> Option<Pricing> {
    // Prefer exact/known variants before family heuristics
    let m = model_id.to_lowercase();
    // Opus 4.5 (and catch generic "claude-4-5" as flagship/Opus)
    if m.contains("opus-4-5") || m == "claude-4-5" {
        let in_pt = 15e-6; // $15 / 1M (assumed)
        return Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 75e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        });
    }
    // Opus 4.1
    if m.contains("opus-4-1") {
        let in_pt = 15e-6; // $15 / 1M
        return Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 75e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        });
    }
    // Opus 4 (avoid matching 4.1 above)
    if m.contains("opus-4") {
        let in_pt = 15e-6;
        return Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 75e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        });
    }
    // Sonnet 4 (also catch "claude-4-sonnet")
    if m.contains("sonnet-4") || m.contains("4-sonnet") {
        let in_pt = 3e-6; // $3 / 1M
        return Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 15e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        });
    }
    // Claude 3.7 Sonnet
    if m.contains("3-7-sonnet") {
        let in_pt = 3e-6; // treat like sonnet family
        return Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 15e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        });
    }
    // Claude 3.5 Sonnet
    if m.contains("3-5-sonnet") {
        let in_pt = 3e-6;
        return Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 15e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        });
    }
    if m.contains("3-5-haiku") {
        return Some(Pricing {
            in_per_tok: 0.8e-6,  // $0.8 / 1M
            out_per_tok: 4.0e-6, // $4 / 1M
            cache_create_per_tok: 1.0e-6,
            cache_read_per_tok: 0.08e-6,
        });
    }
    None
}

pub fn pricing_for_model(model_id: &str) -> Option<Pricing> {
    let m = model_id.to_lowercase();

    // Priority 1: Environment variable overrides (when all four are provided)
    if let (Ok(gi), Ok(go), Ok(gc), Ok(gr)) = (
        env::var("CLAUDE_PRICE_INPUT").map(|s| s.parse::<f64>()),
        env::var("CLAUDE_PRICE_OUTPUT").map(|s| s.parse::<f64>()),
        env::var("CLAUDE_PRICE_CACHE_CREATE").map(|s| s.parse::<f64>()),
        env::var("CLAUDE_PRICE_CACHE_READ").map(|s| s.parse::<f64>()),
    ) {
        if let (Ok(ii), Ok(oo), Ok(cc), Ok(cr)) = (gi, go, gc, gr) {
            return Some(Pricing {
                in_per_tok: ii,
                out_per_tok: oo,
                cache_create_per_tok: cc,
                cache_read_per_tok: cr,
            });
        }
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
        let in_pt = 15e-6; // $15 / 1M
        Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 75e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        })
    } else if m.contains("sonnet") {
        let in_pt = 3e-6; // $3 / 1M
        Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 15e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        })
    } else if m.contains("haiku") {
        let in_pt = 0.25e-6; // $0.25 / 1M
        Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 1.25e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        })
    } else {
        None
    }
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
        // Check if this tier applies to the model
        let applies = tier
            .applies_to
            .iter()
            .any(|pattern| model_lower.contains(&pattern.to_lowercase()));

        if applies && total_input_tokens > tier.threshold {
            // Apply multipliers
            return Pricing {
                in_per_tok: base_pricing.in_per_tok * tier.multipliers.input,
                out_per_tok: base_pricing.out_per_tok * tier.multipliers.output,
                cache_create_per_tok: base_pricing.cache_create_per_tok
                    * tier.multipliers.cache_create,
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
        // Test opus family fallback
        let opus_fallback = pricing_for_model("some-future-opus-model").unwrap();
        assert_eq!(opus_fallback.in_per_tok, 15e-6);
        assert_eq!(opus_fallback.out_per_tok, 75e-6);

        // Test sonnet family fallback
        let sonnet_fallback = pricing_for_model("some-future-sonnet-model").unwrap();
        assert_eq!(sonnet_fallback.in_per_tok, 3e-6);
        assert_eq!(sonnet_fallback.out_per_tok, 15e-6);
    }

    #[test]
    fn test_unknown_model() {
        assert!(pricing_for_model("unknown-model").is_none());
    }
}
