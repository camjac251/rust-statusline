use chrono::{DateTime, Utc};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::Duration;

const USER_AGENT: &str = "claude-code";
const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const CACHE_TTL_SECONDS: i64 = 60;
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const API_CACHE_KEY: &str = "oauth_usage_summary";

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
        .set("User-Agent", USER_AGENT)
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
