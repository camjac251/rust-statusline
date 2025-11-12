//! # Cache Module
//!
//! Provides in-memory caching for parsed JSONL data to improve performance
//! on frequent statusline updates.

use crate::models::Entry;
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

/// Type alias for cached usage data: (entries, today_cost, latest_reset, api_key_source)
pub type CachedUsageData = (Vec<Entry>, f64, Option<DateTime<Utc>>, Option<String>);

/// Cache entry with expiration and date tracking
#[derive(Clone, Debug)]
struct CacheEntry {
    entries: Vec<Entry>,
    today_cost: f64,
    cached_date: NaiveDate, // Track which day this cost is for
    latest_reset: Option<DateTime<Utc>>,
    api_key_source: Option<String>,
    expires_at: DateTime<Utc>,
}

/// Global cache for parsed JSONL data
static USAGE_CACHE: Lazy<Mutex<HashMap<String, CacheEntry>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Default cache TTL in seconds
const CACHE_TTL_SECONDS: i64 = 60;

/// Generate cache key from session ID and project directory
fn make_cache_key(session_id: &str, project_dir: Option<&str>) -> String {
    match project_dir {
        Some(dir) => format!("{}:{}", session_id, dir),
        None => session_id.to_string(),
    }
}

/// Get cached usage data if available, not expired, and for the current day
pub fn get_cached_usage(session_id: &str, project_dir: Option<&str>) -> Option<CachedUsageData> {
    let key = make_cache_key(session_id, project_dir);
    let now = Utc::now();
    let today = Local::now().date_naive();

    let cache = USAGE_CACHE.lock().ok()?;
    let entry = cache.get(&key)?;

    // Only return cached data if not expired AND cached for current day (prevents using yesterday's cost after midnight)
    if entry.expires_at > now && entry.cached_date == today {
        Some((
            entry.entries.clone(),
            entry.today_cost,
            entry.latest_reset,
            entry.api_key_source.clone(),
        ))
    } else {
        None
    }
}

/// Store usage data in cache with current date
pub fn cache_usage(
    session_id: &str,
    project_dir: Option<&str>,
    entries: Vec<Entry>,
    today_cost: f64,
    latest_reset: Option<DateTime<Utc>>,
    api_key_source: Option<String>,
) {
    let key = make_cache_key(session_id, project_dir);
    let ttl = std::env::var("CLAUDE_CACHE_TTL")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(CACHE_TTL_SECONDS);

    let now = Utc::now();
    let today = Local::now().date_naive();
    let expires_at = now + Duration::seconds(ttl);

    if let Ok(mut cache) = USAGE_CACHE.lock() {
        // Clean up expired or outdated entries while we have the lock
        cache.retain(|_, entry| entry.expires_at > now && entry.cached_date == today);

        // Add new entry with current date
        cache.insert(
            key,
            CacheEntry {
                entries,
                today_cost,
                cached_date: today,
                latest_reset,
                api_key_source,
                expires_at,
            },
        );
    }
}

/// Clear all cached data
pub fn clear_cache() {
    if let Ok(mut cache) = USAGE_CACHE.lock() {
        cache.clear();
    }
}

/// Get cache statistics (for debugging)
pub fn cache_stats() -> (usize, usize) {
    if let Ok(cache) = USAGE_CACHE.lock() {
        let total = cache.len();
        let now = Utc::now();
        let valid = cache.values().filter(|e| e.expires_at > now).count();
        (total, valid)
    } else {
        (0, 0)
    }
}
