use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const ANTHROPIC_API_HOST: &str = "api.anthropic.com";
/// The endpoint allows a small burst per account inside a fixed window and then
/// answers 429 with `retry-after` counting down to the window's reset. Measured
/// budget is 5 requests per 300s, so the default keeps one machine at a single
/// request per window and leaves room for other machines on the same account.
const CACHE_TTL_SECONDS: i64 = 300;
/// Floor for an operator-supplied TTL. Below this, a handful of machines sharing
/// one account would exhaust the per-window budget and sit in 429 permanently.
const MIN_CACHE_TTL_SECONDS: i64 = 60;
const CACHE_TTL_ENV: &str = "CLAUDE_USAGE_CACHE_TTL_SECONDS";
/// Backstop when a rejected fetch carries no usable `retry-after`.
const NEGATIVE_CACHE_TTL_SECONDS: i64 = 120;
/// Ceiling on a server-supplied `retry-after` so one absurd value cannot wedge
/// the statusline on stale numbers for hours.
const MAX_RETRY_AFTER_SECONDS: i64 = 900;
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

/// Whether the subscription usage figures apply to this environment.
///
/// Deliberately model-independent, unlike [`is_direct_claude_api`]. The
/// five-hour, weekly, scoped, and extra-usage rows describe the account's
/// subscription rather than the turn in flight, so a mixed-model launcher that
/// routes one turn to a third-party model is still spending the same
/// subscription and must not blank the whole cluster until the next Claude
/// reply lands. Only a non-Anthropic `ANTHROPIC_BASE_URL` means the numbers
/// genuinely do not describe this session.
pub fn subscription_usage_applies() -> bool {
    is_direct_claude_api(None)
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
    /// Effective positive-cache lifetime, after any env override and floor.
    pub cache_ttl_seconds: i64,
    /// Response fields the cached summary could not attribute to a known field.
    /// Non-empty means the endpoint grew something worth mapping.
    pub unknown_response_fields: Vec<String>,
    pub egress: UsageEgress,
}

pub fn inspect_usage_api(claude_paths: &[PathBuf]) -> UsageApiHealth {
    UsageApiHealth {
        direct_claude_api: subscription_usage_applies(),
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
        cache_ttl_seconds: cache_ttl_seconds(),
        unknown_response_fields: crate::db::get_stale_api_cache(API_CACHE_KEY)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<UsageSummary>(&json).ok())
            .map(|summary| summary.unknown_fields)
            .unwrap_or_default(),
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

    /// Blank a rolling window whose reset has already passed. The percentage
    /// belongs to a window that ended, and how much of the new one is spent is
    /// unknown until a fetch lands, so neither the old number nor a fabricated
    /// zero can be shown. `resets_at` goes with it: a reset in the past makes
    /// the countdown freeze at `0m` instead of rolling forward from the window
    /// anchor.
    pub fn clear_if_reset_elapsed(&mut self, now: DateTime<Utc>) {
        if self.resets_at.is_some_and(|reset| reset <= now) {
            *self = Self::default();
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtraUsage {
    pub is_enabled: bool,
    pub monthly_limit: Option<f64>,
    pub used_credits: Option<f64>,
    pub utilization: Option<f64>,
    pub currency: Option<String>,
    pub disabled_reason: Option<String>,
    /// Minor-unit exponent for `monthly_limit` / `used_credits`. The API states
    /// it explicitly; only fall back to 2 when the field is absent.
    pub decimal_places: Option<i32>,
    /// Set when the user turned extra usage off themselves, as opposed to an
    /// org policy or an exhausted balance. Distinguishes the two causes that
    /// `disabled_reason` alone conflates.
    pub user_disabled: Option<bool>,
    pub spend_limit_reached: Option<bool>,
    pub credits_ever_enabled: Option<bool>,
    /// Sub-window credit rows. Shape is unverified because the live endpoint has
    /// only ever returned null for these, so they are preserved verbatim rather
    /// than typed against a guess.
    pub daily: Option<serde_json::Value>,
    pub weekly: Option<serde_json::Value>,
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
    /// Purchasable ceiling, split by funding kind. Distinct from `limit`, which
    /// is the currently provisioned spend cap.
    pub cap: Option<UsageSpendCap>,
    pub disclaimer: Option<String>,
    /// Shapes unverified: the live endpoint has only ever returned null for
    /// these, so they are preserved verbatim rather than typed against a guess.
    pub balance: Option<serde_json::Value>,
    pub auto_reload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageSpendCap {
    pub money: Option<UsageMoney>,
    pub credits: Option<UsageMoney>,
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
    /// Non-null codename limit slots, keyed by their wire name. Empty in every
    /// response observed so far; populated rather than dropped so a slot that
    /// activates shows up in `--json` without another schema change.
    pub codename_limits: BTreeMap<String, UsageLimit>,
    pub member_dashboard_available: Option<bool>,
    /// Names of response fields no DTO field claimed. Surfaced by `doctor` and
    /// `--debug` as an early warning that the endpoint grew a new field.
    pub unknown_fields: Vec<String>,
    /// When this response was read from the API. Stamped before caching, so it
    /// round-trips through the cached JSON and describes the underlying fetch
    /// rather than the moment some later process read the cache.
    pub fetched_at: Option<DateTime<Utc>>,
    /// True when serving expired cached data after an API failure
    pub stale: bool,
}

impl UsageSummary {
    /// True when the five-hour window's reset time is at or before `now`, meaning
    /// the cached utilization describes a window that has already rolled over and
    /// a refetch is needed to surface the fresh percentage and next reset time.
    pub fn window_reset_elapsed(&self, now: DateTime<Utc>) -> bool {
        self.window.resets_at.is_some_and(|reset| reset <= now)
    }

    /// Every rolling window's reset time. Covers the five-hour window, all the
    /// seven-day windows, and the scoped weekly rows. Deliberately excludes
    /// `cinder_cove` and the non-rolling codename slots: their `resets_at` is a
    /// one-time credit expiry, so treating it as a window boundary would refetch
    /// on every call once the credit lapsed.
    fn rolling_resets(&self) -> impl Iterator<Item = DateTime<Utc>> + '_ {
        [
            self.window.resets_at,
            self.seven_day.resets_at,
            self.seven_day_opus.resets_at,
            self.seven_day_sonnet.resets_at,
            self.seven_day_oauth_apps.resets_at,
            self.seven_day_cowork.resets_at,
        ]
        .into_iter()
        .chain(self.limits.iter().map(|limit| limit.resets_at))
        .chain(ROLLING_CODENAME_LIMIT_KEYS.iter().map(|key| {
            self.codename_limits
                .get(*key)
                .and_then(|limit| limit.resets_at)
        }))
        .flatten()
    }

    /// True when any rolling usage window's reset has passed, so some percentage
    /// on this summary describes an expired window.
    pub fn any_window_reset_elapsed(&self, now: DateTime<Utc>) -> bool {
        self.rolling_resets().any(|reset| reset <= now)
    }

    /// True when a rolling window rolled over *after* this response was read,
    /// which is the case where a refetch has something new to say and the cache
    /// should be bypassed.
    ///
    /// A reset the fetch itself already saw is the endpoint's latest word on
    /// that window. Bypassing for it would refetch on every invocation and spend
    /// the account's per-window request budget on an answer that cannot change,
    /// ending in a 429 that pins the display on the very numbers the bypass was
    /// meant to replace. A response cached before `fetched_at` existed carries no
    /// fetch time, so it falls back to the plain elapsed check.
    pub fn any_window_reset_since_fetch(&self, now: DateTime<Utc>) -> bool {
        match self.fetched_at {
            Some(fetched) => self
                .rolling_resets()
                .any(|reset| reset <= now && reset > fetched),
            None => self.any_window_reset_elapsed(now),
        }
    }

    /// Drop the percentages that would otherwise render from a window that has
    /// already rolled over.
    ///
    /// The refetch that `any_window_reset_since_fetch` triggers cannot always
    /// land: a sibling session may hold the fetch lock, a 429 may have armed the
    /// backoff, or the token may have expired entirely. In each of those the
    /// last response is served instead, and these rows are the only copy of
    /// `fable:`/`opus:`/`sonnet:` and friends, so they render on with a figure
    /// from a week that ended.
    ///
    /// Scope is deliberately the rows the OAuth response alone supplies.
    /// `window` and `seven_day` are excluded because the hook carries them too
    /// and `main.rs` already judges their staleness: it withholds an expired
    /// hook snapshot, and where no live value replaces it, keeps the last number
    /// and sets `stale` so the label renders `~usage:`. Clearing them here would
    /// delete that marked figure outright on the API-primary path, since the
    /// restore is gated on hook `rate_limits` that path does not have.
    ///
    /// Only the values go. `resets_at` stays on the `limits[]` rows so `--json`
    /// still reports which window ended and when.
    pub fn clear_elapsed_windows(&mut self, now: DateTime<Utc>) {
        self.seven_day_opus.clear_if_reset_elapsed(now);
        self.seven_day_sonnet.clear_if_reset_elapsed(now);
        self.seven_day_oauth_apps.clear_if_reset_elapsed(now);
        self.seven_day_cowork.clear_if_reset_elapsed(now);
        // A row with no `resets_at` carries no evidence its window ended, so it
        // is left alone rather than guessed at.
        for limit in &mut self.limits {
            if limit.resets_at.is_some_and(|reset| reset <= now) {
                limit.percent = None;
            }
        }
        for key in ROLLING_CODENAME_LIMIT_KEYS {
            if let Some(limit) = self.codename_limits.get_mut(key) {
                limit.clear_if_reset_elapsed(now);
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExtraUsageDto {
    is_enabled: bool,
    monthly_limit: Option<f64>,
    used_credits: Option<f64>,
    utilization: Option<f64>,
    currency: Option<String>,
    disabled_reason: Option<String>,
    decimal_places: Option<i32>,
    user_disabled: Option<bool>,
    spend_limit_reached: Option<bool>,
    credits_ever_enabled: Option<bool>,
    daily: Option<serde_json::Value>,
    weekly: Option<serde_json::Value>,
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

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UsageSpendDto {
    used: Option<UsageMoneyDto>,
    limit: Option<UsageMoneyDto>,
    percent: Option<f64>,
    severity: Option<String>,
    enabled: Option<bool>,
    disabled_reason: Option<String>,
    can_purchase_credits: Option<bool>,
    can_toggle: Option<bool>,
    cap: Option<UsageSpendCapDto>,
    disclaimer: Option<String>,
    balance: Option<serde_json::Value>,
    auto_reload: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UsageSpendCapDto {
    money: Option<UsageMoneyDto>,
    credits: Option<UsageMoneyDto>,
}

/// Codename limit slots the API ships alongside the documented windows. None of
/// them is read by any Claude Code build, and all have been null in every
/// observed response, but they are named limit rows so they are captured rather
/// than discarded. Order here is the order they appear on the wire.
const CODENAME_LIMIT_KEYS: [&str; 6] = [
    "seven_day_omelette",
    "tangelo",
    "iguana_necktie",
    "omelette_promotional",
    "nimbus_quill",
    "amber_ladder",
];

/// Codename slots that are rolling windows rather than one-time grants, and so
/// must invalidate a cached response once their reset passes. `seven_day_*` is
/// a weekly window by construction; the rest have unknown semantics and are
/// deliberately excluded, because treating a one-time expiry as a rolling reset
/// causes a refetch on every invocation once that date passes.
const ROLLING_CODENAME_LIMIT_KEYS: [&str; 1] = ["seven_day_omelette"];

/// Deserialize one section, degrading to `None` when its shape no longer
/// matches instead of failing the whole response.
///
/// The endpoint is undocumented and has already renamed and restructured fields
/// in place (`used` -> `used_dollars`, extra usage duplicated into `spend`). A
/// strict struct turns any one such change into a total blackout: no session
/// percentage, no weekly percentage, nothing. Isolating per section means a
/// reshaped `spend` costs `spend` alone and the numbers on the line survive.
fn lenient_section<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// Same isolation for the `limits` array, but per row: one malformed entry is
/// dropped rather than discarding every limit row alongside it.
fn lenient_rows<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let serde_json::Value::Array(rows) = value else {
        return Ok(Vec::new());
    };
    Ok(rows
        .into_iter()
        .filter_map(|row| serde_json::from_value(row).ok())
        .collect())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UsageResponseDto {
    #[serde(deserialize_with = "lenient_section")]
    five_hour: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    seven_day: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    seven_day_opus: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    seven_day_sonnet: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    seven_day_oauth_apps: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    seven_day_cowork: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    cinder_cove: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    seven_day_omelette: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    tangelo: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    iguana_necktie: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    omelette_promotional: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    nimbus_quill: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    amber_ladder: Option<UsageLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    member_dashboard_available: Option<bool>,
    #[serde(deserialize_with = "lenient_section")]
    extra_usage: Option<ExtraUsageDto>,
    #[serde(deserialize_with = "lenient_rows")]
    limits: Vec<UsageApiLimitDto>,
    #[serde(deserialize_with = "lenient_section")]
    spend: Option<UsageSpendDto>,
    /// Anything the endpoint adds that none of the above claims. Kept so a new
    /// field surfaces in `doctor`/`--debug` instead of vanishing silently.
    #[serde(flatten)]
    unknown: BTreeMap<String, serde_json::Value>,
}

pub fn get_usage_summary(claude_paths: &[PathBuf]) -> Option<UsageSummary> {
    // Subsystem-level disable now lives at main.rs (subsystems.usage_api). We
    // keep the base-URL guard here because it depends on env details that the
    // gate caller doesn't know.
    if !subscription_usage_applies() {
        return None;
    }

    let now = Utc::now();

    // Try to get from persistent SQLite cache first. A cached response whose
    // five-hour, weekly, or scoped window reset after it was read is serving
    // percentages from a window that has since rolled over, so fall through to a
    // refetch that picks up the fresh percentages and next reset times.
    if let Ok(Some(cached_json)) = crate::db::get_api_cache(API_CACHE_KEY) {
        if let Ok(summary) = serde_json::from_str::<UsageSummary>(&cached_json) {
            if !summary.any_window_reset_since_fetch(now) {
                return Some(without_elapsed_windows(summary, now));
            }
        }
    }

    // If API recently failed (429/error), don't retry -- serve stale data
    if let Ok(Some(_)) = crate::db::get_api_cache(NEGATIVE_CACHE_KEY) {
        return stale_fallback(now);
    }

    // Acquire fetch lock to prevent concurrent API calls across sessions.
    // Only the first process wins; others get stale data instead of racing.
    let got_lock = crate::db::try_set_api_cache(NEGATIVE_CACHE_KEY, "f", FETCH_LOCK_TTL_SECONDS)
        .unwrap_or(false);
    if !got_lock {
        return stale_fallback(now);
    }

    // Cache miss or invalid - fetch from API
    match fetch_usage_summary(claude_paths) {
        Ok(mut summary) => {
            // Stamped here, and with the instant the request went out, so a
            // reset landing mid-flight still counts as new to
            // `any_window_reset_since_fetch` instead of already-seen.
            summary.fetched_at = Some(now);
            // Cache before clearing: a later read compares those reset times
            // against `fetched_at` to decide whether a window rolled over since.
            if let Ok(json) = serde_json::to_string(&summary) {
                let _ = crate::db::set_api_cache(API_CACHE_KEY, &json, cache_ttl_seconds());
            }
            let _ = crate::db::set_api_cache(NEGATIVE_CACHE_KEY, "", 0);
            // Clear against the current instant, not the pre-fetch one, so a
            // window that rolled over during the call does not slip through.
            Some(without_elapsed_windows(summary, Utc::now()))
        }
        Err(failure) => {
            // Upgrade fetch lock to full negative cache to prevent retry storm.
            // A 429 states exactly how long the account's window has left, and
            // retrying before then just burns another request off the budget, so
            // the server's number wins over the local default whenever present.
            let backoff = failure
                .retry_after_seconds
                .map(|seconds| seconds.clamp(1, MAX_RETRY_AFTER_SECONDS))
                .unwrap_or(NEGATIVE_CACHE_TTL_SECONDS);
            let _ = crate::db::set_api_cache(NEGATIVE_CACHE_KEY, "1", backoff);
            stale_fallback(now)
        }
    }
}

/// Positive-cache lifetime. Defaults to one request per rate-limit window and
/// can be shortened for a single-machine setup, but never below the floor that
/// keeps several machines on one account inside the shared budget.
pub fn cache_ttl_seconds() -> i64 {
    env::var(CACHE_TTL_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .map(|seconds| seconds.max(MIN_CACHE_TTL_SECONDS))
        .unwrap_or(CACHE_TTL_SECONDS)
}

/// Why a fetch did not produce a summary, and how long to wait before retrying.
struct FetchFailure {
    retry_after_seconds: Option<i64>,
}

/// Parse a `retry-after` header. Only the delta-seconds form is handled; the
/// HTTP-date form is not emitted by this endpoint and a wrong parse would be
/// worse than falling back to the local default.
fn parse_retry_after(raw: &str) -> Option<i64> {
    raw.trim().parse::<i64>().ok().filter(|value| *value > 0)
}

/// Return the last cached API data (even if expired), marked as stale
fn stale_fallback(now: DateTime<Utc>) -> Option<UsageSummary> {
    if let Ok(Some(json)) = crate::db::get_stale_api_cache(API_CACHE_KEY) {
        if let Ok(mut summary) = serde_json::from_str::<UsageSummary>(&json) {
            summary.stale = true;
            return Some(without_elapsed_windows(summary, now));
        }
    }
    None
}

/// Last gate before a summary reaches a display consumer: no percentage from a
/// window that has already rolled over leaves this module. The stale path needs
/// it, since a cached row is served with no age bound whenever the refetch
/// cannot happen; every path gets it so a fresh response that reports a window
/// as already reset is covered by the same rule.
fn without_elapsed_windows(mut summary: UsageSummary, now: DateTime<Utc>) -> UsageSummary {
    summary.clear_elapsed_windows(now);
    summary
}

fn fetch_usage_summary(claude_paths: &[PathBuf]) -> Result<UsageSummary, FetchFailure> {
    let no_retry_after = || FetchFailure {
        retry_after_seconds: None,
    };
    let Some(token) = find_oauth_token(claude_paths) else {
        return Err(no_retry_after());
    };
    // Deliver non-2xx as a normal response instead of an error, so a 429 can be
    // read for its `retry-after` header rather than collapsing into an opaque
    // transport error that loses the one value worth having.
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false);
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
            return Err(no_retry_after());
        }
    };

    if response.status() != 200 {
        let retry_after_seconds = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after);
        match retry_after_seconds {
            Some(seconds) => eprintln!(
                "Usage API HTTP {} (retry-after {}s)",
                response.status(),
                seconds
            ),
            None => eprintln!("Usage API HTTP {}", response.status()),
        }
        return Err(FetchFailure {
            retry_after_seconds,
        });
    }

    match response.body_mut().read_json::<UsageResponseDto>() {
        Ok(dto) => Ok(usage_summary_from_dto(dto)),
        Err(e) => {
            eprintln!("Usage API parse error: {}", e);
            Err(no_retry_after())
        }
    }
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

impl From<UsageSpendCapDto> for UsageSpendCap {
    fn from(value: UsageSpendCapDto) -> Self {
        UsageSpendCap {
            money: value.money.map(UsageMoney::from),
            credits: value.credits.map(UsageMoney::from),
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
            cap: value.cap.map(UsageSpendCap::from),
            disclaimer: value.disclaimer,
            balance: value.balance,
            auto_reload: value.auto_reload,
        }
    }
}

/// Minor units per major unit for a currency exponent, e.g. 2 -> 100.0. Falls
/// back to the near-universal 2 when the API omits the exponent, and rejects
/// nonsensical values rather than scaling a balance into gibberish.
fn minor_unit_divisor(exponent: Option<i32>) -> f64 {
    match exponent {
        Some(exponent) if (0..=6).contains(&exponent) => 10_f64.powi(exponent),
        _ => 100.0,
    }
}

impl From<ExtraUsageDto> for ExtraUsage {
    fn from(value: ExtraUsageDto) -> Self {
        // Amounts arrive in minor units; `decimal_places` names the exponent.
        let divisor = minor_unit_divisor(value.decimal_places);
        ExtraUsage {
            is_enabled: value.is_enabled,
            monthly_limit: value.monthly_limit.map(|v| v / divisor),
            used_credits: value.used_credits.map(|v| v / divisor),
            utilization: value.utilization,
            currency: value.currency,
            disabled_reason: value.disabled_reason,
            decimal_places: value.decimal_places,
            user_disabled: value.user_disabled,
            spend_limit_reached: value.spend_limit_reached,
            credits_ever_enabled: value.credits_ever_enabled,
            daily: value.daily,
            weekly: value.weekly,
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

    // Keep only the codename slots the response actually populated, so the map
    // stays empty on the common path instead of carrying six null rows.
    let codename_dtos = [
        dto.seven_day_omelette,
        dto.tangelo,
        dto.iguana_necktie,
        dto.omelette_promotional,
        dto.nimbus_quill,
        dto.amber_ladder,
    ];
    let codename_limits = CODENAME_LIMIT_KEYS
        .iter()
        .zip(codename_dtos)
        .filter_map(|(key, limit)| limit.map(|limit| ((*key).to_string(), UsageLimit::from(limit))))
        .collect::<BTreeMap<_, _>>();

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
        codename_limits,
        member_dashboard_available: dto.member_dashboard_available,
        unknown_fields: dto.unknown.into_keys().collect(),
        // Set by the caller once the response is about to be cached.
        fetched_at: None,
        stale: false,
    }
}

impl UsageSummary {
    /// How long ago this response was read from the API.
    ///
    /// `None` when the summary predates age stamping, so a cache entry written
    /// by an older build reports no age rather than a fabricated one.
    pub fn age_seconds(&self, now: DateTime<Utc>) -> Option<i64> {
        self.fetched_at
            .map(|fetched| (now - fetched).num_seconds().max(0))
    }

    /// True once the response has outlived the refresh window it was cached
    /// for, meaning a refresh was due and did not land.
    pub fn is_past_refresh_window(&self, now: DateTime<Utc>) -> bool {
        self.age_seconds(now)
            .is_some_and(|age| age > cache_ttl_seconds())
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
        // `UsageMoney::amount` is already scaled out of minor units.
        monthly_limit: limit.and_then(|money| money.amount),
        used_credits: used.and_then(|money| money.amount),
        utilization: spend.percent,
        currency: used
            .and_then(|money| money.currency.clone())
            .or_else(|| limit.and_then(|money| money.currency.clone())),
        disabled_reason: spend.disabled_reason.clone(),
        // `spend` carries no equivalent of the extra_usage-only flags.
        ..ExtraUsage::default()
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
    // Accept the RFC 3339 strings the endpoint sends today and numeric epoch
    // seconds. Claude Code's own hook already reports resets as epoch floats, so
    // that form is a plausible drift rather than a hypothetical one, and a reset
    // time is worth keeping across the change.
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(raw)) => DateTime::parse_from_rfc3339(&raw)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(serde::de::Error::custom),
        Some(serde_json::Value::Number(raw)) => {
            let seconds = raw
                .as_f64()
                .ok_or_else(|| serde::de::Error::custom("reset timestamp is not a number"))?;
            let nanos = (seconds.fract() * 1e9).round().clamp(0.0, 999_999_999.0) as u32;
            DateTime::from_timestamp(seconds.trunc() as i64, nanos)
                .map(Some)
                .ok_or_else(|| serde::de::Error::custom("reset timestamp out of range"))
        }
        Some(other) => Err(serde::de::Error::custom(format!(
            "unexpected reset timestamp shape: {other}"
        ))),
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

    fn summary_with_window_reset(reset: Option<DateTime<Utc>>) -> UsageSummary {
        UsageSummary {
            window: UsageLimit {
                resets_at: reset,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn window_reset_elapsed_true_when_reset_in_past() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let summary = summary_with_window_reset(Some(now - chrono::TimeDelta::minutes(1)));
        assert!(summary.window_reset_elapsed(now));
    }

    #[test]
    fn window_reset_elapsed_true_at_exact_boundary() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let summary = summary_with_window_reset(Some(now));
        assert!(summary.window_reset_elapsed(now));
    }

    #[test]
    fn window_reset_elapsed_false_when_reset_in_future() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let summary = summary_with_window_reset(Some(now + chrono::TimeDelta::minutes(1)));
        assert!(!summary.window_reset_elapsed(now));
    }

    #[test]
    fn window_reset_elapsed_false_when_reset_absent() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let summary = summary_with_window_reset(None);
        assert!(!summary.window_reset_elapsed(now));
    }

    #[test]
    fn any_window_reset_elapsed_triggers_on_seven_day_while_five_hour_is_fresh() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = summary_with_window_reset(Some(now + chrono::TimeDelta::hours(1)));
        summary.seven_day.resets_at = Some(now - chrono::TimeDelta::minutes(1));
        assert!(summary.any_window_reset_elapsed(now));
        // The five-hour window alone is still fresh.
        assert!(!summary.window_reset_elapsed(now));
    }

    #[test]
    fn any_window_reset_elapsed_triggers_on_scoped_limit() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = UsageSummary::default();
        summary.limits.push(UsageApiLimit {
            resets_at: Some(now - chrono::TimeDelta::seconds(30)),
            ..Default::default()
        });
        assert!(summary.any_window_reset_elapsed(now));
    }

    #[test]
    fn any_window_reset_elapsed_false_when_all_future_or_absent() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = summary_with_window_reset(Some(now + chrono::TimeDelta::hours(1)));
        summary.seven_day.resets_at = Some(now + chrono::TimeDelta::hours(48));
        summary.limits.push(UsageApiLimit {
            resets_at: Some(now + chrono::TimeDelta::hours(24)),
            ..Default::default()
        });
        assert!(!summary.any_window_reset_elapsed(now));
    }

    #[test]
    fn any_window_reset_elapsed_ignores_cinder_cove_expiry() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = UsageSummary::default();
        // A lapsed one-time promo credit must not force perpetual refetching.
        summary.cinder_cove.resets_at = Some(now - chrono::TimeDelta::hours(72));
        assert!(!summary.any_window_reset_elapsed(now));
    }

    /// A scoped weekly row that carries a percentage from a window that already
    /// ended. This is what a session holding no fetch lock, or sitting in a 429
    /// backoff, is handed when it asks for the usage summary.
    fn summary_with_elapsed_fable(now: DateTime<Utc>) -> UsageSummary {
        UsageSummary {
            limits: vec![UsageApiLimit {
                kind: Some("weekly_scoped".to_string()),
                group: Some("weekly".to_string()),
                percent: Some(24.0),
                resets_at: Some(now - chrono::TimeDelta::minutes(5)),
                scope: Some(UsageLimitScope {
                    model: Some(UsageLimitScopeModel {
                        id: None,
                        display_name: Some("Fable".to_string()),
                    }),
                    surface: None,
                }),
                ..UsageApiLimit::default()
            }],
            ..UsageSummary::default()
        }
    }

    #[test]
    fn clear_elapsed_windows_drops_a_scoped_percentage_from_a_rolled_over_week() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = summary_with_elapsed_fable(now);
        summary.clear_elapsed_windows(now);

        assert_eq!(summary.limits[0].percent, None);
        // The row itself stays so `--json` still reports which window ended.
        assert_eq!(
            summary.limits[0].resets_at,
            Some(now - chrono::TimeDelta::minutes(5))
        );
        assert_eq!(
            summary.limits[0]
                .scope
                .as_ref()
                .and_then(|scope| scope.model.as_ref())
                .and_then(|model| model.display_name.as_deref()),
            Some("Fable")
        );
    }

    #[test]
    fn clear_elapsed_windows_keeps_a_scoped_percentage_whose_week_is_live() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = summary_with_elapsed_fable(now);
        summary.limits[0].resets_at = Some(now + chrono::TimeDelta::hours(1));
        summary.clear_elapsed_windows(now);

        assert_eq!(summary.limits[0].percent, Some(24.0));
    }

    #[test]
    fn clear_elapsed_windows_blanks_every_rolling_window_it_covers() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let past = Some(now - chrono::TimeDelta::minutes(1));
        let mut summary = UsageSummary {
            seven_day_opus: UsageLimit {
                utilization: Some(31.0),
                resets_at: past,
                ..UsageLimit::default()
            },
            seven_day_cowork: UsageLimit {
                utilization: Some(18.0),
                resets_at: past,
                ..UsageLimit::default()
            },
            ..UsageSummary::default()
        };
        summary.codename_limits.insert(
            "seven_day_omelette".to_string(),
            UsageLimit {
                utilization: Some(12.0),
                resets_at: past,
                ..UsageLimit::default()
            },
        );
        summary.clear_elapsed_windows(now);

        assert_eq!(summary.seven_day_opus.utilization, None);
        // The reset goes with the value so the countdown rolls forward from the
        // window anchor instead of freezing at "0m" against a passed reset.
        assert_eq!(summary.seven_day_opus.resets_at, None);
        assert_eq!(summary.seven_day_cowork.utilization, None);
        assert_eq!(
            summary.codename_limits["seven_day_omelette"].utilization,
            None
        );
    }

    /// `main.rs` owns the staleness of the two windows the hook also carries: it
    /// keeps the last number and marks it `~stale` rather than dropping it, and
    /// the restore is gated on hook data the API-primary path does not have.
    #[test]
    fn clear_elapsed_windows_leaves_the_hook_owned_windows_to_main() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let past = Some(now - chrono::TimeDelta::minutes(1));
        let mut summary = UsageSummary {
            window: UsageLimit {
                utilization: Some(88.0),
                resets_at: past,
                ..UsageLimit::default()
            },
            seven_day: UsageLimit {
                utilization: Some(55.0),
                resets_at: past,
                ..UsageLimit::default()
            },
            ..UsageSummary::default()
        };
        summary.clear_elapsed_windows(now);

        assert_eq!(summary.window.utilization, Some(88.0));
        assert_eq!(summary.seven_day.utilization, Some(55.0));
    }

    /// A row with no reset time carries no evidence its window ended, so it is
    /// left alone. The scoped rows the endpoint actually sends do carry one.
    #[test]
    fn clear_elapsed_windows_cannot_judge_a_row_without_a_reset() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = summary_with_elapsed_fable(now);
        summary.limits[0].resets_at = None;
        summary.clear_elapsed_windows(now);

        assert_eq!(summary.limits[0].percent, Some(24.0));
    }

    #[test]
    fn clear_elapsed_windows_leaves_one_time_credits_alone() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = UsageSummary {
            cinder_cove: UsageLimit {
                utilization: Some(40.0),
                resets_at: Some(now - chrono::TimeDelta::hours(72)),
                ..UsageLimit::default()
            },
            extra_usage: Some(ExtraUsage {
                is_enabled: true,
                used_credits: Some(12.34),
                ..ExtraUsage::default()
            }),
            ..UsageSummary::default()
        };
        summary.codename_limits.insert(
            "tangelo".to_string(),
            UsageLimit {
                utilization: Some(9.0),
                resets_at: Some(now - chrono::TimeDelta::hours(72)),
                ..UsageLimit::default()
            },
        );
        summary.clear_elapsed_windows(now);

        // An expiry is not a rolling reset: the credit is spent, not renewed.
        assert_eq!(summary.cinder_cove.utilization, Some(40.0));
        assert_eq!(summary.codename_limits["tangelo"].utilization, Some(9.0));
        assert_eq!(
            summary.extra_usage.as_ref().unwrap().used_credits,
            Some(12.34)
        );
    }

    #[test]
    fn any_window_reset_since_fetch_true_when_the_window_rolled_after_the_read() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = summary_with_elapsed_fable(now);
        summary.fetched_at = Some(now - chrono::TimeDelta::minutes(10));
        assert!(summary.any_window_reset_since_fetch(now));
    }

    #[test]
    fn any_window_reset_since_fetch_false_when_the_fetch_already_saw_the_reset() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut summary = summary_with_elapsed_fable(now);
        // Fetched after the reset the response reports: refetching would spend a
        // request per invocation on an answer the endpoint has already given.
        summary.fetched_at = Some(now - chrono::TimeDelta::minutes(1));
        assert!(!summary.any_window_reset_since_fetch(now));
        assert!(summary.any_window_reset_elapsed(now));
    }

    #[test]
    fn any_window_reset_since_fetch_falls_back_when_the_row_predates_age_stamping() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let summary = summary_with_elapsed_fable(now);
        assert_eq!(summary.fetched_at, None);
        assert!(summary.any_window_reset_since_fetch(now));
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

    /// Mirrors every top-level key the live endpoint returns. If the response
    /// grows a field, this stays green while `captures_unrecognized_fields`
    /// documents where it lands.
    const FULL_SHAPE_RESPONSE: &str = r#"{
        "five_hour": {"utilization": 10.0, "resets_at": "2026-09-01T12:00:00.111111+00:00",
                      "limit_dollars": null, "used_dollars": null, "remaining_dollars": null},
        "seven_day": {"utilization": 20.0, "resets_at": "2026-09-05T07:00:00.222222+00:00",
                      "limit_dollars": null, "used_dollars": null, "remaining_dollars": null},
        "seven_day_oauth_apps": null,
        "seven_day_opus": null,
        "seven_day_sonnet": null,
        "seven_day_cowork": null,
        "seven_day_omelette": null,
        "tangelo": null,
        "iguana_necktie": null,
        "omelette_promotional": null,
        "nimbus_quill": null,
        "cinder_cove": null,
        "amber_ladder": null,
        "extra_usage": {
            "is_enabled": false, "monthly_limit": 2000, "used_credits": 500.0,
            "utilization": 25.0, "currency": "USD", "decimal_places": 2,
            "disabled_reason": "org_level_disabled_until", "user_disabled": false,
            "spend_limit_reached": true, "credits_ever_enabled": true,
            "daily": null, "weekly": null
        },
        "limits": [],
        "spend": {
            "used": {"amount_minor": 500, "currency": "USD", "exponent": 2},
            "limit": {"amount_minor": 2000, "currency": "USD", "exponent": 2},
            "percent": 25, "severity": "normal", "enabled": false,
            "disabled_reason": "org_level_disabled_until",
            "cap": {"money": null, "credits": {"amount_minor": 2000, "exponent": 2}},
            "balance": null, "auto_reload": null,
            "disclaimer": "Usage credits cover you when you hit your plan limits.",
            "can_purchase_credits": false, "can_toggle": false
        },
        "member_dashboard_available": false
    }"#;

    #[test]
    fn live_response_shape_leaves_no_unclaimed_fields() {
        let dto: UsageResponseDto =
            serde_json::from_str(FULL_SHAPE_RESPONSE).expect("parse full-shape response");
        let summary = usage_summary_from_dto(dto);

        assert!(
            summary.unknown_fields.is_empty(),
            "unclaimed fields: {:?}",
            summary.unknown_fields
        );
        assert_eq!(summary.member_dashboard_available, Some(false));
        // Every codename slot was null, so none should occupy the map.
        assert!(summary.codename_limits.is_empty());
    }

    #[test]
    fn captures_unrecognized_fields_instead_of_dropping_them() {
        let raw = r#"{"five_hour": null, "brand_new_slot": {"utilization": 5.0}}"#;
        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse unknown field");
        let summary = usage_summary_from_dto(dto);

        assert_eq!(summary.unknown_fields, vec!["brand_new_slot".to_string()]);
    }

    #[test]
    fn populated_codename_slot_is_retained() {
        let raw = r#"{
            "seven_day_omelette": {"utilization": 12.0, "resets_at": "2026-09-05T07:00:00+00:00"},
            "tangelo": {"utilization": 3.0, "resets_at": null}
        }"#;
        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse codename slots");
        let summary = usage_summary_from_dto(dto);

        assert_eq!(summary.codename_limits.len(), 2);
        assert_eq!(
            summary
                .codename_limits
                .get("seven_day_omelette")
                .and_then(|limit| limit.utilization),
            Some(12.0)
        );
        assert!(summary.unknown_fields.is_empty());
    }

    #[test]
    fn rolling_codename_reset_invalidates_but_unknown_codename_does_not() {
        let now = Utc::now();
        let elapsed = UsageLimit {
            resets_at: Some(now - chrono::TimeDelta::hours(1)),
            ..UsageLimit::default()
        };

        let mut rolling = UsageSummary::default();
        rolling
            .codename_limits
            .insert("seven_day_omelette".to_string(), elapsed.clone());
        assert!(rolling.any_window_reset_elapsed(now));

        // Unknown semantics: an elapsed date is assumed to be a one-time expiry,
        // which must not force a refetch on every single invocation.
        let mut unknown = UsageSummary::default();
        unknown
            .codename_limits
            .insert("tangelo".to_string(), elapsed);
        assert!(!unknown.any_window_reset_elapsed(now));
    }

    #[test]
    fn extra_usage_scales_by_reported_decimal_places() {
        let two = r#"{"extra_usage": {"is_enabled": true, "monthly_limit": 5000,
                       "used_credits": 5091.0, "decimal_places": 2}}"#;
        let dto: UsageResponseDto = serde_json::from_str(two).expect("parse exponent 2");
        let extra = usage_summary_from_dto(dto).extra_usage.expect("extra");
        assert_eq!(extra.monthly_limit, Some(50.0));
        assert_eq!(extra.used_credits, Some(50.91));
        assert_eq!(extra.decimal_places, Some(2));

        // A three-decimal currency must not be read as cents.
        let three = r#"{"extra_usage": {"is_enabled": true, "monthly_limit": 5000,
                         "used_credits": 5091.0, "decimal_places": 3}}"#;
        let dto: UsageResponseDto = serde_json::from_str(three).expect("parse exponent 3");
        let extra = usage_summary_from_dto(dto).extra_usage.expect("extra");
        assert_eq!(extra.monthly_limit, Some(5.0));
        assert_eq!(extra.used_credits, Some(5.091));
    }

    #[test]
    fn extra_usage_without_decimal_places_stays_on_cents() {
        let raw = r#"{"extra_usage": {"is_enabled": true, "monthly_limit": 5000,
                      "used_credits": 5091.0}}"#;
        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse missing exponent");
        let extra = usage_summary_from_dto(dto).extra_usage.expect("extra");
        assert_eq!(extra.monthly_limit, Some(50.0));
        assert_eq!(extra.used_credits, Some(50.91));
        assert_eq!(extra.decimal_places, None);
    }

    #[test]
    fn nonsensical_decimal_places_falls_back_to_cents() {
        assert_eq!(minor_unit_divisor(Some(2)), 100.0);
        assert_eq!(minor_unit_divisor(Some(0)), 1.0);
        assert_eq!(minor_unit_divisor(None), 100.0);
        assert_eq!(minor_unit_divisor(Some(-3)), 100.0);
        assert_eq!(minor_unit_divisor(Some(99)), 100.0);
    }

    #[test]
    fn spend_cap_and_passthrough_fields_are_retained() {
        let dto: UsageResponseDto =
            serde_json::from_str(FULL_SHAPE_RESPONSE).expect("parse full-shape response");
        let spend = usage_summary_from_dto(dto).spend.expect("spend");

        let cap = spend.cap.expect("cap");
        assert_eq!(cap.credits.and_then(|money| money.amount), Some(20.0));
        assert!(cap.money.is_none());
        assert!(spend.disclaimer.is_some());
        assert!(spend.balance.is_none());
    }

    #[test]
    fn survives_fields_being_removed_from_the_response() {
        // Every optional section gone, including ones that were mandatory in
        // earlier shapes. The response must still parse.
        let dto: UsageResponseDto = serde_json::from_str("{}").expect("parse empty response");
        let summary = usage_summary_from_dto(dto);
        assert!(summary.window.utilization.is_none());
        assert!(summary.limits.is_empty());
        assert!(summary.extra_usage.is_none());

        // A section that kept only some of its keys.
        let partial = r#"{"five_hour": {"utilization": 44.0},
                          "spend": {"percent": 12.0},
                          "extra_usage": {"is_enabled": true}}"#;
        let dto: UsageResponseDto = serde_json::from_str(partial).expect("parse partial sections");
        let summary = usage_summary_from_dto(dto);
        assert_eq!(summary.window.utilization, Some(44.0));
        assert_eq!(summary.spend.and_then(|spend| spend.percent), Some(12.0));
    }

    #[test]
    fn a_reshaped_section_does_not_take_down_the_rest() {
        // `spend` becomes a scalar and `extra_usage` an array: both shapes the
        // structs cannot represent. The percentages that drive the line must
        // still come through.
        let raw = r#"{
            "five_hour": {"utilization": 17.0, "resets_at": "2026-09-01T09:30:00+00:00"},
            "seven_day": {"utilization": 30.0},
            "spend": 12345,
            "extra_usage": ["unexpected"],
            "limits": [
                {"kind": "session", "group": "session", "percent": 17.0},
                {"kind": "weekly_all", "group": "weekly", "percent": "thirty"},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 5.0,
                 "scope": {"model": {"id": null, "display_name": "Fable"}}}
            ]
        }"#;
        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse reshaped sections");
        let summary = usage_summary_from_dto(dto);

        assert_eq!(summary.window.utilization, Some(17.0));
        assert_eq!(summary.seven_day.utilization, Some(30.0));
        assert!(summary.window.resets_at.is_some());
        // The two unrepresentable sections degrade to absent, not to a failure.
        assert!(summary.spend.is_none());
        assert!(summary.extra_usage.is_none());
        // Only the row whose `percent` changed type is dropped.
        assert_eq!(summary.limits.len(), 2);
        assert_eq!(
            find_scoped_weekly_limit(&summary.limits, "fable").and_then(|limit| limit.percent),
            Some(5.0)
        );
    }

    #[test]
    fn a_reshaped_nested_field_costs_only_its_own_section() {
        // `spend.limit` stops being a money object. `spend` is lost, but the
        // sibling windows are untouched.
        let raw = r#"{
            "five_hour": {"utilization": 8.0},
            "spend": {"percent": 3.0, "limit": "50 dollars"}
        }"#;
        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse nested reshape");
        let summary = usage_summary_from_dto(dto);
        assert_eq!(summary.window.utilization, Some(8.0));
        assert!(summary.spend.is_none());
    }

    #[test]
    fn added_fields_are_recorded_while_known_ones_still_parse() {
        let raw = r#"{
            "five_hour": {"utilization": 21.0},
            "brand_new_window": {"utilization": 9.0},
            "some_scalar_flag": true,
            "nested_novelty": {"a": {"b": [1, 2, 3]}}
        }"#;
        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse added fields");
        let summary = usage_summary_from_dto(dto);

        assert_eq!(summary.window.utilization, Some(21.0));
        assert_eq!(
            summary.unknown_fields,
            vec![
                "brand_new_window".to_string(),
                "nested_novelty".to_string(),
                "some_scalar_flag".to_string(),
            ]
        );
    }

    #[test]
    fn limits_rows_still_supply_percentages_when_the_named_windows_disappear() {
        // The dedicated `five_hour` / `seven_day` objects are gone entirely, as
        // they would be after a rename. The generic rows carry the same numbers,
        // so the line keeps its percentages without a code change.
        let raw = r#"{
            "limits": [
                {"kind": "session", "group": "session", "percent": 17.0,
                 "resets_at": "2026-09-01T09:30:00+00:00", "is_active": false},
                {"kind": "weekly_all", "group": "weekly", "percent": 30.0,
                 "resets_at": "2026-09-05T07:00:00+00:00", "is_active": true},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 24.0,
                 "scope": {"model": {"id": null, "display_name": "Opus"}}}
            ]
        }"#;
        let dto: UsageResponseDto = serde_json::from_str(raw).expect("parse limits-only response");
        let summary = usage_summary_from_dto(dto);

        assert_eq!(summary.window.utilization, Some(17.0));
        assert!(summary.window.resets_at.is_some());
        assert_eq!(summary.seven_day.utilization, Some(30.0));
        assert_eq!(summary.seven_day_opus.utilization, Some(24.0));
    }

    #[test]
    fn reset_timestamps_parse_as_rfc3339_or_epoch_seconds() {
        let rfc =
            r#"{"five_hour": {"utilization": 5.0, "resets_at": "2026-09-01T09:30:00+00:00"}}"#;
        let dto: UsageResponseDto = serde_json::from_str(rfc).expect("parse rfc3339");
        let rfc_reset = usage_summary_from_dto(dto).window.resets_at.expect("reset");

        // Claude Code's own hook already reports resets as epoch seconds, so the
        // endpoint moving to that form is a plausible change rather than a
        // hypothetical one.
        let epoch = r#"{"five_hour": {"utilization": 5.0, "resets_at": 1788255000}}"#;
        let dto: UsageResponseDto = serde_json::from_str(epoch).expect("parse epoch seconds");
        let epoch_reset = usage_summary_from_dto(dto).window.resets_at.expect("reset");

        assert_eq!(rfc_reset, epoch_reset);
    }

    #[test]
    fn retry_after_accepts_delta_seconds_only() {
        assert_eq!(parse_retry_after("293"), Some(293));
        assert_eq!(parse_retry_after("  120 "), Some(120));
        assert_eq!(parse_retry_after("0"), None);
        assert_eq!(parse_retry_after("-5"), None);
        // HTTP-date form is deliberately unhandled rather than mis-parsed.
        assert_eq!(parse_retry_after("Wed, 29 Jul 2026 05:51:11 GMT"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    #[test]
    fn cache_ttl_override_cannot_drop_below_the_shared_budget_floor() {
        // Guards the parse-and-clamp rule without mutating process env, which
        // would race the other tests in this binary.
        let resolve = |raw: &str| {
            raw.trim()
                .parse::<i64>()
                .ok()
                .map(|seconds| seconds.max(MIN_CACHE_TTL_SECONDS))
                .unwrap_or(CACHE_TTL_SECONDS)
        };
        assert_eq!(resolve("5"), MIN_CACHE_TTL_SECONDS);
        assert_eq!(resolve("90"), 90);
        assert_eq!(resolve("not-a-number"), CACHE_TTL_SECONDS);
    }
}
