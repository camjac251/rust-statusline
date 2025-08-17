use chrono::{DateTime, Local, NaiveDate, Utc};
use std::env;
use std::io::Read;
use std::path::PathBuf;

use crate::cli::{Args, PlanTierArg};

pub const BASE_TOKEN_LIMIT: f64 = 200_000.0;
pub const WINDOW_DURATION_HOURS: i64 = 5;
pub const WINDOW_DURATION_SECONDS: i64 = WINDOW_DURATION_HOURS * 60 * 60;

pub fn claude_paths(override_env: Option<&str>) -> Vec<PathBuf> {
    let mut paths = vec![];
    if let Some(list) = override_env {
        let list = list.trim();
        if !list.is_empty() {
            for p in list.split(',') {
                let p = p.trim();
                if p.is_empty() {
                    continue;
                }
                let pb = PathBuf::from(p);
                if pb.join("projects").is_dir() {
                    paths.push(pb);
                }
            }
            if !paths.is_empty() {
                return paths;
            }
        }
    }
    let basedirs = directories::BaseDirs::new();
    let home = basedirs
        .as_ref()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("~"));
    let xdg_config = basedirs
        .as_ref()
        .map(|b| b.config_dir().to_path_buf())
        .unwrap_or_else(|| home.join(".config"));
    // Prefer ~/.claude, then XDG config
    for base in [home.join(".claude"), xdg_config.join("claude")].into_iter() {
        if base.join("projects").is_dir() {
            paths.push(base);
        }
    }
    paths
}

pub fn deduce_provider_from_model(model_id: &str) -> &'static str {
    let m = model_id.to_lowercase();
    if m.contains('@') {
        return "vertex";
    }
    if m.contains("anthropic") && (m.contains(":") || m.contains("us.")) {
        return "bedrock";
    }
    "anthropic"
}

pub fn read_stdin() -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    Ok(buf)
}

pub fn format_path(p: &str) -> String {
    if let Some(b) = directories::BaseDirs::new() {
        let home_s = b.home_dir().to_string_lossy();
        if p.starts_with(&*home_s) {
            return format!("~{}", &p[home_s.len()..]);
        }
    }
    p.to_owned()
}

pub fn format_currency(v: f64) -> String {
    format!("{v:.2}")
}

pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

pub fn parse_iso_date(s: &str) -> Option<NaiveDate> {
    let dt: DateTime<Utc> = DateTime::parse_from_rfc3339(s).ok()?.with_timezone(&Utc);
    // compare in local date, like ccusage tables typically show
    Some(dt.with_timezone(&Local).date_naive())
}

pub(crate) fn static_context_limit_lookup(model_id: &str) -> Option<u64> {
    let m = model_id.to_lowercase();
    // Known variants – currently all 200k; structure allows easy updates later
    if m.contains("opus-4-1") {
        return Some(200_000);
    }
    if m.contains("opus-4") {
        return Some(200_000);
    }
    if m.contains("sonnet-4") || m.contains("4-sonnet") {
        return Some(200_000);
    }
    if m.contains("3-7-sonnet") {
        return Some(200_000);
    }
    if m.contains("3-5-sonnet") {
        return Some(200_000);
    }
    if m.contains("3-5-haiku") {
        return Some(200_000);
    }
    None
}

#[allow(dead_code)]
pub fn context_limit_for_model(model_id: &str) -> u64 {
    // Allow explicit override when known
    if let Ok(override_limit) = env::var("CLAUDE_CONTEXT_LIMIT")
        .and_then(|s| s.parse::<u64>().map_err(|_| std::env::VarError::NotPresent))
    {
        return override_limit;
    }
    if let Some(v) = static_context_limit_lookup(model_id) {
        return v;
    }
    // Family fallback (uniform)
    200_000
}

// Context limit detection that mirrors Claude Code behavior:
// - If display name contains "[1m]" then treat context limit as 1,000,000 tokens
// - Otherwise use the model-id lookup and default to 200,000
// - Environment variable CLAUDE_CONTEXT_LIMIT, if set, always wins
pub fn context_limit_for_model_display(model_id: &str, display_name: &str) -> u64 {
    if let Ok(override_limit) = env::var("CLAUDE_CONTEXT_LIMIT")
        .and_then(|s| s.parse::<u64>().map_err(|_| std::env::VarError::NotPresent))
    {
        return override_limit;
    }
    if display_name.contains("[1m]") {
        return 1_000_000;
    }
    if let Some(v) = static_context_limit_lookup(model_id) {
        return v;
    }
    200_000
}

pub fn sanitized_project_name(project_dir: &str) -> String {
    project_dir
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

// Determines the per-block max units (tokens) for usage percent calculations
// Priority:
// 1) CLAUDE_PLAN_MAX_TOKENS (explicit numeric)
// 2) CLAUDE_PLAN_TIER in {pro,max5x,max20x} mapped to 200k * {1,5,20}
// Set none to hide usage percent.
pub(crate) fn plan_from_env() -> (Option<String>, Option<f64>) {
    // Returns (tier_str, max_tokens)
    let tier = env::var("CLAUDE_PLAN_TIER").ok();
    if let Ok(s) = env::var("CLAUDE_PLAN_MAX_TOKENS") {
        if let Ok(v) = s.parse::<f64>() {
            return (tier, Some(v.max(0.0)));
        }
    }
    if let Some(ref t) = tier {
        let base: f64 = 200_000.0;
        let mult = match t.to_lowercase().as_str() {
            "pro" => 1.0,
            "max5x" | "max_5x" | "5x" => 5.0,
            "max20x" | "max_20x" | "20x" => 20.0,
            _ => 0.0,
        };
        if mult > 0.0 {
            return (tier, Some(base * mult));
        }
    }
    (tier, None)
}

/// Resolve plan tier and max tokens from CLI args and environment
pub fn resolve_plan_config(args: &Args) -> (Option<String>, Option<f64>) {
    let (env_tier, env_max_tokens) = plan_from_env();
    let plan_tier_cli: Option<String> = args.plan_tier.map(|t| match t {
        PlanTierArg::Pro => "pro".to_string(),
        PlanTierArg::Max5x => "max5x".to_string(),
        PlanTierArg::Max20x => "max20x".to_string(),
    });
    let plan_max_cli: Option<f64> = args.plan_max_tokens.map(|v| v as f64);
    let plan_tier_final: Option<String> = plan_tier_cli.or(env_tier);
    let plan_max: Option<f64> = plan_max_cli.or(env_max_tokens).or_else(|| {
        if let Some(ref t) = plan_tier_final {
            let mult = match t.as_str() {
                "pro" => 1.0,
                "max5x" => 5.0,
                "max20x" => 20.0,
                _ => 0.0,
            };
            if mult > 0.0 {
                return Some(BASE_TOKEN_LIMIT * mult);
            }
        }
        None
    });
    (plan_tier_final, plan_max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    #[test]
    #[serial]
    fn test_plan_from_env() {
        env::set_var("CLAUDE_PLAN_TIER", "pro");
        let (tier, max_tokens) = plan_from_env();
        assert_eq!(tier, Some("pro".to_string()));
        assert_eq!(max_tokens, Some(200_000.0));

        env::set_var("CLAUDE_PLAN_MAX_TOKENS", "500000");
        let (tier, max_tokens) = plan_from_env();
        assert_eq!(tier, Some("pro".to_string()));
        assert_eq!(max_tokens, Some(500_000.0));

        env::remove_var("CLAUDE_PLAN_TIER");
        env::remove_var("CLAUDE_PLAN_MAX_TOKENS");
    }

    #[test]
    #[serial]
    fn test_context_limit_for_model_display() {
        assert_eq!(
            context_limit_for_model_display("claude-3.5-sonnet", "Claude 3.5 Sonnet"),
            200_000
        );
        assert_eq!(
            context_limit_for_model_display("claude-3.5-sonnet", "Claude 3.5 Sonnet [1m]"),
            1_000_000
        );

        env::set_var("CLAUDE_CONTEXT_LIMIT", "123456");
        assert_eq!(
            context_limit_for_model_display("claude-3.5-sonnet", "Claude 3.5 Sonnet"),
            123456
        );
        env::remove_var("CLAUDE_CONTEXT_LIMIT");
    }
}
