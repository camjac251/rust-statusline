use chrono::{DateTime, Utc};
use directories::BaseDirs;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::process::Command;
use std::time::Duration;

use crate::db;

const DEFAULT_USER_AGENT: &str = "claude-code";
const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const CACHE_TTL_SECONDS: i64 = 60;
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const API_CACHE_KEY: &str = "oauth_usage_summary";
const USER_AGENT_CACHE_KEY: &str = "user_agent_header";
const USER_AGENT_CACHE_TTL_SECONDS: i64 = 86_400;

static USER_AGENT: Lazy<String> = Lazy::new(resolve_user_agent);
static VERSION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(\d+\.\d+\.\d+(?:-[A-Za-z0-9.]+)?)").unwrap());

fn resolve_user_agent() -> String {
    if let Some(explicit) = env_user_agent_override() {
        persist_user_agent(&explicit);
        return explicit;
    }

    let version_override = env_version_override();

    if version_override.is_none() && !force_refresh_user_agent() {
        if let Some(cached) = cached_user_agent() {
            return cached;
        }
    }

    if let Some(version) = version_override
        .or_else(package_json_version)
        .or_else(cli_version)
    {
        let agent = format!("claude-code/{version}");
        persist_user_agent(&agent);
        return agent;
    }

    let fallback = DEFAULT_USER_AGENT.to_string();
    persist_user_agent(&fallback);
    fallback
}

fn cached_user_agent() -> Option<String> {
    match db::load_metadata(USER_AGENT_CACHE_KEY) {
        Ok(Some(entry)) => {
            if let Some(ts) = entry.updated_at {
                let age = Utc::now().timestamp().saturating_sub(ts);
                if age > USER_AGENT_CACHE_TTL_SECONDS {
                    return None;
                }
            }
            Some(entry.value)
        }
        _ => None,
    }
}

fn persist_user_agent(value: &str) {
    let _ = db::store_metadata(USER_AGENT_CACHE_KEY, value);
}

fn force_refresh_user_agent() -> bool {
    match env::var("CLAUDE_STATUSLINE_FORCE_REFRESH_USER_AGENT") {
        Ok(val) => matches!(
            val.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn env_user_agent_override() -> Option<String> {
    env::var("CLAUDE_STATUSLINE_USER_AGENT")
        .ok()
        .and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
}

fn env_version_override() -> Option<String> {
    for key in ["CLAUDE_STATUSLINE_CLAUDE_VERSION", "CLAUDE_CODE_VERSION"] {
        if let Ok(value) = env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    None
}

fn package_json_version() -> Option<String> {
    let path = env::var("CLAUDE_STATUSLINE_CLAUDE_PACKAGE_JSON")
        .or_else(|_| env::var("CLAUDE_CODE_PACKAGE_JSON"))
        .ok()?;

    let contents = fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&contents).ok()?;
    json.get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
}

fn cli_version() -> Option<String> {
    let output = Command::new("claude").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(version) = extract_version(stdout.as_ref()) {
        return Some(version);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    extract_version(stderr.as_ref())
}

fn extract_version(text: &str) -> Option<String> {
    VERSION_RE
        .captures(text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn fetch_enabled() -> bool {
    match std::env::var("CLAUDE_STATUSLINE_FETCH_USAGE") {
        Ok(val) => {
            let trimmed = val.trim();
            trimmed.is_empty()
                || matches!(
                    trimmed.to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
        }
        Err(_) => true,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageLimit {
    pub utilization: Option<f64>,
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageSummary {
    pub window: UsageLimit,
    pub seven_day: UsageLimit,
    pub seven_day_opus: UsageLimit,
    pub seven_day_oauth_apps: UsageLimit,
}

#[derive(Debug, Deserialize)]
struct UsageLimitDto {
    utilization: Option<f64>,
    used: Option<f64>,
    remaining: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_datetime")]
    resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct UsageResponseDto {
    #[serde(default)]
    five_hour: Option<UsageLimitDto>,
    #[serde(default)]
    seven_day: Option<UsageLimitDto>,
    #[serde(default)]
    seven_day_opus: Option<UsageLimitDto>,
    #[serde(default)]
    seven_day_oauth_apps: Option<UsageLimitDto>,
}

pub fn get_usage_summary() -> Option<UsageSummary> {
    if !fetch_enabled() {
        return None;
    }

    // Try to get from persistent SQLite cache first
    if let Ok(Some(cached_json)) = crate::db::get_api_cache(API_CACHE_KEY) {
        if let Ok(summary) = serde_json::from_str::<UsageSummary>(&cached_json) {
            return Some(summary);
        }
    }

    // Cache miss or invalid - fetch from API
    let summary = fetch_usage_summary()?;

    // Store in persistent cache
    if let Ok(json) = serde_json::to_string(&summary) {
        let _ = crate::db::set_api_cache(API_CACHE_KEY, &json, CACHE_TTL_SECONDS);
    }

    Some(summary)
}

fn fetch_usage_summary() -> Option<UsageSummary> {
    let token = find_oauth_token()?;
    let agent = ureq::AgentBuilder::new()
        .timeout_read(Duration::from_secs(5))
        .timeout_write(Duration::from_secs(5))
        .build();

    let response = agent
        .get(USAGE_ENDPOINT)
        .set("Authorization", &format!("Bearer {}", token))
        .set("User-Agent", USER_AGENT.as_str())
        .set("Accept", "application/json")
        .set("anthropic-beta", ANTHROPIC_BETA)
        .call()
        .ok()?;

    if response.status() != 200 {
        return None;
    }

    let dto: UsageResponseDto = response.into_json().ok()?;
    Some(UsageSummary {
        window: dto.five_hour.map(UsageLimit::from).unwrap_or_default(),
        seven_day: dto.seven_day.map(UsageLimit::from).unwrap_or_default(),
        seven_day_opus: dto.seven_day_opus.map(UsageLimit::from).unwrap_or_default(),
        seven_day_oauth_apps: dto
            .seven_day_oauth_apps
            .map(UsageLimit::from)
            .unwrap_or_default(),
    })
}

impl From<UsageLimitDto> for UsageLimit {
    fn from(value: UsageLimitDto) -> Self {
        UsageLimit {
            utilization: value.utilization,
            used: value.used,
            remaining: value.remaining,
            resets_at: value.resets_at,
        }
    }
}

fn find_oauth_token() -> Option<String> {
    for env in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"] {
        if let Ok(val) = std::env::var(env) {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    let base_dirs = BaseDirs::new()?;
    let credentials_path = base_dirs
        .home_dir()
        .join(".claude")
        .join(".credentials.json");
    let raw = fs::read_to_string(credentials_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let access = json
        .get("claudeAiOauth")
        .and_then(|v| v.get("accessToken"))
        .and_then(|v| v.as_str())?
        .trim()
        .to_string();
    if access.is_empty() {
        None
    } else {
        Some(access)
    }
}

fn deserialize_optional_datetime<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    if let Some(s) = opt {
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(serde::de::Error::custom)
    } else {
        Ok(None)
    }
}
