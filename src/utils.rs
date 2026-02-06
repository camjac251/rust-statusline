use chrono::{DateTime, Local, NaiveDate, Utc};
use std::env;
use std::io::Read;
use std::path::PathBuf;

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
        let val = n as f64 / 1e9;
        if val.fract() == 0.0 {
            format!("{}B", val as u64)
        } else {
            format!("{:.1}B", val)
        }
    } else if n >= 1_000_000 {
        let val = n as f64 / 1e6;
        if val.fract() == 0.0 {
            format!("{}M", val as u64)
        } else {
            format!("{:.1}M", val)
        }
    } else if n >= 1_000 {
        let val = n as f64 / 1e3;
        if val.fract() == 0.0 {
            format!("{}K", val as u64)
        } else {
            format!("{:.1}K", val)
        }
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
    if m.contains("haiku-4") || m.contains("3-5-haiku") {
        return Some(200_000);
    }
    None
}

// Context limit detection (fallback when hook.context_window.context_window_size is unavailable):
// Priority order:
// 1. CLAUDE_CONTEXT_LIMIT env var (always wins if set)
// 2. Display name heuristics: "[1m]" tag, "1m" + "context", model id with "-1m" or ending in "1m"
// 3. Static model ID lookup (known Claude models)
// 4. Default: 200,000
//
// Note: When Claude Code 2.0.69+ provides context_window in the hook JSON, that takes
// precedence over all of these heuristics. This function is only used as a fallback.
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
const MAX_OUTPUT_CAP: u64 = 32_000;
const DEFAULT_AUTOCOMPACT_HEADROOM: u64 = 13_000;

const MAX_OUTPUT_SONNET_4: u64 = 64_000;
const MAX_OUTPUT_OPUS_32K: u64 = 32_000; // Opus 4, 4.1
const MAX_OUTPUT_OPUS_128K: u64 = 128_000; // Opus 4.6+
const MAX_OUTPUT_HAIKU_4: u64 = 64_000;
const MAX_OUTPUT_LEGACY: u64 = 8_192;

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
        if let Some(val) = parse_u64_env("CLAUDE_CODE_MAX_OUTPUT_TOKENS") {
            return val.min(SMALL_MODEL_OUTPUT_RESERVE);
        }
        return SMALL_MODEL_OUTPUT_RESERVE;
    }
    if let Some(val) = parse_u64_env("CLAUDE_CODE_MAX_OUTPUT_TOKENS") {
        return val.min(MAX_OUTPUT_CAP);
    }
    DEFAULT_OUTPUT_RESERVE
}

pub fn auto_compact_enabled() -> bool {
    parse_bool_env("CLAUDE_AUTO_COMPACT_ENABLED")
}

pub fn auto_compact_headroom_tokens() -> u64 {
    if let Some(pct) = parse_u64_env("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE") {
        let pct_fraction = (pct as f64 / 100.0).min(1.0);
        let calculated = (168_000.0 * pct_fraction) as u64;
        return calculated.min(DEFAULT_AUTOCOMPACT_HEADROOM);
    }
    parse_u64_env("CLAUDE_AUTO_COMPACT_HEADROOM").unwrap_or(DEFAULT_AUTOCOMPACT_HEADROOM)
}

pub fn system_overhead_tokens() -> u64 {
    parse_u64_env("CLAUDE_SYSTEM_OVERHEAD").unwrap_or(0)
}

pub fn max_output_capability(model_id: &str) -> u64 {
    let lower = model_id.to_lowercase();
    if lower.contains("4") {
        if lower.contains("opus") {
            if lower.contains("opus-4-6") {
                MAX_OUTPUT_OPUS_128K
            } else if lower.contains("opus-4-5") {
                MAX_OUTPUT_SONNET_4 // 64K
            } else {
                MAX_OUTPUT_OPUS_32K
            }
        } else if lower.contains("haiku") {
            MAX_OUTPUT_HAIKU_4
        } else {
            MAX_OUTPUT_SONNET_4
        }
    } else {
        MAX_OUTPUT_LEGACY
    }
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

/// Convert a raw model ID into a friendly display name when Claude Code
/// sends the raw ID as the display_name (e.g. `"claude-opus-4-6"` instead
/// of `"Claude Opus 4.6"`).  Returns the original `display_name` if it
/// already looks friendly (contains uppercase letters or spaces) or if the
/// ID doesn't follow a recognisable Claude naming pattern.
///
/// Handles both current (`claude-{family}-{ver}`) and legacy
/// (`claude-{ver}-{family}`) naming schemes, with optional date suffixes
/// and Bedrock `anthropic.` prefixes.
pub fn friendly_model_name(model_id: &str, display_name: &str) -> String {
    // If display_name already looks like a proper friendly name, keep it.
    if display_name.contains(' ') || display_name.chars().any(|c| c.is_uppercase()) {
        return display_name.to_string();
    }

    let id = model_id.to_lowercase();

    // Strip known prefixes
    let stripped = if let Some(s) = id.strip_prefix("claude-") {
        s
    } else if let Some(s) = id.strip_prefix("anthropic.claude-") {
        s
    } else {
        return display_name.to_string();
    };

    // Strip date suffix -YYYYMMDD (exactly 8 digits preceded by '-')
    let without_date = if stripped.len() >= 9 {
        let (head, tail) = stripped.split_at(stripped.len() - 8);
        if tail.chars().all(|c| c.is_ascii_digit()) && head.ends_with('-') {
            &head[..head.len() - 1]
        } else {
            stripped
        }
    } else {
        stripped
    };

    // Strip Bedrock version suffix like -v1, -v1:0
    let without_suffix = without_date
        .split_once("-v")
        .and_then(|(prefix, rest)| {
            if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                Some(prefix)
            } else {
                None
            }
        })
        .unwrap_or(without_date);

    const FAMILIES: &[&str] = &["opus", "sonnet", "haiku", "instant"];

    for family in FAMILIES {
        // Current format: {family}-{version} e.g. "opus-4-6"
        if let Some(rest) = without_suffix.strip_prefix(&format!("{}-", family)) {
            let version = rest.replace('-', ".");
            return format!("{} {}", capitalize(family), version);
        }
        // Legacy format: {version}-{family} e.g. "3-5-sonnet"
        if let Some(rest) = without_suffix.strip_suffix(&format!("-{}", family)) {
            let version = rest.replace('-', ".");
            return format!("{} {}", capitalize(family), version);
        }
    }

    // No family found but still claude- prefix: "Claude {version}"
    let version = without_suffix.replace('-', ".");
    if !version.is_empty() {
        return format!("Claude {}", version);
    }

    display_name.to_string()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

pub fn sanitized_project_name(project_dir: &str) -> String {
    project_dir
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

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

        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_CONTEXT_LIMIT", "123456") };
        assert_eq!(
            context_limit_for_model_display("claude-3.5-sonnet", "Claude 3.5 Sonnet"),
            123456
        );
        unsafe { env::remove_var("CLAUDE_CONTEXT_LIMIT") };
    }

    #[test]
    #[serial]
    fn test_reserved_output_tokens_for_model() {
        // Test 3-5/haiku models get 8192 reserve
        assert_eq!(reserved_output_tokens_for_model("claude-3-5-sonnet"), 8_192);
        assert_eq!(reserved_output_tokens_for_model("claude-3-5-haiku"), 8_192);

        // Test Sonnet 4.5 gets 32000 reserve
        assert_eq!(
            reserved_output_tokens_for_model("claude-sonnet-4-5"),
            32_000
        );

        // Test env override for Sonnet 4.5 (capped at 32000)
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "16000") };
        assert_eq!(
            reserved_output_tokens_for_model("claude-sonnet-4-5"),
            16_000
        );

        // Test env override exceeding cap
        unsafe { env::set_var("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "64000") };
        assert_eq!(
            reserved_output_tokens_for_model("claude-sonnet-4-5"),
            32_000
        );

        // Test env override for 3-5 model (capped at 8192)
        unsafe { env::set_var("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "4000") };
        assert_eq!(reserved_output_tokens_for_model("claude-3-5-sonnet"), 4_000);

        unsafe { env::set_var("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "16000") };
        assert_eq!(reserved_output_tokens_for_model("claude-3-5-sonnet"), 8_192);

        unsafe { env::remove_var("CLAUDE_CODE_MAX_OUTPUT_TOKENS") };
    }

    #[test]
    #[serial]
    fn test_usable_context_limit() {
        // Test Sonnet 4.5: 200k - 32k = 168k
        assert_eq!(
            usable_context_limit("claude-sonnet-4-5", "Claude Sonnet 4.5"),
            168_000
        );

        // Test 3.5 Sonnet: 200k - 8192 = 191808
        assert_eq!(
            usable_context_limit("claude-3-5-sonnet", "Claude 3.5 Sonnet"),
            191_808
        );

        // Test 1M context variant: 1M - 32k = 968k
        assert_eq!(
            usable_context_limit("claude-sonnet-4-5", "Claude Sonnet 4.5 [1m]"),
            968_000
        );

        // Test with env override
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "16000") };
        assert_eq!(
            usable_context_limit("claude-sonnet-4-5", "Claude Sonnet 4.5"),
            184_000
        );

        unsafe { env::remove_var("CLAUDE_CODE_MAX_OUTPUT_TOKENS") };
    }

    #[test]
    fn test_friendly_model_name_current_format() {
        // Current naming: claude-{family}-{major}-{minor}
        assert_eq!(
            friendly_model_name("claude-opus-4-6", "claude-opus-4-6"),
            "Opus 4.6"
        );
        assert_eq!(
            friendly_model_name("claude-sonnet-4-5", "claude-sonnet-4-5"),
            "Sonnet 4.5"
        );
        assert_eq!(
            friendly_model_name("claude-haiku-4-5", "claude-haiku-4-5"),
            "Haiku 4.5"
        );
        assert_eq!(
            friendly_model_name("claude-opus-4", "claude-opus-4"),
            "Opus 4"
        );
    }

    #[test]
    fn test_friendly_model_name_with_date() {
        assert_eq!(
            friendly_model_name("claude-sonnet-4-5-20250929", "claude-sonnet-4-5-20250929"),
            "Sonnet 4.5"
        );
        assert_eq!(
            friendly_model_name("claude-opus-4-1-20250805", "claude-opus-4-1-20250805"),
            "Opus 4.1"
        );
    }

    #[test]
    fn test_friendly_model_name_legacy_format() {
        // Legacy naming: claude-{major}-{minor}-{family}
        assert_eq!(
            friendly_model_name("claude-3-5-sonnet-20241022", "claude-3-5-sonnet-20241022"),
            "Sonnet 3.5"
        );
        assert_eq!(
            friendly_model_name("claude-3-opus-20240229", "claude-3-opus-20240229"),
            "Opus 3"
        );
        assert_eq!(
            friendly_model_name("claude-3-haiku-20240307", "claude-3-haiku-20240307"),
            "Haiku 3"
        );
    }

    #[test]
    fn test_friendly_model_name_bedrock() {
        assert_eq!(
            friendly_model_name(
                "anthropic.claude-opus-4-6-v1",
                "anthropic.claude-opus-4-6-v1"
            ),
            "Opus 4.6"
        );
    }

    #[test]
    fn test_friendly_model_name_no_family() {
        // No family in ID → "Claude {version}"
        assert_eq!(
            friendly_model_name("claude-4-5", "claude-4-5"),
            "Claude 4.5"
        );
    }

    #[test]
    fn test_friendly_model_name_already_friendly() {
        // Already has uppercase/spaces → returned as-is
        assert_eq!(
            friendly_model_name("claude-opus-4-6", "Claude Opus 4.6"),
            "Claude Opus 4.6"
        );
        assert_eq!(
            friendly_model_name("claude-sonnet-4-5", "Sonnet 4.5"),
            "Sonnet 4.5"
        );
    }

    #[test]
    fn test_friendly_model_name_non_claude() {
        // Non-Claude models → returned as-is
        assert_eq!(friendly_model_name("gpt-4o", "gpt-4o"), "gpt-4o");
        assert_eq!(
            friendly_model_name("gemini-2.5-pro", "gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
    }
}
