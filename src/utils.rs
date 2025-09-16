use chrono::{DateTime, Local, NaiveDate, Utc};
use std::env;
use std::io::Read;
use std::path::PathBuf;

use crate::cli::{Args, PlanProfileArg, PlanTierArg};

// Default 5-hour base tokens for Pro. Many ccusage users operate with 250k as the
// effective base; Max tiers are derived as 5x and 20x. You can override via
// CLAUDE_5H_BASE_TOKENS or set a hard numeric with CLAUDE_PLAN_MAX_TOKENS.
pub const BASE_TOKEN_LIMIT: f64 = 250_000.0;
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
    // Robust 1M detection:
    // - explicit display tag "[1m]"
    // - any mention of "1m" + "context" (e.g., "with 1M context")
    // - model id containing "1m" (e.g., "sonnet-1m")
    let dn_l = display_name.to_lowercase();
    let mid_l = model_id.to_lowercase();
    if dn_l.contains("[1m]")
        || (dn_l.contains("1m") && dn_l.contains("context"))
        || mid_l.contains("-1m")
        || mid_l.ends_with("1m")
    {
        return 1_000_000;
    }
    if let Some(v) = static_context_limit_lookup(model_id) {
        return v;
    }
    200_000
}

const DEFAULT_OUTPUT_RESERVE: u64 = 32_000;
const SMALL_MODEL_OUTPUT_RESERVE: u64 = 8_192;
const DEFAULT_AUTOCOMPACT_HEADROOM: u64 = 13_000;

fn parse_bool_env(var: &str) -> bool {
    if let Ok(val) = env::var(var) {
        let trimmed = val.trim();
        trimmed == "1" || trimmed.eq_ignore_ascii_case("true")
    } else {
        false
    }
}

fn parse_u64_env(var: &str) -> Option<u64> {
    env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
}

pub fn reserved_output_tokens_for_model(model_id: &str) -> u64 {
    let lower = model_id.to_lowercase();
    if lower.contains("3-5") || lower.contains("haiku") {
        return SMALL_MODEL_OUTPUT_RESERVE;
    }
    if let Some(val) = parse_u64_env("CLAUDE_CODE_MAX_OUTPUT_TOKENS") {
        if val == 0 {
            return DEFAULT_OUTPUT_RESERVE;
        }
        return val.min(DEFAULT_OUTPUT_RESERVE);
    }
    DEFAULT_OUTPUT_RESERVE
}

pub fn auto_compact_enabled() -> bool {
    parse_bool_env("CLAUDE_AUTO_COMPACT_ENABLED")
}

pub fn auto_compact_headroom_tokens() -> u64 {
    parse_u64_env("CLAUDE_AUTO_COMPACT_HEADROOM").unwrap_or(DEFAULT_AUTOCOMPACT_HEADROOM)
}

pub fn system_overhead_tokens() -> u64 {
    parse_u64_env("CLAUDE_SYSTEM_OVERHEAD").unwrap_or(0)
}

pub fn usable_context_limit(model_id: &str, display_name: &str) -> u64 {
    let base = context_limit_for_model_display(model_id, display_name);
    let reserve = reserved_output_tokens_for_model(model_id);
    let mut usable = base.saturating_sub(reserve);
    if auto_compact_enabled() {
        usable = usable.saturating_sub(auto_compact_headroom_tokens());
    }
    usable
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
        let base: f64 = five_hour_base_tokens();
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

/// Read the base five-hour token limit for Pro (defaults to 200k). This is used to derive
/// Max 5x/20x caps when exact caps are not known.
pub fn five_hour_base_tokens() -> f64 {
    if let Ok(s) = env::var("CLAUDE_5H_BASE_TOKENS") {
        if let Ok(v) = s.parse::<f64>() {
            if v.is_finite() && v > 0.0 {
                return v;
            }
        }
    }
    BASE_TOKEN_LIMIT
}

/// Auto-detect plan tier based on token usage in current window
pub fn auto_detect_plan_tier(window_tokens: f64) -> Option<String> {
    // Use configured base to infer tier boundaries: base → pro, base*5 → max5x, base*20 → max20x
    let base = five_hour_base_tokens();
    if window_tokens > base * 5.0 {
        Some("max20x".to_string())
    } else if window_tokens > base {
        Some("max5x".to_string())
    } else if window_tokens > 0.0 {
        Some("pro".to_string())
    } else {
        None
    }
}

/// Resolve plan tier and max tokens from CLI args, environment, and settings.json overrides
pub fn resolve_plan_config(args: &Args) -> (Option<String>, Option<f64>) {
    let (env_tier, env_max_tokens) = plan_from_env();
    let settings = read_settings_overrides();
    // Plan profile from CLI or env (CLAUDE_PLAN_PROFILE)
    let plan_profile_env = env::var("CLAUDE_PLAN_PROFILE")
        .ok()
        .map(|s| s.to_lowercase());
    let plan_profile_settings = settings.as_ref().and_then(|s| s.plan_profile.clone());
    let plan_profile_cli: Option<String> = args.plan_profile.map(|p| match p {
        PlanProfileArg::Standard => "standard".to_string(),
        PlanProfileArg::Monitor => "monitor".to_string(),
    });
    // Default profile now aligns with Claude Code Usage Monitor behavior
    // (pro=19k, max5x=88k, max20x=220k)
    let plan_profile_final = plan_profile_cli
        .or(plan_profile_env)
        .or(plan_profile_settings)
        .unwrap_or_else(|| "monitor".to_string());
    let plan_tier_cli: Option<String> = args.plan_tier.map(|t| match t {
        PlanTierArg::Pro => "pro".to_string(),
        PlanTierArg::Max5x => "max5x".to_string(),
        PlanTierArg::Max20x => "max20x".to_string(),
    });
    let plan_max_cli: Option<f64> = args.plan_max_tokens.map(|v| v as f64);
    let plan_tier_settings = settings.as_ref().and_then(|s| s.plan_tier.clone());
    let plan_max_settings = settings.as_ref().and_then(|s| s.plan_max_tokens);
    let plan_tier_final: Option<String> = plan_tier_cli.or(env_tier).or(plan_tier_settings);
    let plan_max: Option<f64> = plan_max_cli
        .or(env_max_tokens)
        .or(plan_max_settings)
        .or_else(|| {
            if let Some(ref t) = plan_tier_final {
                match plan_profile_final.as_str() {
                    // Standard mapping: base * {1,5,20}; base defaults to 200k and can be overridden by CLAUDE_5H_BASE_TOKENS
                    "standard" => {
                        let base = five_hour_base_tokens();
                        let mult = match t.as_str() {
                            "pro" => 1.0,
                            "max5x" => 5.0,
                            "max20x" => 20.0,
                            _ => 0.0,
                        };
                        if mult > 0.0 {
                            return Some(base * mult);
                        }
                    }
                    // Monitor mapping: fixed caps per tier (≈19k/88k/220k)
                    "monitor" => {
                        let val = match t.as_str() {
                            "pro" => Some(19_000.0),
                            "max5x" => Some(88_000.0),
                            "max20x" => Some(220_000.0),
                            _ => None,
                        };
                        if let Some(v) = val {
                            return Some(v);
                        }
                    }
                    _ => {}
                }
            }
            None
        });
    (plan_tier_final, plan_max)
}

// --- Settings overrides loader ---
#[derive(Debug, Clone, Default)]
struct SettingsOverrides {
    plan_tier: Option<String>,
    plan_profile: Option<String>,
    plan_max_tokens: Option<f64>,
}

fn read_settings_overrides() -> Option<SettingsOverrides> {
    // Allow override path via env
    let override_path = env::var("CLAUDE_SETTINGS_FILE").ok();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = override_path {
        candidates.push(PathBuf::from(p));
    }
    if let Some(b) = directories::BaseDirs::new() {
        candidates.push(b.home_dir().join(".claude").join("settings.json"));
        candidates.push(b.config_dir().join("claude").join("settings.json"));
    }
    for path in candidates {
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(mut s) = parse_settings_overrides(&content) {
                    // args-like overrides (arrays/strings)
                    if s.plan_tier.is_none()
                        || s.plan_profile.is_none()
                        || s.plan_max_tokens.is_none()
                    {
                        if let Some((tier, profile, max)) = parse_args_like_overrides(&content) {
                            if s.plan_tier.is_none() {
                                s.plan_tier = tier;
                            }
                            if s.plan_profile.is_none() {
                                s.plan_profile = profile;
                            }
                            if s.plan_max_tokens.is_none() {
                                s.plan_max_tokens = max;
                            }
                        }
                    }
                    // statusLine.command overrides (a full command string)
                    if s.plan_tier.is_none()
                        || s.plan_profile.is_none()
                        || s.plan_max_tokens.is_none()
                    {
                        if let Some((tier, profile, max)) = parse_command_line_overrides(&content) {
                            if s.plan_tier.is_none() {
                                s.plan_tier = tier;
                            }
                            if s.plan_profile.is_none() {
                                s.plan_profile = profile;
                            }
                            if s.plan_max_tokens.is_none() {
                                s.plan_max_tokens = max;
                            }
                        }
                    }
                    return Some(s);
                }
            }
        }
    }
    None
}

fn parse_settings_overrides(content: &str) -> anyhow::Result<SettingsOverrides> {
    let v: serde_json::Value = serde_json::from_str(content)?;
    let sections = [
        "statusline",
        "claude_statusline",
        "claudeStatusline",
        "claude-statusline",
    ];
    for key in sections {
        if let Some(obj) = v.get(key).and_then(|x| x.as_object()) {
            return Ok(SettingsOverrides {
                plan_tier: get_string_any(obj, &["plan_tier", "planTier", "plan-tier", "tier"])
                    .map(|s| s.to_lowercase()),
                plan_profile: get_string_any(
                    obj,
                    &["plan_profile", "planProfile", "plan-profile", "profile"],
                )
                .map(|s| s.to_lowercase()),
                plan_max_tokens: get_number_any(
                    obj,
                    &[
                        "plan_max_tokens",
                        "planMaxTokens",
                        "plan-max-tokens",
                        "max_tokens",
                        "maxTokens",
                    ],
                ),
            });
        }
    }
    if let Some(obj) = v.as_object() {
        return Ok(SettingsOverrides {
            plan_tier: get_string_any(obj, &["plan_tier", "planTier", "plan-tier", "tier"])
                .map(|s| s.to_lowercase()),
            plan_profile: get_string_any(
                obj,
                &["plan_profile", "planProfile", "plan-profile", "profile"],
            )
            .map(|s| s.to_lowercase()),
            plan_max_tokens: get_number_any(
                obj,
                &[
                    "plan_max_tokens",
                    "planMaxTokens",
                    "plan-max-tokens",
                    "max_tokens",
                    "maxTokens",
                ],
            ),
        });
    }
    Ok(SettingsOverrides::default())
}

fn parse_args_like_overrides(
    content: &str,
) -> Option<(Option<String>, Option<String>, Option<f64>)> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    let mut args_vec: Vec<String> = Vec::new();
    let cand = [
        (None, "statusline_args"),
        (Some("statusline"), "args"),
        (Some("claude_statusline"), "args"),
        (Some("claudeStatusline"), "args"),
        (Some("claude-statusline"), "args"),
    ];
    for (ns, key) in cand.iter() {
        let val_opt = match ns {
            Some(nskey) => v.get(nskey).and_then(|s| s.get(*key)),
            None => v.get(*key),
        };
        if let Some(val) = val_opt {
            if let Some(arr) = val.as_array() {
                for it in arr {
                    if let Some(s) = it.as_str() {
                        args_vec.push(s.to_string());
                    }
                }
            } else if let Some(s) = val.as_str() {
                for part in s.split_whitespace() {
                    if !part.is_empty() {
                        args_vec.push(part.to_string());
                    }
                }
            }
        }
    }
    if args_vec.is_empty() {
        return None;
    }
    let mut tier: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut max_tok: Option<f64> = None;
    let mut it = args_vec.iter();
    while let Some(tok) = it.next() {
        match tok.as_str() {
            "--plan-tier" => {
                if let Some(v) = it.next() {
                    tier = Some(v.to_lowercase());
                }
            }
            "--plan-profile" => {
                if let Some(v) = it.next() {
                    profile = Some(v.to_lowercase());
                }
            }
            "--plan-max-tokens" => {
                if let Some(v) = it.next() {
                    if let Ok(n) = v.parse::<f64>() {
                        max_tok = Some(n);
                    }
                }
            }
            _ => {}
        }
    }
    Some((tier, profile, max_tok))
}

fn parse_command_line_overrides(
    content: &str,
) -> Option<(Option<String>, Option<String>, Option<f64>)> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    // Look for statusLine.command (Claude Code style), and fallback to statusline.command
    let cmd_val = v
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .or_else(|| v.get("statusline").and_then(|s| s.get("command")))?;
    let cmd = cmd_val.as_str()?;
    // naive split; acceptable for typical flags; drop the binary path (first token)
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let mut it = parts.iter();
    // skip binary
    it.next();
    let mut tier: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut max_tok: Option<f64> = None;
    while let Some(tok) = it.next() {
        match *tok {
            "--plan-tier" => {
                if let Some(v) = it.next() {
                    tier = Some(v.to_string().to_lowercase());
                }
            }
            "--plan-profile" => {
                if let Some(v) = it.next() {
                    profile = Some(v.to_string().to_lowercase());
                }
            }
            "--plan-max-tokens" => {
                if let Some(v) = it.next() {
                    if let Ok(n) = v.parse::<f64>() {
                        max_tok = Some(n);
                    }
                }
            }
            _ => {}
        }
    }
    if tier.is_none() && profile.is_none() && max_tok.is_none() {
        return None;
    }
    Some((tier, profile, max_tok))
}

fn get_string_any<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn get_number_any(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(n) = obj.get(*k) {
            if let Some(i) = n.as_i64() {
                return Some(i as f64);
            }
            if let Some(f) = n.as_f64() {
                return Some(f);
            }
            if let Some(s) = n.as_str() {
                if let Ok(v) = s.parse::<f64>() {
                    return Some(v);
                }
            }
        }
    }
    None
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
    fn test_auto_detect_plan_tier() {
        assert_eq!(auto_detect_plan_tier(0.0), None);
        assert_eq!(auto_detect_plan_tier(100_000.0), Some("pro".to_string()));
        assert_eq!(auto_detect_plan_tier(200_000.0), Some("pro".to_string()));
        assert_eq!(auto_detect_plan_tier(200_001.0), Some("max5x".to_string()));
        assert_eq!(auto_detect_plan_tier(500_000.0), Some("max5x".to_string()));
        assert_eq!(
            auto_detect_plan_tier(1_000_000.0),
            Some("max5x".to_string())
        );
        assert_eq!(
            auto_detect_plan_tier(1_000_001.0),
            Some("max20x".to_string())
        );
        assert_eq!(
            auto_detect_plan_tier(2_000_000.0),
            Some("max20x".to_string())
        );
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
