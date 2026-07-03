use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const ANTHROPIC_API_HOST: &str = "api.anthropic.com";
const CACHE_TTL_SECONDS: i64 = 300;
const NEGATIVE_CACHE_TTL_SECONDS: i64 = 120;
const FETCH_LOCK_TTL_SECONDS: i64 = 10;
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
/// Extra CA bundle path, matching Claude Code's proxy CA env var. When set, the
/// usage call trusts these certs (plus system roots) so it validates behind a
/// TLS-intercepting proxy whose CA is not in the public root store.
const EXTRA_CA_ENV: &str = "NODE_EXTRA_CA_CERTS";
const API_CACHE_KEY: &str = "oauth_usage_summary";
const NEGATIVE_CACHE_KEY: &str = "oauth_usage_negative";

/// Check if we're using direct Anthropic API with a Claude model.
/// Returns false if:
/// - ANTHROPIC_BASE_URL is set to a non-Anthropic endpoint (proxy detected)
/// - The model ID doesn't look like a Claude model
///
/// Used to determine if OAuth API calls make sense and if window/reset
/// display is relevant (5h window is Claude-specific).
pub fn is_direct_claude_api(model_id: Option<&str>) -> bool {
    // Check for proxy via ANTHROPIC_BASE_URL
    if let Ok(base_url) = env::var("ANTHROPIC_BASE_URL") {
        let base_url = base_url.trim().to_lowercase();
        if !base_url.is_empty() && !base_url.contains(ANTHROPIC_API_HOST) {
            return false;
        }
    }

    // If model_id provided, validate it looks like a Claude model
    if let Some(id) = model_id {
        let m = id.to_lowercase();
        // Claude models: claude-*, anthropic.claude-* (Bedrock), claude-*@* (Vertex)
        if !m.contains("claude") && !m.starts_with("anthropic.") {
            return false;
        }
    }

    true
}

/// Where the OAuth usage ("stats") API request egresses: straight to Anthropic,
/// or through an HTTP/HTTPS proxy resolved from the environment.
///
/// Resolution mirrors the real request path. It uses ureq's own
/// `Proxy::try_from_env` (the same value the request agent is built from) and
/// `NO_PROXY` matching, so the reported route is exactly what the call takes.
/// Proxy credentials are never included in any field.
#[derive(Debug, Clone, Serialize)]
pub struct UsageEgress {
    /// Human-readable route with credentials masked, e.g. `direct` or
    /// `proxy http://127.0.0.1:8080 (auth)`.
    pub route: String,
    /// True when an environment proxy carries the request.
    pub via_proxy: bool,
    /// Proxy origin as `host:port` when `via_proxy`; never contains credentials.
    pub proxy_origin: Option<String>,
    /// True when a configured proxy is bypassed by `NO_PROXY` for the usage host.
    pub no_proxy_bypass: bool,
    /// Path from `NODE_EXTRA_CA_CERTS` when set; the usage call trusts this CA
    /// bundle (plus system roots) for TLS, mirroring Claude Code.
    pub extra_ca: Option<String>,
}

/// Path from `NODE_EXTRA_CA_CERTS` if it is set to a non-empty value.
fn extra_ca_path() -> Option<String> {
    env::var(EXTRA_CA_ENV)
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// Resolve the egress route for the usage endpoint from the current environment.
pub fn resolve_usage_egress() -> UsageEgress {
    let extra_ca = extra_ca_path();
    let direct = |route: &str, bypass: bool| UsageEgress {
        route: route.to_string(),
        via_proxy: false,
        proxy_origin: None,
        no_proxy_bypass: bypass,
        extra_ca: extra_ca.clone(),
    };

    let Ok(endpoint) = USAGE_ENDPOINT.parse::<ureq::http::Uri>() else {
        return direct("direct", false);
    };

    match ureq::Proxy::try_from_env() {
        None => direct("direct", false),
        Some(proxy) if proxy.is_no_proxy(&endpoint) => direct("direct (NO_PROXY bypass)", true),
        Some(proxy) => {
            let scheme = proxy.protocol().to_string().to_lowercase();
            let origin = format!("{}:{}", proxy.host(), proxy.port());
            let auth = if proxy.username().is_some() {
                " (auth)"
            } else {
                ""
            };
            UsageEgress {
                route: format!("proxy {scheme}://{origin}{auth}"),
                via_proxy: true,
                proxy_origin: Some(origin),
                no_proxy_bypass: false,
                extra_ca,
            }
        }
    }
}

/// Parse every certificate from a PEM bundle, skipping any non-certificate items.
fn parse_ca_pem(pem: &[u8]) -> Vec<ureq::tls::Certificate<'static>> {
    ureq::tls::parse_pem(pem)
        .filter_map(|item| match item {
            Ok(ureq::tls::PemItem::Certificate(cert)) => Some(cert),
            _ => None,
        })
        .collect()
}

/// Build the TLS root set for the usage call.
///
/// Returns `None` when `NODE_EXTRA_CA_CERTS` is unset, so ureq keeps its default
/// Mozilla roots and the common path is unchanged. When set, trust the system
/// roots plus the extra CA bundle (Claude Code trusts bundled + system + extra),
/// so the call validates whether or not the proxy intercepts the usage host.
fn usage_root_certs() -> Option<ureq::tls::RootCerts> {
    let extra_path = extra_ca_path()?;

    let mut certs: Vec<ureq::tls::Certificate<'static>> = Vec::new();

    // System roots so hosts the proxy does not intercept still validate.
    let native = rustls_native_certs::load_native_certs();
    for err in &native.errors {
        eprintln!("CA load: system root store warning: {}", err);
    }
    certs.extend(
        native
            .certs
            .iter()
            .map(|der| ureq::tls::Certificate::from_der(der.as_ref()).to_owned()),
    );

    // The proxy's CA bundle (NODE_EXTRA_CA_CERTS); may contain several certs.
    match fs::read(&extra_path) {
        Ok(pem) => certs.extend(parse_ca_pem(&pem)),
        Err(e) => eprintln!(
            "CA load: cannot read {} ({}): {}",
            EXTRA_CA_ENV, extra_path, e
        ),
    }

    if certs.is_empty() {
        return None;
    }
    Some(ureq::tls::RootCerts::from(certs))
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageApiHealth {
    pub direct_claude_api: bool,
    pub oauth_token_present: bool,
    pub fresh_cache_present: bool,
    pub stale_cache_present: bool,
    pub negative_cache_active: bool,
    pub egress: UsageEgress,
}

pub fn inspect_usage_api(claude_paths: &[PathBuf], model_id: Option<&str>) -> UsageApiHealth {
    UsageApiHealth {
        direct_claude_api: is_direct_claude_api(model_id),
        oauth_token_present: find_oauth_token(claude_paths).is_some(),
        fresh_cache_present: crate::db::get_api_cache(API_CACHE_KEY)
            .ok()
            .flatten()
            .is_some(),
        stale_cache_present: crate::db::get_stale_api_cache(API_CACHE_KEY)
            .ok()
            .flatten()
            .is_some(),
        negative_cache_active: crate::db::get_api_cache(NEGATIVE_CACHE_KEY)
            .ok()
            .flatten()
            .is_some(),
        egress: resolve_usage_egress(),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageLimit {
    pub utilization: Option<f64>,
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    pub limit: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
}

impl UsageLimit {
    pub fn fill_missing_from(&mut self, other: &UsageLimit) {
        if self.utilization.is_none() {
            self.utilization = other.utilization;
        }
        if self.used.is_none() {
            self.used = other.used;
        }
        if self.remaining.is_none() {
            self.remaining = other.remaining;
        }
        if self.limit.is_none() {
            self.limit = other.limit;
        }
        if self.resets_at.is_none() {
            self.resets_at = other.resets_at;
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtraUsage {
    pub is_enabled: bool,
    pub monthly_limit: Option<f64>,
    pub used_credits: Option<f64>,
    pub utilization: Option<f64>,
    pub currency: Option<String>,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageApiLimit {
    pub kind: Option<String>,
    pub group: Option<String>,
    pub percent: Option<f64>,
    pub severity: Option<String>,
    pub resets_at: Option<DateTime<Utc>>,
    pub scope: Option<UsageLimitScope>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageLimitScope {
    pub model: Option<UsageLimitScopeModel>,
    pub surface: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageLimitScopeModel {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageMoney {
    pub amount_minor: Option<i64>,
    pub exponent: Option<i32>,
    pub currency: Option<String>,
    pub amount: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageSpend {
    pub used: Option<UsageMoney>,
    pub limit: Option<UsageMoney>,
    pub percent: Option<f64>,
    pub severity: Option<String>,
    pub enabled: Option<bool>,
    pub disabled_reason: Option<String>,
    pub can_purchase_credits: Option<bool>,
    pub can_toggle: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageSummary {
    pub window: UsageLimit,
    pub seven_day: UsageLimit,
    pub seven_day_opus: UsageLimit,
    pub seven_day_sonnet: UsageLimit,
    pub seven_day_oauth_apps: UsageLimit,
    pub seven_day_cowork: UsageLimit,
    /// One-time promotional credit shared between Claude Code and Cowork;
    /// `resets_at` is the credit's expiry rather than a window reset.
    pub cinder_cove: UsageLimit,
    pub extra_usage: Option<ExtraUsage>,
    /// Generic limit rows from the live OAuth usage response. Newer endpoint
    /// shapes put canonical session/weekly/scoped percentages here.
    pub limits: Vec<UsageApiLimit>,
    /// Newer extra-usage spend object with explicit money units.
    pub spend: Option<UsageSpend>,
    /// True when serving expired cached data after an API failure
    pub stale: bool,
}

#[derive(Debug, Deserialize)]
struct ExtraUsageDto {
    #[serde(default)]
    is_enabled: bool,
    monthly_limit: Option<f64>,
    used_credits: Option<f64>,
    utilization: Option<f64>,
    currency: Option<String>,
    disabled_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageLimitDto {
    utilization: Option<f64>,
    #[serde(alias = "used_dollars")]
    used: Option<f64>,
    #[serde(alias = "remaining_dollars")]
    remaining: Option<f64>,
    #[serde(alias = "limit_dollars")]
    limit: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_datetime")]
    resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct UsageApiLimitDto {
    kind: Option<String>,
    group: Option<String>,
    percent: Option<f64>,
    severity: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_datetime")]
    resets_at: Option<DateTime<Utc>>,
    scope: Option<UsageLimitScope>,
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UsageMoneyDto {
    amount_minor: Option<i64>,
    exponent: Option<i32>,
    currency: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageSpendDto {
    used: Option<UsageMoneyDto>,
    limit: Option<UsageMoneyDto>,
    percent: Option<f64>,
    severity: Option<String>,
    enabled: Option<bool>,
    disabled_reason: Option<String>,
    can_purchase_credits: Option<bool>,
    can_toggle: Option<bool>,
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
    seven_day_sonnet: Option<UsageLimitDto>,
    #[serde(default)]
    seven_day_oauth_apps: Option<UsageLimitDto>,
    #[serde(default)]
    seven_day_cowork: Option<UsageLimitDto>,
    #[serde(default)]
    cinder_cove: Option<UsageLimitDto>,
    #[serde(default)]
    extra_usage: Option<ExtraUsageDto>,
    #[serde(default)]
    limits: Vec<UsageApiLimitDto>,
    #[serde(default)]
    spend: Option<UsageSpendDto>,
}

pub fn get_usage_summary(claude_paths: &[PathBuf], model_id: Option<&str>) -> Option<UsageSummary> {
    // Subsystem-level disable now lives at main.rs (subsystems.usage_api). We
    // keep the direct-API guard here because it depends on env/model details
    // that the gate caller doesn't know.
    if !is_direct_claude_api(model_id) {
        return None;
    }

    // Try to get from persistent SQLite cache first
    if let Ok(Some(cached_json)) = crate::db::get_api_cache(API_CACHE_KEY) {
        if let Ok(summary) = serde_json::from_str::<UsageSummary>(&cached_json) {
            return Some(summary);
        }
    }

    // If API recently failed (429/error), don't retry -- serve stale data
    if let Ok(Some(_)) = crate::db::get_api_cache(NEGATIVE_CACHE_KEY) {
        return stale_fallback();
    }

    // Acquire fetch lock to prevent concurrent API calls across sessions.
    // Only the first process wins; others get stale data instead of racing.
    let got_lock = crate::db::try_set_api_cache(NEGATIVE_CACHE_KEY, "f", FETCH_LOCK_TTL_SECONDS)
        .unwrap_or(false);
    if !got_lock {
        return stale_fallback();
    }

    // Cache miss or invalid - fetch from API
    let summary = fetch_usage_summary(claude_paths);

    match summary {
        Some(s) => {
            // Store in persistent cache; clear the fetch lock
            if let Ok(json) = serde_json::to_string(&s) {
                let _ = crate::db::set_api_cache(API_CACHE_KEY, &json, CACHE_TTL_SECONDS);
            }
            let _ = crate::db::set_api_cache(NEGATIVE_CACHE_KEY, "", 0);
            Some(s)
        }
        None => {
            // Upgrade fetch lock to full negative cache to prevent retry storm
            let _ = crate::db::set_api_cache(NEGATIVE_CACHE_KEY, "1", NEGATIVE_CACHE_TTL_SECONDS);
            stale_fallback()
        }
    }
}

/// Return the last cached API data (even if expired), marked as stale
fn stale_fallback() -> Option<UsageSummary> {
    if let Ok(Some(json)) = crate::db::get_stale_api_cache(API_CACHE_KEY) {
        if let Ok(mut summary) = serde_json::from_str::<UsageSummary>(&json) {
            summary.stale = true;
            return Some(summary);
        }
    }
    None
}

fn fetch_usage_summary(claude_paths: &[PathBuf]) -> Option<UsageSummary> {
    let token = find_oauth_token(claude_paths)?;
    let mut config = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5)));
    // Honor NODE_EXTRA_CA_CERTS so the call works behind a TLS-intercepting proxy.
    if let Some(roots) = usage_root_certs() {
        config = config.tls_config(ureq::tls::TlsConfig::builder().root_certs(roots).build());
    }
    let agent: ureq::Agent = config.build().into();

    let response = agent
        .get(USAGE_ENDPOINT)
        .header("Authorization", &format!("Bearer {}", token))
        .header("Accept", "application/json")
        .header("anthropic-beta", ANTHROPIC_BETA)
        .call();

    let mut response = match response {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Usage API error: {}", e);
            return None;
        }
    };

    if response.status() != 200 {
        eprintln!("Usage API HTTP {}", response.status());
        return None;
    }

    let dto: UsageResponseDto = response.body_mut().read_json().ok()?;
    Some(usage_summary_from_dto(dto))
}

impl From<UsageLimitDto> for UsageLimit {
    fn from(value: UsageLimitDto) -> Self {
        let utilization = value
            .utilization
            .or_else(|| usage_percent_from_dollars(value.used, value.limit));
        let remaining = value
            .remaining
            .or_else(|| remaining_from_dollars(value.used, value.limit));
        UsageLimit {
            utilization,
            used: value.used,
            remaining,
            limit: value.limit,
            resets_at: value.resets_at.map(crate::usage::normalize_reset_time),
        }
    }
}

impl From<UsageApiLimitDto> for UsageApiLimit {
    fn from(value: UsageApiLimitDto) -> Self {
        UsageApiLimit {
            kind: value.kind,
            group: value.group,
            percent: value.percent,
            severity: value.severity,
            resets_at: value.resets_at.map(crate::usage::normalize_reset_time),
            scope: value.scope,
            is_active: value.is_active,
        }
    }
}

impl From<UsageMoneyDto> for UsageMoney {
    fn from(value: UsageMoneyDto) -> Self {
        let amount = match (value.amount_minor, value.exponent) {
            (Some(minor), Some(exponent)) => Some(minor as f64 / 10_f64.powi(exponent)),
            _ => None,
        };
        UsageMoney {
            amount_minor: value.amount_minor,
            exponent: value.exponent,
            currency: value.currency,
            amount,
        }
    }
}

impl From<UsageSpendDto> for UsageSpend {
    fn from(value: UsageSpendDto) -> Self {
        UsageSpend {
            used: value.used.map(UsageMoney::from),
            limit: value.limit.map(UsageMoney::from),
            percent: value.percent,
            severity: value.severity,
            enabled: value.enabled,
            disabled_reason: value.disabled_reason,
            can_purchase_credits: value.can_purchase_credits,
            can_toggle: value.can_toggle,
        }
    }
}

impl From<ExtraUsageDto> for ExtraUsage {
    fn from(value: ExtraUsageDto) -> Self {
        ExtraUsage {
            is_enabled: value.is_enabled,
            // API returns cents, convert to dollars.
            monthly_limit: value.monthly_limit.map(|v| v / 100.0),
            used_credits: value.used_credits.map(|v| v / 100.0),
            utilization: value.utilization,
            currency: value.currency,
            disabled_reason: value.disabled_reason,
        }
    }
}

fn usage_summary_from_dto(dto: UsageResponseDto) -> UsageSummary {
    let limits = dto
        .limits
        .into_iter()
        .map(UsageApiLimit::from)
        .collect::<Vec<_>>();
    let spend = dto.spend.map(UsageSpend::from);

    let mut window = dto.five_hour.map(UsageLimit::from).unwrap_or_default();
    fill_from_api_limit(&mut window, find_session_limit(&limits));

    let mut seven_day = dto.seven_day.map(UsageLimit::from).unwrap_or_default();
    fill_from_api_limit(&mut seven_day, find_weekly_all_limit(&limits));

    let mut seven_day_opus = dto.seven_day_opus.map(UsageLimit::from).unwrap_or_default();
    fill_from_api_limit(
        &mut seven_day_opus,
        find_scoped_weekly_limit(&limits, "opus"),
    );

    let mut seven_day_sonnet = dto
        .seven_day_sonnet
        .map(UsageLimit::from)
        .unwrap_or_default();
    fill_from_api_limit(
        &mut seven_day_sonnet,
        find_scoped_weekly_limit(&limits, "sonnet"),
    );

    let mut extra_usage = dto.extra_usage.map(ExtraUsage::from);
    if let Some(spend_extra) = spend.as_ref().and_then(extra_usage_from_spend) {
        match extra_usage.as_mut() {
            Some(extra) => fill_extra_usage_from_spend(extra, &spend_extra),
            None => extra_usage = Some(spend_extra),
        }
    }

    UsageSummary {
        window,
        seven_day,
        seven_day_opus,
        seven_day_sonnet,
        seven_day_oauth_apps: dto
            .seven_day_oauth_apps
            .map(UsageLimit::from)
            .unwrap_or_default(),
        seven_day_cowork: dto
            .seven_day_cowork
            .map(UsageLimit::from)
            .unwrap_or_default(),
        cinder_cove: dto.cinder_cove.map(UsageLimit::from).unwrap_or_default(),
        extra_usage,
        limits,
        spend,
        stale: false,
    }
}

fn usage_percent_from_dollars(used: Option<f64>, limit: Option<f64>) -> Option<f64> {
    match (used, limit) {
        (Some(used), Some(limit)) if limit > 0.0 => Some(used / limit * 100.0),
        _ => None,
    }
}

fn remaining_from_dollars(used: Option<f64>, limit: Option<f64>) -> Option<f64> {
    match (used, limit) {
        (Some(used), Some(limit)) => Some((limit - used).max(0.0)),
        _ => None,
    }
}

fn fill_from_api_limit(target: &mut UsageLimit, source: Option<&UsageApiLimit>) {
    let Some(source) = source else {
        return;
    };
    if target.utilization.is_none() {
        target.utilization = source.percent;
    }
    if target.resets_at.is_none() {
        target.resets_at = source.resets_at;
    }
}

fn find_session_limit(limits: &[UsageApiLimit]) -> Option<&UsageApiLimit> {
    limits.iter().find(|limit| {
        limit.kind.as_deref() == Some("session") || limit.group.as_deref() == Some("session")
    })
}

fn find_weekly_all_limit(limits: &[UsageApiLimit]) -> Option<&UsageApiLimit> {
    limits.iter().find(|limit| {
        limit.kind.as_deref() == Some("weekly_all")
            || (limit.group.as_deref() == Some("weekly") && limit.scope.is_none())
    })
}

fn find_scoped_weekly_limit<'a>(
    limits: &'a [UsageApiLimit],
    family: &str,
) -> Option<&'a UsageApiLimit> {
    limits.iter().find(|limit| {
        let is_scoped_weekly = limit.kind.as_deref() == Some("weekly_scoped")
            || (limit.group.as_deref() == Some("weekly") && limit.scope.is_some());
        is_scoped_weekly && limit_scope_contains(limit, family)
    })
}

fn limit_scope_contains(limit: &UsageApiLimit, needle: &str) -> bool {
    let needle = needle.to_ascii_lowercase();
    let Some(model) = limit.scope.as_ref().and_then(|scope| scope.model.as_ref()) else {
        return false;
    };

    model
        .id
        .as_deref()
        .is_some_and(|id| id.to_ascii_lowercase().contains(&needle))
        || model
            .display_name
            .as_deref()
            .is_some_and(|display| display.to_ascii_lowercase().contains(&needle))
}

fn extra_usage_from_spend(spend: &UsageSpend) -> Option<ExtraUsage> {
    let used = spend.used.as_ref();
    let limit = spend.limit.as_ref();
    let has_extra_usage_data = spend.enabled.is_some()
        || spend.percent.is_some()
        || spend.disabled_reason.is_some()
        || used.is_some()
        || limit.is_some();
    if !has_extra_usage_data {
        return None;
    }

    Some(ExtraUsage {
        is_enabled: spend.enabled.unwrap_or(false),
        monthly_limit: limit.and_then(|money| money.amount),
        used_credits: used.and_then(|money| money.amount),
        utilization: spend.percent,
        currency: used
            .and_then(|money| money.currency.clone())
            .or_else(|| limit.and_then(|money| money.currency.clone())),
        disabled_reason: spend.disabled_reason.clone(),
    })
}

fn fill_extra_usage_from_spend(target: &mut ExtraUsage, source: &ExtraUsage) {
    target.is_enabled = target.is_enabled || source.is_enabled;
    if target.monthly_limit.is_none() {
        target.monthly_limit = source.monthly_limit;
    }
    if target.used_credits.is_none() {
        target.used_credits = source.used_credits;
    }
    if target.utilization.is_none() {
        target.utilization = source.utilization;
    }
    if target.currency.is_none() {
        target.currency = source.currency.clone();
    }
    if target.disabled_reason.is_none() {
        target.disabled_reason = source.disabled_reason.clone();
    }
}

fn find_oauth_token(claude_paths: &[PathBuf]) -> Option<String> {
    // Check environment variables first
    for env in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"] {
        if let Ok(val) = std::env::var(env) {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    // macOS: Try Keychain first (credentials stored in Keychain, not file)
    #[cfg(target_os = "macos")]
    {
        if let Some(token) = read_from_macos_keychain() {
            return Some(token);
        }
    }

    // Search through all provided claude paths for .credentials.json (Linux/Windows)
    for base_path in claude_paths {
        let credentials_path = base_path.join(".credentials.json");
        if let Ok(raw) = fs::read_to_string(&credentials_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(access) = json
                    .get("claudeAiOauth")
                    .and_then(|v| v.get("accessToken"))
                    .and_then(|v| v.as_str())
                {
                    let trimmed = access.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn read_from_macos_keychain() -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::process::Command;

    // Get current username for account field
    let username = env::var("USER").ok()?;

    // Build service name: "Claude Code-credentials"
    // If CLAUDE_CONFIG_DIR is set, append 8-char SHA256 suffix
    let mut service_name = "Claude Code-credentials".to_string();

    if let Ok(config_dir) = env::var("CLAUDE_CONFIG_DIR") {
        let mut hasher = Sha256::new();
        hasher.update(config_dir.as_bytes());
        let hash = hasher.finalize();
        let mut suffix = String::with_capacity(8);
        for byte in hash.iter().take(4) {
            suffix.push_str(&format!("{:02x}", byte));
        }
        service_name.push('-');
        service_name.push_str(&suffix);
    }

    // Query macOS Keychain for the credentials JSON
    let output = Command::new("security")
        .args(&[
            "find-generic-password",
            "-a",
            &username, // Account name
            "-s",
            &service_name, // Service name
            "-w",          // Output password only
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    // Parse the JSON payload stored in Keychain
    let json_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if json_str.is_empty() {
        return None;
    }

    // The stored value is the full credentials JSON
    let json: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let access_token = json
        .get("claudeAiOauth")
        .and_then(|v| v.get("accessToken"))
        .and_then(|v| v.as_str())?
        .trim()
        .to_string();

    if access_token.is_empty() {
        None
    } else {
        Some(access_token)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Every env var ureq's `Proxy::try_from_env` inspects, cleared so each
    /// egress test starts from a known direct baseline regardless of any proxy
    /// vars the host shell exports.
    const PROXY_VARS: &[&str] = &[
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "NO_PROXY",
        "no_proxy",
    ];

    fn clear_proxy_env() {
        for var in PROXY_VARS {
            unsafe { env::remove_var(var) };
        }
    }

    #[test]
    #[serial]
    fn egress_is_direct_without_proxy_env() {
        clear_proxy_env();
        let egress = resolve_usage_egress();
        assert_eq!(egress.route, "direct");
        assert!(!egress.via_proxy);
        assert!(!egress.no_proxy_bypass);
        assert!(egress.proxy_origin.is_none());
    }

    #[test]
    #[serial]
    fn egress_reports_proxy_without_leaking_credentials() {
        clear_proxy_env();
        unsafe { env::set_var("HTTPS_PROXY", "http://user:s3cr3t@127.0.0.1:8080") };
        let egress = resolve_usage_egress();
        clear_proxy_env();

        assert!(egress.via_proxy);
        assert_eq!(egress.proxy_origin.as_deref(), Some("127.0.0.1:8080"));
        assert_eq!(egress.route, "proxy http://127.0.0.1:8080 (auth)");
        // Credentials must never appear in any reported field.
        assert!(!egress.route.contains("user"));
        assert!(!egress.route.contains("s3cr3t"));
    }

    #[test]
    #[serial]
    fn egress_marks_proxy_without_auth() {
        clear_proxy_env();
        unsafe { env::set_var("HTTPS_PROXY", "http://127.0.0.1:8080") };
        let egress = resolve_usage_egress();
        clear_proxy_env();

        assert!(egress.via_proxy);
        assert_eq!(egress.route, "proxy http://127.0.0.1:8080");
    }

    #[test]
    #[serial]
    fn egress_honors_no_proxy_bypass_for_usage_host() {
        clear_proxy_env();
        unsafe {
            env::set_var("HTTPS_PROXY", "http://127.0.0.1:8080");
            env::set_var("NO_PROXY", "api.anthropic.com");
        }
        let egress = resolve_usage_egress();
        clear_proxy_env();

        assert!(!egress.via_proxy);
        assert!(egress.no_proxy_bypass);
        assert_eq!(egress.route, "direct (NO_PROXY bypass)");
    }

    /// A throwaway self-signed CA (generic `example.com` subject) used to verify
    /// PEM parsing without depending on the host trust store.
    const TEST_CA_PEM: &[u8] = b"-----BEGIN CERTIFICATE-----
MIIDNzCCAh+gAwIBAgIUP1PQSL0D5eHPT0VFXLKgGCLiMRgwDQYJKoZIhvcNAQEL
BQAwKzEXMBUGA1UEAwwOY2EuZXhhbXBsZS5jb20xEDAOBgNVBAoMB0V4YW1wbGUw
HhcNMjYwNjExMjMxNzU4WhcNMzYwNjA4MjMxNzU4WjArMRcwFQYDVQQDDA5jYS5l
eGFtcGxlLmNvbTEQMA4GA1UECgwHRXhhbXBsZTCCASIwDQYJKoZIhvcNAQEBBQAD
ggEPADCCAQoCggEBALR302tX0VsRu3oA1+erX01HgCJLjbRtzBv9kWenCwiWfJN5
AGkcKc0iMJ/gzQ9TbAoLJf/pNtF6v+AtI3CSb0+TbwbvlTrBIpyN6KtWdEyrvgyD
HcE1fWvZA/b9lEnzEXd5NNcBjlkpnqqBM8HucR40hpfRj7n7tcPvaBLvMzcK87Lq
LBB9jzPswBn4LqjZ7ExFb6CbrrgL9ByMww8pE0CtL3b8OsK09dyHbgPcoiBmWl6n
KjYwNAciMwnDffcX+BvlGrQKOiUdvJtwFOgvXPVRux+7wrpOxok9rC2JkGm/9yDd
XR3U6p4X+lCR1RuABX+J1UXmJU3UObTAavkVrFkCAwEAAaNTMFEwHQYDVR0OBBYE
FFeqt9XZl/slVJ030KkOyr776PDUMB8GA1UdIwQYMBaAFFeqt9XZl/slVJ030KkO
yr776PDUMA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZIhvcNAQELBQADggEBAKJ8V5hl
BA0mC/B1f3MbKvZQbEtryd+a3np27N2IRJ+XfLMHOa1LvGg1qC4+JoVyfhEtNmsO
sgkHnIL2RvIW8/SYMSUaq1cY7mzw6JRNcZe12OTVYkOtqJ52wTZACqWlQ1xpwz0e
k5nj3y5iLftTuywFo817iOMyqpz7iq2XNdufSEVj/xo04si+s8Un9moyBEr5nCep
fdecU9SsQ5B3axIt8C4/jrJZT1NczYwQFdeBhO9P7v6l9z6OcrPaHjB/pXqi3KOb
5EwLfd2N18XYtvK6cgwtbTtKA/Y7Gpsx9DJgjXKEgxmf/bFy5MQ/t6W1O0GzjhFF
KYWlso+DPM561Zw=
-----END CERTIFICATE-----
";

    #[test]
    fn parse_ca_pem_extracts_certificates() {
        assert_eq!(parse_ca_pem(TEST_CA_PEM).len(), 1);
    }

    #[test]
    fn parse_ca_pem_ignores_non_certificate_input() {
        assert!(parse_ca_pem(b"not a pem file").is_empty());
    }

    #[test]
    #[serial]
    fn usage_root_certs_absent_without_extra_ca_env() {
        let prior = env::var(EXTRA_CA_ENV).ok();
        unsafe { env::remove_var(EXTRA_CA_ENV) };
        assert!(usage_root_certs().is_none());
        if let Some(value) = prior {
            unsafe { env::set_var(EXTRA_CA_ENV, value) };
        }
    }

    #[test]
    #[serial]
    fn egress_reports_extra_ca_path() {
        clear_proxy_env();
        let prior = env::var(EXTRA_CA_ENV).ok();

        unsafe { env::set_var(EXTRA_CA_ENV, "/etc/ssl/corp-ca.pem") };
        assert_eq!(
            resolve_usage_egress().extra_ca.as_deref(),
            Some("/etc/ssl/corp-ca.pem")
        );

        unsafe { env::remove_var(EXTRA_CA_ENV) };
        assert!(resolve_usage_egress().extra_ca.is_none());

        match prior {
            Some(value) => unsafe { env::set_var(EXTRA_CA_ENV, value) },
            None => unsafe { env::remove_var(EXTRA_CA_ENV) },
        }
    }

    #[test]
    fn usage_response_parses_raw_api_shape() {
        // Mirrors the /api/oauth/usage response shape with synthetic values,
        // including null codename fields the statusline does not model.
        let raw = r#"{
            "five_hour": {
                "utilization": 42.3,
                "resets_at": "2026-08-14T18:45:00.000000+00:00",
                "limit_dollars": 123.45,
                "used_dollars": 67.89,
                "remaining_dollars": 55.56
            },
            "seven_day": {"utilization": 73.2, "resets_at": "2026-08-18T07:00:00.000000+00:00"},
            "seven_day_oauth_apps": null,
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": 0.0, "resets_at": null},
            "seven_day_cowork": null,
            "seven_day_omelette": null,
            "tangelo": null,
            "iguana_necktie": null,
            "omelette_promotional": null,
            "cinder_cove": {"utilization": 8.6, "resets_at": "2026-09-01T00:00:00+00:00"},
            "extra_usage": {
                "is_enabled": true,
                "monthly_limit": 98765,
                "used_credits": 4321.0,
                "utilization": 4.4,
                "currency": "USD",
                "disabled_reason": null
            },
            "spend": {
                "used": {"amount_minor": 4321, "currency": "USD", "exponent": 2},
                "limit": {"amount_minor": 98765, "currency": "USD", "exponent": 2},
                "percent": 4.4,
                "severity": "normal",
                "enabled": true,
                "disabled_reason": null,
                "can_purchase_credits": false,
                "can_toggle": false
            },
            "limits": [
                {
                    "kind": "session",
                    "group": "session",
                    "percent": 42.3,
                    "severity": "normal",
                    "resets_at": "2026-08-14T18:45:00.000000+00:00",
                    "scope": null,
                    "is_active": false
                },
                {
                    "kind": "weekly_all",
                    "group": "weekly",
                    "percent": 73.2,
                    "severity": "normal",
                    "resets_at": "2026-08-18T07:00:00.000000+00:00",
                    "scope": null,
                    "is_active": true
                }
            ]
        }"#;

        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse raw usage response");
        let summary = usage_summary_from_dto(dto);
        assert_eq!(summary.window.utilization, Some(42.3));
        assert_eq!(summary.window.used, Some(67.89));
        assert_eq!(summary.window.remaining, Some(55.56));
        assert_eq!(summary.window.limit, Some(123.45));
        assert!(summary.seven_day_opus.utilization.is_none());

        let cinder = summary.cinder_cove;
        assert_eq!(cinder.utilization, Some(8.6));
        assert!(cinder.resets_at.is_some());

        let extra = summary.extra_usage.expect("extra_usage");
        assert_eq!(extra.currency.as_deref(), Some("USD"));
        assert_eq!(extra.disabled_reason, None);
        assert_eq!(extra.monthly_limit, Some(987.65));
        assert_eq!(summary.limits.len(), 2);
        assert_eq!(
            summary
                .spend
                .as_ref()
                .and_then(|spend| spend.limit.as_ref())
                .and_then(|money| money.amount),
            Some(987.65)
        );
    }

    #[test]
    fn usage_response_uses_limits_and_spend_as_fallbacks() {
        let raw = r#"{
            "five_hour": null,
            "seven_day": null,
            "seven_day_opus": null,
            "seven_day_sonnet": null,
            "extra_usage": null,
            "spend": {
                "used": {"amount_minor": 1234, "currency": "USD", "exponent": 2},
                "limit": {"amount_minor": 98765, "currency": "USD", "exponent": 2},
                "percent": 1.25,
                "severity": "normal",
                "enabled": true,
                "disabled_reason": null
            },
            "limits": [
                {
                    "kind": "session",
                    "group": "session",
                    "percent": 37,
                    "severity": "normal",
                    "resets_at": "2026-10-06T15:30:00.000000+00:00",
                    "scope": null,
                    "is_active": false
                },
                {
                    "kind": "weekly_all",
                    "group": "weekly",
                    "percent": 62,
                    "severity": "normal",
                    "resets_at": "2026-10-13T07:00:00.000000+00:00",
                    "scope": null,
                    "is_active": true
                },
                {
                    "kind": "weekly_scoped",
                    "group": "weekly",
                    "percent": 24,
                    "severity": "normal",
                    "resets_at": "2026-10-13T07:00:00.000000+00:00",
                    "scope": {
                        "model": {"id": null, "display_name": "Fable"},
                        "surface": null
                    },
                    "is_active": false
                }
            ]
        }"#;

        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse fallback response");
        let summary = usage_summary_from_dto(dto);

        assert_eq!(summary.window.utilization, Some(37.0));
        assert!(summary.window.resets_at.is_some());
        assert_eq!(summary.seven_day.utilization, Some(62.0));
        assert_eq!(
            find_scoped_weekly_limit(&summary.limits, "fable").and_then(|limit| limit.percent),
            Some(24.0)
        );
        assert!(find_scoped_weekly_limit(&summary.limits, "sonnet").is_none());
        assert_eq!(
            summary
                .limits
                .iter()
                .find(|limit| limit.kind.as_deref() == Some("weekly_scoped"))
                .and_then(|limit| limit.scope.as_ref())
                .and_then(|scope| scope.model.as_ref())
                .and_then(|model| model.display_name.as_deref()),
            Some("Fable")
        );
        assert_eq!(summary.seven_day_opus.utilization, None);
        assert_eq!(summary.seven_day_sonnet.utilization, None);

        assert_eq!(
            summary.spend.as_ref().and_then(|spend| spend.percent),
            Some(1.25)
        );
        let extra = summary.extra_usage.expect("spend-backed extra usage");
        assert!(extra.is_enabled);
        assert_eq!(extra.used_credits, Some(12.34));
        assert_eq!(extra.monthly_limit, Some(987.65));
        assert_eq!(extra.utilization, Some(1.25));
        assert_eq!(extra.currency.as_deref(), Some("USD"));
    }
}
