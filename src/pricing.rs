//! # Pricing Module
//!
//! Provides model-specific pricing information for cost calculations.
//!
//! ## Pricing Structure
//!
//! Each model has pricing for:
//! - Input tokens
//! - Output tokens  
//! - Cache creation (typically 1.25x input price)
//! - Cache reads (typically 0.1x input price)
//!
//! Prices can be overridden via environment variables:
//! - `CLAUDE_PRICE_INPUT`
//! - `CLAUDE_PRICE_OUTPUT`
//! - `CLAUDE_PRICE_CACHE_CREATE`
//! - `CLAUDE_PRICE_CACHE_READ`

use std::env;

#[derive(Clone, Copy)]
pub struct Pricing {
    pub in_per_tok: f64,
    pub out_per_tok: f64,
    pub cache_create_per_tok: f64,
    pub cache_read_per_tok: f64,
}

pub(crate) fn static_pricing_lookup(model_id: &str) -> Option<Pricing> {
    // Prefer exact/known variants before family heuristics
    let m = model_id.to_lowercase();
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
    // Claude 3.5 Haiku
    if m.contains("3-5-haiku") {
        let in_pt = 0.25e-6;
        return Some(Pricing {
            in_per_tok: in_pt,
            out_per_tok: 1.25e-6,
            cache_create_per_tok: in_pt * 1.25,
            cache_read_per_tok: in_pt * 0.1,
        });
    }
    None
}

pub fn pricing_for_model(model_id: &str) -> Option<Pricing> {
    let m = model_id.to_lowercase();
    // Static per-token prices in USD (per token).
    // Assumptions: cache write ≈ 1.25× input price; cache read ≈ 0.1× input price
    // Env overrides take precedence when all four are provided.
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

    // Prefer explicit known model variants
    if let Some(p) = static_pricing_lookup(&m) {
        return Some(p);
    }
    // Family heuristics
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
