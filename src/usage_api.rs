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
    pub resets_at: Option<DateTime<Utc>>,
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
    seven_day_sonnet: Option<UsageLimitDto>,
    #[serde(default)]
    seven_day_oauth_apps: Option<UsageLimitDto>,
    #[serde(default)]
    seven_day_cowork: Option<UsageLimitDto>,
    #[serde(default)]
    cinder_cove: Option<UsageLimitDto>,
    #[serde(default)]
    extra_usage: Option<ExtraUsageDto>,
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
    Some(UsageSummary {
        window: dto.five_hour.map(UsageLimit::from).unwrap_or_default(),
        seven_day: dto.seven_day.map(UsageLimit::from).unwrap_or_default(),
        seven_day_opus: dto.seven_day_opus.map(UsageLimit::from).unwrap_or_default(),
        seven_day_sonnet: dto
            .seven_day_sonnet
            .map(UsageLimit::from)
            .unwrap_or_default(),
        seven_day_oauth_apps: dto
            .seven_day_oauth_apps
            .map(UsageLimit::from)
            .unwrap_or_default(),
        seven_day_cowork: dto
            .seven_day_cowork
            .map(UsageLimit::from)
            .unwrap_or_default(),
        cinder_cove: dto.cinder_cove.map(UsageLimit::from).unwrap_or_default(),
        extra_usage: dto.extra_usage.map(|e| ExtraUsage {
            is_enabled: e.is_enabled,
            // API returns cents, convert to dollars
            monthly_limit: e.monthly_limit.map(|v| v / 100.0),
            used_credits: e.used_credits.map(|v| v / 100.0),
            utilization: e.utilization,
            currency: e.currency,
            disabled_reason: e.disabled_reason,
        }),
        stale: false,
    })
}

impl From<UsageLimitDto> for UsageLimit {
    fn from(value: UsageLimitDto) -> Self {
        UsageLimit {
            utilization: value.utilization,
            used: value.used,
            remaining: value.remaining,
            resets_at: value.resets_at.map(crate::usage::normalize_reset_time),
        }
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
        // Mirrors the live /api/oauth/usage body, including null codename
        // fields the statusline does not model.
        let raw = r#"{
            "five_hour": {"utilization": 20.0, "resets_at": "2026-06-09T21:20:01.037017+00:00"},
            "seven_day": {"utilization": 5.0, "resets_at": "2026-06-16T07:00:01.037038+00:00"},
            "seven_day_oauth_apps": null,
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": 0.0, "resets_at": null},
            "seven_day_cowork": null,
            "seven_day_omelette": null,
            "tangelo": null,
            "iguana_necktie": null,
            "omelette_promotional": null,
            "cinder_cove": {"utilization": 12.0, "resets_at": "2026-07-01T00:00:00+00:00"},
            "extra_usage": {
                "is_enabled": true,
                "monthly_limit": 30000,
                "used_credits": 1200.0,
                "utilization": 4.0,
                "currency": "USD",
                "disabled_reason": null
            }
        }"#;

        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse raw usage response");
        assert_eq!(
            dto.five_hour.as_ref().and_then(|l| l.utilization),
            Some(20.0)
        );
        assert!(dto.seven_day_opus.is_none());

        let cinder = UsageLimit::from(dto.cinder_cove.expect("cinder_cove"));
        assert_eq!(cinder.utilization, Some(12.0));
        assert!(cinder.resets_at.is_some());

        let extra = dto.extra_usage.expect("extra_usage");
        assert_eq!(extra.currency.as_deref(), Some("USD"));
        assert_eq!(extra.disabled_reason, None);
    }
}
