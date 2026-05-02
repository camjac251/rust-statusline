//! # Usage Module
//!
//! Tracks and analyzes Claude Code session usage data from transcript files and usage logs.
//!
//! ## Key Functions
//!
//! - `scan_usage`: Scans Claude config directories for usage JSONL files
//! - `identify_blocks`: Groups usage entries into 5-hour window blocks with gap detection
//! - `calc_context_from_*`: Calculates context window usage from various sources

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

use crate::models::prompt_cache::{PROMPT_CACHE_1H_TTL_SECONDS, PROMPT_CACHE_5M_TTL_SECONDS};
use crate::models::{
    Entry, MessageUsage, PromptCacheBucketInfo, PromptCacheBucketKind, PromptCacheInfo,
    RateLimitInfo, TranscriptLine,
};
use crate::pricing::calculate_cost_for_usage;
use crate::utils::{context_limit_for_model_display, parse_iso_date, system_overhead_tokens};

/// Session-specific state extracted from the session's own transcript file.
/// Unlike the global scan, this reads only the target transcript for fast, authoritative data.
#[derive(Debug, Default)]
pub struct SessionState {
    /// "fast" or "normal" -- from the most recent API response in this session
    pub speed: Option<String>,
    /// The actual model used in the most recent API call
    pub model: Option<String>,
    /// Service tier from the most recent API response
    pub service_tier: Option<String>,
    /// Session cost from the most recent SDK result message
    pub session_cost: Option<f64>,
    /// Timestamp of the latest assistant response in this session
    pub last_assistant_at: Option<DateTime<Utc>>,
    /// Prompt-cache activity from this session's assistant usage blocks.
    pub prompt_cache: Option<PromptCacheInfo>,
}

/// Parse session-specific state directly from a transcript file.
/// Reads all lines sequentially, keeping the latest values (last writer wins).
pub fn parse_session_state(transcript_path: &Path) -> SessionState {
    let mut state = SessionState::default();
    let mut cache_5m_bucket: Option<PromptCacheBucketInfo> = None;
    let mut cache_1h_bucket: Option<PromptCacheBucketInfo> = None;
    let mut cache_unknown_bucket: Option<PromptCacheBucketInfo> = None;
    let mut last_cache_write_at: Option<DateTime<Utc>> = None;
    let mut last_cache_read_at: Option<DateTime<Utc>> = None;
    let mut last_cache_write_tokens = 0;
    let mut last_cache_read_tokens = 0;

    let file = match File::open(transcript_path) {
        Ok(f) => f,
        Err(_) => return state,
    };

    // Read all lines (transcript files are bounded by context window size)
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(t) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // SDK result messages have authoritative session cost
        if v.get("type").and_then(|s| s.as_str()) == Some("result") {
            if let Some(cost) = v.get("total_cost_usd").and_then(|n| n.as_f64()) {
                if cost > state.session_cost.unwrap_or(0.0) {
                    state.session_cost = Some(cost);
                }
            }
        }

        // Assistant messages with usage blocks have speed, model, service_tier
        let msg = if let Some(m) = v.get("message") {
            m
        } else {
            continue;
        };
        if msg.get("role").and_then(|s| s.as_str()) != Some("assistant") {
            continue;
        }
        let assistant_ts = v
            .get("timestamp")
            .and_then(|s| s.as_str())
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&Utc));
        if let Some(ts) = assistant_ts {
            if state
                .last_assistant_at
                .map(|last| ts > last)
                .unwrap_or(true)
            {
                state.last_assistant_at = Some(ts);
            }
        }
        let usage = match msg.get("usage") {
            Some(u) => u,
            None => continue,
        };
        // Only update from entries that have actual token data
        if usage
            .get("input_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0)
            == 0
            && usage
                .get("output_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
                == 0
            && usage
                .get("cache_creation_input_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
                == 0
            && usage
                .get("cache_read_input_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
                == 0
            && usage
                .get("cache_creation")
                .and_then(|creation| creation.get("ephemeral_5m_input_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
                == 0
            && usage
                .get("cache_creation")
                .and_then(|creation| creation.get("ephemeral_1h_input_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
                == 0
        {
            continue;
        }

        if let Some(spd) = usage
            .get("speed")
            .and_then(|s| s.as_str())
            .or_else(|| v.get("speed").and_then(|s| s.as_str()))
        {
            state.speed = Some(spd.to_string());
        }
        if let Some(mdl) = msg.get("model").and_then(|s| s.as_str()) {
            state.model = Some(mdl.to_string());
        }
        if let Some(tier) = usage.get("service_tier").and_then(|s| s.as_str()) {
            state.service_tier = Some(tier.to_string());
        }

        if let Some(ts) = assistant_ts {
            let cache_create_total = usage
                .get("cache_creation_input_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            let cache_read = usage
                .get("cache_read_input_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            let cache_creation = usage.get("cache_creation");
            let cache_1h = cache_creation
                .and_then(|creation| creation.get("ephemeral_1h_input_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            let cache_5m = cache_creation
                .and_then(|creation| creation.get("ephemeral_5m_input_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0);

            if cache_5m > 0 {
                cache_5m_bucket = Some(PromptCacheBucketInfo {
                    kind: PromptCacheBucketKind::FiveMinute,
                    created_at: ts,
                    ttl_seconds: PROMPT_CACHE_5M_TTL_SECONDS,
                    input_tokens: cache_5m,
                });
            }
            if cache_1h > 0 {
                cache_1h_bucket = Some(PromptCacheBucketInfo {
                    kind: PromptCacheBucketKind::OneHour,
                    created_at: ts,
                    ttl_seconds: PROMPT_CACHE_1H_TTL_SECONDS,
                    input_tokens: cache_1h,
                });
            }
            let cache_create_known = cache_5m + cache_1h;
            let cache_create_unknown = cache_create_total.saturating_sub(cache_create_known);

            if cache_create_unknown > 0 {
                cache_unknown_bucket = Some(PromptCacheBucketInfo {
                    kind: PromptCacheBucketKind::Unknown,
                    created_at: ts,
                    ttl_seconds: PROMPT_CACHE_5M_TTL_SECONDS,
                    input_tokens: cache_create_unknown,
                });
            }
            if cache_create_known > 0 || cache_create_unknown > 0 {
                last_cache_write_at = Some(ts);
                last_cache_write_tokens = cache_create_known + cache_create_unknown;
            }
            if cache_read > 0 {
                last_cache_read_at = Some(ts);
                last_cache_read_tokens = cache_read;
            }
        }
    }

    let mut buckets = Vec::new();
    if let Some(bucket) = cache_5m_bucket {
        buckets.push(bucket);
    }
    if let Some(bucket) = cache_1h_bucket {
        buckets.push(bucket);
    }
    if let Some(bucket) = cache_unknown_bucket {
        buckets.push(bucket);
    }
    if last_cache_write_at.is_some() || last_cache_read_at.is_some() {
        state.prompt_cache = Some(PromptCacheInfo {
            buckets,
            last_cache_write_at,
            last_cache_read_at,
            cache_write_input_tokens: last_cache_write_tokens,
            cache_read_input_tokens: last_cache_read_tokens,
            now: Utc::now(),
        });
    }

    state
}

// Helper: detect reset time from assistant text like "... limit reached ... resets 5am" with DST correction
static ASSISTANT_LIMIT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)limit\s+reached.*resets\s+(\d{1,2})\s*(am|pm)").unwrap());

/// Round reset time to nearest hour (:00) to handle timezone/clock offset issues
pub fn normalize_reset_time(dt: DateTime<Utc>) -> DateTime<Utc> {
    let minute = dt.minute();
    let second = dt.second();

    // If already at :00:00, return as-is
    if minute == 0 && second == 0 {
        return dt;
    }

    // Always round to nearest hour (:00) to handle timezone/clock offset issues
    // Round up if minute >= 30, otherwise round down
    let rounded_hour = if minute >= 30 {
        // Round up to next hour
        dt + chrono::TimeDelta::hours(1)
    } else {
        // Round down to current hour
        dt
    };

    rounded_hour
        .with_minute(0)
        .and_then(|d| d.with_second(0))
        .and_then(|d| d.with_nanosecond(0))
        .unwrap_or(dt)
}

// Context warning message patterns
static CONTEXT_AUTO_COMPACT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"Context left until auto-compact: (\d+)%").unwrap());

static CONTEXT_LOW_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"Context low \((\d+)% remaining\)").unwrap());

fn parse_am_pm_reset(ts_utc: DateTime<Utc>, text: &str) -> Option<DateTime<Utc>> {
    let caps = ASSISTANT_LIMIT_RE.captures(text)?;
    let hour_s = caps.get(1)?.as_str();
    let ampm = caps.get(2)?.as_str().to_lowercase();
    let base_hour: u32 = hour_s.parse().ok()?;
    if base_hour == 0 || base_hour > 12 {
        return None;
    }
    let hour24: u32 = if ampm == "am" {
        if base_hour == 12 { 0 } else { base_hour }
    } else if base_hour == 12 {
        12
    } else {
        (base_hour + 12) % 24
    };
    // Convert ts to local
    let ts_local = ts_utc.with_timezone(&Local);
    // Construct same-day local time at the given hour
    let mut same_day = ts_local
        .with_hour(hour24)?
        .with_minute(0)?
        .with_second(0)?
        .with_nanosecond(0)?;

    // Optional DST correction (for historical Claude Code bug where reset hour was shown in standard time).
    // Enable by setting CLAUDE_RESET_ASSUME_STANDARD_TIME=1
    let assume_standard = std::env::var("CLAUDE_RESET_ASSUME_STANDARD_TIME")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if assume_standard {
        // Compute DST offset difference: current local offset minus minimum offset in this year
        let year = ts_local.year();
        let mut min_off: i32 = ts_local.offset().local_minus_utc();
        for m in 1..=12 {
            // Prefer midnight; if ambiguous/unavailable, try noon to avoid DST gaps
            let cand_midnight = Local.with_ymd_and_hms(year, m as u32, 1, 0, 0, 0);
            let cand = match cand_midnight {
                chrono::LocalResult::Single(dt) => Some(dt),
                _ => match Local.with_ymd_and_hms(year, m as u32, 1, 12, 0, 0) {
                    chrono::LocalResult::Single(dt) => Some(dt),
                    _ => None,
                },
            };
            if let Some(dt) = cand {
                let off = dt.offset().local_minus_utc();
                if off < min_off {
                    min_off = off;
                }
            }
        }
        let cur_off = ts_local.offset().local_minus_utc();
        let diff_minutes = cur_off - min_off; // typically 0 or +60 during DST
        if diff_minutes != 0 {
            same_day += chrono::TimeDelta::minutes(diff_minutes as i64);
        }
    }
    // If we've already passed that time today, use tomorrow
    let reset_local = if ts_local < same_day {
        same_day
    } else {
        same_day + chrono::TimeDelta::days(1)
    };
    Some(normalize_reset_time(reset_local.with_timezone(&Utc)))
}

pub fn calc_context_from_transcript(
    transcript_path: &Path,
    model_id: &str,
    model_display_name: &str,
) -> Option<(u64, u32)> {
    // Stream the file line-by-line to avoid loading entire transcripts into memory.
    // Keep the last assistant message with usage; sum input + output + cache* to mirror Claude CLI totals.
    let file = File::open(transcript_path).ok()?;
    let reader = BufReader::new(file);
    let mut last_total_in: Option<u64> = None;
    let mut context_warning_pct: Option<u32> = None;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let t = line.trim();
        if t.is_empty() {
            continue;
        }

        // First try to parse as JSON
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(t) {
            // Check for system messages with context warnings
            if parsed.get("type").and_then(|v| v.as_str()) == Some("system_message") {
                if let Some(content) = parsed.get("content").and_then(|v| v.as_str()) {
                    // Parse "Context left until auto-compact: X%"
                    if let Some(caps) = CONTEXT_AUTO_COMPACT_RE.captures(content) {
                        if let Ok(percent_left) = caps[1].parse::<u32>() {
                            context_warning_pct = Some(100 - percent_left);
                        }
                    }
                    // Parse "Context low (X% remaining)"
                    else if let Some(caps) = CONTEXT_LOW_RE.captures(content) {
                        if let Ok(percent_left) = caps[1].parse::<u32>() {
                            context_warning_pct = Some(100 - percent_left);
                        }
                    }
                }
            }

            // Continue with existing assistant message parsing
            if let Ok(parsed_line) = serde_json::from_value::<TranscriptLine>(parsed) {
                if parsed_line.r#type.as_deref() == Some("assistant") {
                    let usage = parsed_line
                        .message
                        .and_then(|m| m.usage)
                        .unwrap_or(MessageUsage {
                            input_tokens: None,
                            output_tokens: None,
                            cache_creation_input_tokens: None,
                            cache_read_input_tokens: None,
                            cache_creation: None,
                        });
                    let inp = usage.input_tokens.unwrap_or(0);
                    let out = usage.output_tokens.unwrap_or(0);
                    let total_in = inp
                        + out
                        + usage.cache_creation_input_tokens.unwrap_or(0)
                        + usage.cache_read_input_tokens.unwrap_or(0);
                    if total_in > 0 {
                        last_total_in = Some(total_in);
                    }
                }
            }
        }
    }

    // Prefer token-based calculation if available, fall back to context warning
    let budget = context_limit_for_model_display(model_id, model_display_name);
    if let Some(total_in) = last_total_in {
        let overhead = system_overhead_tokens();
        let adjusted = total_in.saturating_add(overhead);
        let pct = if budget == 0 {
            if adjusted == 0 { 0 } else { 100 }
        } else {
            ((adjusted as f64 / budget as f64) * 100.0).round() as u32
        };
        Some((adjusted, pct.min(100)))
    } else if let Some(warning_pct) = context_warning_pct {
        let estimated_tokens = if budget == 0 {
            0
        } else {
            ((warning_pct as f64 / 100.0) * budget as f64).round() as u64
        };
        Some((estimated_tokens, warning_pct.min(100)))
    } else {
        None
    }
}

pub fn calc_context_from_entries(
    entries: &[Entry],
    session_id: &str,
    model_id: &str,
    model_display_name: &str,
) -> Option<(u64, u32)> {
    let mut filtered: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.session_id.as_deref() == Some(session_id))
        .collect();
    if filtered.is_empty() {
        return None;
    }
    filtered.sort_by_key(|e| e.ts);
    let last = filtered.last()?;
    let total_in = last.input + last.output + last.cache_create + last.cache_read;
    let overhead = system_overhead_tokens();
    let adjusted = total_in.saturating_add(overhead);
    let limit = context_limit_for_model_display(model_id, model_display_name);
    let pct = if limit == 0 {
        if adjusted == 0 { 0 } else { 100 }
    } else {
        ((adjusted as f64 / limit as f64) * 100.0).round() as u32
    };
    Some((adjusted, pct.min(100)))
}

// Additional usage metrics for enhanced tracking
#[derive(Debug, Clone)]
pub struct EnhancedMetrics {
    pub agent_invocations: HashMap<String, u32>,
    pub compact_summary_count: u32,
    pub last_compact_time: Option<DateTime<Utc>>,
    pub tool_correlation_count: usize,
    pub message_complexity: MessageComplexity,
}

// Message complexity tracking for accurate session limits
#[derive(Debug, Clone)]
pub struct MessageComplexity {
    pub total_weight: f64,
    pub message_count: u32,
    pub short_messages: u32, // weight < 0.5
    pub long_messages: u32,  // weight > 2.0
    pub average_weight: f64,
}

impl Default for MessageComplexity {
    fn default() -> Self {
        Self {
            total_weight: 0.0,
            message_count: 0,
            short_messages: 0,
            long_messages: 0,
            average_weight: 1.0,
        }
    }
}

/// Efficiently discover JSONL files using walkdir with directory-level mtime filtering.
/// Skips entire directory trees when the directory mtime is older than cutoff,
/// dramatically reducing stat() syscalls from O(all_files) to O(recent_dirs).
fn find_recent_jsonl_files(root: &Path, cutoff: SystemTime) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            // Always process the root
            if entry.depth() == 0 {
                return true;
            }
            // For directories, check mtime - if old, skip entire subtree
            if entry.file_type().is_dir() {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        return mtime >= cutoff;
                    }
                }
                // If we can't get mtime, include the directory to be safe
                return true;
            }
            // For files, only include .jsonl files
            entry.path().extension().is_some_and(|ext| ext == "jsonl")
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .filter(|e| {
            // Final mtime check on files (dirs already filtered)
            if let Ok(meta) = e.metadata() {
                if let Ok(mtime) = meta.modified() {
                    return mtime >= cutoff;
                }
            }
            true
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

#[allow(clippy::type_complexity)]
pub fn scan_usage(
    paths: &[PathBuf],
    session_id: &str,
    project_dir: Option<&str>,
    _model_id_for_probe: Option<&str>,
) -> Result<(
    f64, /*session*/
    f64, /*session_today*/
    f64, /*today*/
    Vec<Entry>,
    Option<DateTime<Utc>>,
    Option<String>,
    Option<RateLimitInfo>,
)> {
    // Check cache first
    if let Some((cached_entries, cached_today_cost, cached_reset, cached_api_key)) =
        crate::cache::get_cached_usage(session_id, project_dir)
    {
        let today = Local::now().date_naive();
        // Calculate session cost and session today cost from cached entries
        let mut session_cost = 0.0;
        let mut session_today_cost = 0.0;
        for e in &cached_entries {
            if e.session_id.as_deref() == Some(session_id) {
                session_cost += e.cost;
                let ts_s = e.ts.to_rfc3339();
                if let Some(d) = parse_iso_date(&ts_s) {
                    if d == today {
                        session_today_cost += e.cost;
                    }
                }
            }
        }
        return Ok((
            session_cost,
            session_today_cost,
            cached_today_cost,
            cached_entries,
            cached_reset,
            cached_api_key,
            read_persisted_reset_state().map(|st| RateLimitInfo {
                status: st.status,
                resets_at: st.reset_at,
                fallback_available: st.fallback.as_deref().map(|s| s == "available"),
                fallback_percentage: st.fallback_percentage,
                rate_limit_type: st.rate_limit_type,
                overage_status: st.overage_status,
                overage_resets_at: st.overage_resets_at,
                is_using_overage: None,
            }),
        ));
    }

    let today = Local::now().date_naive();
    let mut session_cost = 0.0f64;
    // Prefer precise session cost from SDK result messages when available.
    // Track the maximum observed total_cost_usd for this session to avoid overcounting across retries.
    let mut session_cost_via_results: f64 = 0.0;
    let mut today_cost = 0.0f64;
    // Aggregate usage by request/message id to avoid double-counting incremental updates
    let mut aggregated: HashMap<String, Entry> = HashMap::new();
    let mut latest_reset: Option<DateTime<Utc>> = None;
    let mut api_key_source: Option<String> = None;
    // Map ids to session for imputing when missing on some lines
    let mut sid_by_mid: HashMap<String, String> = HashMap::new();
    let mut sid_by_rid: HashMap<String, String> = HashMap::new();
    // Track last-seen raw values per aggregation key to detect cumulative vs delta updates
    let mut last_seen_raw: HashMap<String, (u64, u64, u64, u64)> = HashMap::new();
    // Once an id shows non-monotonicity, mark it as delta mode to sum subsequent updates
    let mut force_delta_mode: HashMap<String, bool> = HashMap::new();
    // Track agent/Task tool invocations for better cost analysis
    let mut agent_invocations: HashMap<String, u32> = HashMap::new();
    // Track tool_use blocks for correlation with tool_result (for accurate token counting)
    let mut tool_use_tokens: HashMap<String, u64> = HashMap::new();
    // Track compact summaries (when conversations get auto-compacted)
    let mut _compact_summary_count = 0u32;
    let mut _last_compact_time: Option<DateTime<Utc>> = None;

    // Optimization: Skip files older than 48 hours by default
    let cutoff_time = if let Ok(hours_str) = env::var("CLAUDE_SCAN_LOOKBACK_HOURS") {
        if let Ok(hours) = hours_str.parse::<i64>() {
            Utc::now() - Duration::hours(hours)
        } else {
            Utc::now() - Duration::hours(48)
        }
    } else {
        Utc::now() - Duration::hours(48)
    };
    // Convert to SystemTime for efficient walkdir filtering
    let cutoff_system = SystemTime::UNIX_EPOCH
        + std::time::Duration::from_secs(cutoff_time.timestamp().max(0) as u64);

    for base in paths {
        let root = base.join("projects");
        if !root.is_dir() {
            continue;
        }
        // Global reset anchor discovery across all recent project files under this root
        // Uses walkdir with directory-level mtime filtering for efficiency
        let recent_files = find_recent_jsonl_files(&root, cutoff_system);
        for path in &recent_files {
            let file = match File::open(path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(t) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Capture precise cost from SDK result messages for this session
                if v.get("type").and_then(|s| s.as_str()) == Some("result") {
                    let sid_v = v
                        .get("sessionId")
                        .or_else(|| v.get("session_id"))
                        .and_then(|s| s.as_str());
                    if let Some(sid_here) = sid_v {
                        if sid_here == session_id {
                            if let Some(cn) = v.get("total_cost_usd") {
                                if let Some(n) = cn.as_f64() {
                                    if n > session_cost_via_results {
                                        session_cost_via_results = n;
                                    }
                                } else if let Some(s) = cn.as_str() {
                                    if let Ok(n) = s.parse::<f64>() {
                                        if n > session_cost_via_results {
                                            session_cost_via_results = n;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Best-effort timestamp for limit parsing
                let tsd_for_limits: Option<DateTime<Utc>> = v
                    .get("timestamp")
                    .and_then(|s| s.as_str())
                    .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                    .map(|d| d.with_timezone(&Utc));

                if v.get("isApiErrorMessage").and_then(|b| b.as_bool()) == Some(true) {
                    if let Some(msg) = v.get("message") {
                        if let Some(content) = msg.get("content") {
                            if let Some(arr) = content.as_array() {
                                for c in arr {
                                    if let Some(text) = c.get("text").and_then(|s| s.as_str()) {
                                        if text.contains("Claude AI usage limit reached") {
                                            if let Some(idx) = text.rfind('|') {
                                                if let Ok(n) = text[idx + 1..].trim().parse::<i64>()
                                                {
                                                    if n > 0 {
                                                        let normalized_epoch =
                                                            normalize_reset_anchor(n);
                                                        if let Some(dt) =
                                                            DateTime::<Utc>::from_timestamp(
                                                                normalized_epoch,
                                                                0,
                                                            )
                                                        {
                                                            let dt = normalize_reset_time(dt);
                                                            if latest_reset
                                                                .map(|x| dt > x)
                                                                .unwrap_or(true)
                                                            {
                                                                latest_reset = Some(dt);
                                                            }
                                                        }
                                                    }
                                                }
                                            } else if let Some(base) = tsd_for_limits {
                                                if let Some(dt) = parse_am_pm_reset(base, text) {
                                                    if latest_reset.map(|x| dt > x).unwrap_or(true)
                                                    {
                                                        latest_reset = Some(dt);
                                                    }
                                                }
                                            } else if let Some(dt) =
                                                parse_am_pm_reset(Utc::now(), text)
                                            {
                                                if latest_reset.map(|x| dt > x).unwrap_or(true) {
                                                    latest_reset = Some(dt);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else if let Some(content) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for c in content {
                        if let Some(text) = c.get("text").and_then(|s| s.as_str()) {
                            if text.to_lowercase().contains("usage limit") {
                                if let Some(idx) = text.rfind('|') {
                                    if let Ok(n) = text[idx + 1..].trim().parse::<i64>() {
                                        if n > 0 {
                                            let normalized_epoch = normalize_reset_anchor(n);
                                            if let Some(dt) =
                                                DateTime::<Utc>::from_timestamp(normalized_epoch, 0)
                                            {
                                                let dt = normalize_reset_time(dt);
                                                if latest_reset.map(|x| dt > x).unwrap_or(true) {
                                                    latest_reset = Some(dt);
                                                }
                                            }
                                        }
                                    }
                                } else if let Some(base) = tsd_for_limits {
                                    if let Some(dt) = parse_am_pm_reset(base, text) {
                                        if latest_reset.map(|x| dt > x).unwrap_or(true) {
                                            latest_reset = Some(dt);
                                        }
                                    }
                                } else if let Some(dt) = parse_am_pm_reset(Utc::now(), text) {
                                    if latest_reset.map(|x| dt > x).unwrap_or(true) {
                                        latest_reset = Some(dt);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Reuse the recent_files list from reset anchor discovery (already filtered by mtime)
        // This avoids a second expensive directory walk
        for path in &recent_files {
            let file = match File::open(path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            // Derive project name from path under <base>/projects/<project>/...
            let proj_name: Option<String> = path
                .strip_prefix(&root)
                .ok()
                .and_then(|p| p.components().next())
                .map(|c| c.as_os_str().to_string_lossy().to_string());
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(t) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // detect init system apiKeySource
                if api_key_source.is_none()
                    && v.get("type").and_then(|s| s.as_str()) == Some("system")
                    && v.get("subtype").and_then(|s| s.as_str()) == Some("init")
                {
                    if let Some(src) = v.get("apiKeySource").and_then(|s| s.as_str()) {
                        api_key_source = Some(src.to_string());
                    }
                }

                // Track Task/Agent tool invocations and tool_use blocks
                if v.get("type").and_then(|s| s.as_str()) == Some("assistant") {
                    if let Some(msg) = v.get("message") {
                        if let Some(content) = msg.get("content") {
                            if let Some(content_array) = content.as_array() {
                                for block in content_array {
                                    if block.get("type").and_then(|s| s.as_str())
                                        == Some("tool_use")
                                    {
                                        // Track all tool_use blocks by ID for correlation
                                        if let Some(tool_id) =
                                            block.get("id").and_then(|s| s.as_str())
                                        {
                                            // Estimate tokens for tool use (name + input)
                                            let name_tokens = block
                                                .get("name")
                                                .and_then(|s| s.as_str())
                                                .map(|s| s.len() as u64 / 4)
                                                .unwrap_or(0);
                                            let input_tokens = block
                                                .get("input")
                                                .map(|v| v.to_string().len() as u64 / 4)
                                                .unwrap_or(0);
                                            tool_use_tokens.insert(
                                                tool_id.to_string(),
                                                name_tokens + input_tokens,
                                            );
                                        }

                                        // Special handling for Task tool (agent invocations)
                                        if block.get("name").and_then(|s| s.as_str())
                                            == Some("Task")
                                        {
                                            if let Some(input) = block.get("input") {
                                                if let Some(agent_type) = input
                                                    .get("subagent_type")
                                                    .and_then(|s| s.as_str())
                                                {
                                                    *agent_invocations
                                                        .entry(agent_type.to_string())
                                                        .or_insert(0) += 1;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Track tool_result blocks and correlate with tool_use (for accurate context tracking)
                if v.get("type").and_then(|s| s.as_str()) == Some("user") {
                    if let Some(msg) = v.get("message") {
                        if let Some(content) = msg.get("content") {
                            if let Some(content_array) = content.as_array() {
                                for block in content_array {
                                    if block.get("type").and_then(|s| s.as_str())
                                        == Some("tool_result")
                                    {
                                        if let Some(tool_use_id) =
                                            block.get("tool_use_id").and_then(|s| s.as_str())
                                        {
                                            // Add tool result tokens to the original tool_use tracking
                                            let result_tokens = block
                                                .get("content")
                                                .map(|v| v.to_string().len() as u64 / 4)
                                                .unwrap_or(0);
                                            if let Some(existing) =
                                                tool_use_tokens.get_mut(tool_use_id)
                                            {
                                                *existing += result_tokens;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Detect compact summaries (conversation compaction events)
                if v.get("isCompactSummary").and_then(|b| b.as_bool()) == Some(true) {
                    _compact_summary_count += 1;
                    if let Some(ts_str) = v.get("timestamp").and_then(|s| s.as_str()) {
                        if let Ok(ts) = DateTime::parse_from_rfc3339(ts_str) {
                            _last_compact_time = Some(ts.with_timezone(&Utc));
                        }
                    }
                }

                // Also check for compact summary indicators in system messages
                if v.get("type").and_then(|s| s.as_str()) == Some("system") {
                    if let Some(content) = v.get("content").and_then(|s| s.as_str()) {
                        if content.contains("conversation has been compacted")
                            || content.contains("auto-compact")
                            || content.contains("context has been reset")
                        {
                            _compact_summary_count += 1;
                            if let Some(ts_str) = v.get("timestamp").and_then(|s| s.as_str()) {
                                if let Ok(ts) = DateTime::parse_from_rfc3339(ts_str) {
                                    _last_compact_time = Some(ts.with_timezone(&Utc));
                                }
                            }
                        }
                    }
                }
                // detect reset time from API error line or other usage-limit messages with pipe+epoch
                if v.get("isApiErrorMessage").and_then(|b| b.as_bool()) == Some(true) {
                    if let Some(msg) = v.get("message") {
                        if let Some(content) = msg.get("content") {
                            if let Some(arr) = content.as_array() {
                                for c in arr {
                                    if let Some(text) = c.get("text").and_then(|s| s.as_str()) {
                                        if text.contains("Claude AI usage limit reached") {
                                            if let Some(idx) = text.rfind('|') {
                                                if let Ok(n) = text[idx + 1..].trim().parse::<i64>()
                                                {
                                                    if n > 0 {
                                                        let normalized_epoch =
                                                            normalize_reset_anchor(n);
                                                        if let Some(dt) =
                                                            DateTime::<Utc>::from_timestamp(
                                                                normalized_epoch,
                                                                0,
                                                            )
                                                        {
                                                            let dt = normalize_reset_time(dt);
                                                            if latest_reset
                                                                .map(|x| dt > x)
                                                                .unwrap_or(true)
                                                            {
                                                                latest_reset = Some(dt);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // broader detection: look for assistant/system text containing 'usage limit' and trailing | epoch
                    if let Some(content) = v
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        for c in content {
                            if let Some(text) = c.get("text").and_then(|s| s.as_str()) {
                                if text.to_lowercase().contains("usage limit") {
                                    if let Some(idx) = text.rfind('|') {
                                        if let Ok(n) = text[idx + 1..].trim().parse::<i64>() {
                                            if n > 0 {
                                                let normalized_epoch = normalize_reset_anchor(n);
                                                if let Some(dt) = DateTime::<Utc>::from_timestamp(
                                                    normalized_epoch,
                                                    0,
                                                ) {
                                                    let dt = normalize_reset_time(dt);
                                                    if latest_reset.map(|x| dt > x).unwrap_or(true)
                                                    {
                                                        latest_reset = Some(dt);
                                                    }
                                                }
                                            }
                                        }
                                    } else if let Some(ts_s) =
                                        v.get("timestamp").and_then(|s| s.as_str())
                                    {
                                        if let Ok(b) = DateTime::parse_from_rfc3339(ts_s)
                                            .map(|d| d.with_timezone(&Utc))
                                        {
                                            if let Some(dt) = parse_am_pm_reset(b, text) {
                                                if latest_reset.map(|x| dt > x).unwrap_or(true) {
                                                    latest_reset = Some(dt);
                                                }
                                            }
                                        }
                                    } else if let Some(dt) = parse_am_pm_reset(Utc::now(), text) {
                                        if latest_reset.map(|x| dt > x).unwrap_or(true) {
                                            latest_reset = Some(dt);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // usage line
                let ts = match v.get("timestamp").and_then(|s| s.as_str()) {
                    Some(s) => s,
                    None => continue,
                };
                let tsd = match DateTime::parse_from_rfc3339(ts).map(|d| d.with_timezone(&Utc)) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let msg = match v.get("message") {
                    Some(m) => m,
                    None => continue,
                };
                let usage = match msg.get("usage") {
                    Some(u) => u,
                    None => continue,
                };
                let input = usage
                    .get("input_tokens")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let output = usage
                    .get("output_tokens")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let cache_create = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let cache_read = usage
                    .get("cache_read_input_tokens")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let mut cost = 0.0f64;
                // Web search request charges
                let web_search_reqs = usage
                    .get("server_tool_use")
                    .and_then(|o| o.get("web_search_requests"))
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let model = msg
                    .get("model")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let service_tier = usage
                    .get("service_tier")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let speed = v
                    .get("speed")
                    .or_else(|| usage.get("speed"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let agent_id = v
                    .get("agentId")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                // Accept either key spelling for session identifier
                let sid = v
                    .get("sessionId")
                    .or_else(|| v.get("session_id"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let mid = msg
                    .get("id")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                let rid = v
                    .get("requestId")
                    .or_else(|| v.get("request_id"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                // Build an aggregation key: prefer requestId > message.id; else composite with session hint
                let composite = format!(
                    "{}|{}|{}|{}|{}|{}",
                    tsd.to_rfc3339(),
                    model.clone().unwrap_or_default(),
                    input,
                    output,
                    cache_create,
                    cache_read
                );
                let sid_hint = sid
                    .clone()
                    .or_else(|| mid.as_ref().and_then(|m| sid_by_mid.get(m).cloned()))
                    .or_else(|| rid.as_ref().and_then(|r| sid_by_rid.get(r).cloned()));
                let agg_key = if let Some(ref r) = rid {
                    format!("R:{}", r)
                } else if let Some(ref m) = mid {
                    format!("M:{}", m)
                } else {
                    format!("F:{}|{}", sid_hint.clone().unwrap_or_default(), composite)
                };
                // Remember id -> session mappings when available
                if let (Some(m), Some(s)) = (&mid, &sid) {
                    sid_by_mid.insert(m.clone(), s.clone());
                }
                if let (Some(r), Some(s)) = (&rid, &sid) {
                    sid_by_rid.insert(r.clone(), s.clone());
                }
                if let Some(ref mdl) = model {
                    cost = calculate_cost_for_usage(mdl, usage);
                }
                // Decide whether updates for this key are cumulative totals or per-chunk deltas
                let key_clone = agg_key.clone();
                let prev_raw = last_seen_raw.get(&key_clone).copied();
                let mut is_delta = *force_delta_mode.get(&key_clone).unwrap_or(&false);
                if let Some((pi, po, pcc, pcr)) = prev_raw {
                    // If any field is non-monotonic, switch to delta mode for this key
                    if input < pi || output < po || cache_create < pcc || cache_read < pcr {
                        force_delta_mode.insert(key_clone.clone(), true);
                        is_delta = true;
                    }
                }
                // Update last-seen raw snapshot
                if is_delta {
                    let (pi, po, pcc, pcr) = prev_raw.unwrap_or((0, 0, 0, 0));
                    last_seen_raw.insert(
                        key_clone.clone(),
                        (
                            pi + input,
                            po + output,
                            pcc + cache_create,
                            pcr + cache_read,
                        ),
                    );
                } else {
                    last_seen_raw
                        .insert(key_clone.clone(), (input, output, cache_create, cache_read));
                }

                // Merge into aggregate entry
                let e = aggregated.entry(agg_key).or_insert(Entry {
                    ts: tsd,
                    input,
                    output,
                    cache_create,
                    cache_read,
                    web_search_requests: web_search_reqs,
                    speed: speed.clone(),
                    service_tier: service_tier.clone(),
                    cost,
                    model: model.clone(),
                    session_id: sid.clone().or(sid_hint.clone()),
                    msg_id: mid.clone(),
                    req_id: rid.clone(),
                    project: proj_name.clone(),
                    agent_id: agent_id.clone(),
                });
                if is_delta {
                    // Sum deltas
                    if tsd > e.ts {
                        e.ts = tsd;
                    }
                    e.input = e.input.saturating_add(input);
                    e.output = e.output.saturating_add(output);
                    e.cache_create = e.cache_create.saturating_add(cache_create);
                    e.cache_read = e.cache_read.saturating_add(cache_read);
                    e.web_search_requests = e.web_search_requests.saturating_add(web_search_reqs);
                    e.cost += cost;
                    if e.service_tier.is_none() {
                        e.service_tier = service_tier.clone();
                    }
                    if e.model.is_none() {
                        e.model = model.clone();
                    }
                    if e.session_id.is_none() {
                        e.session_id = sid.clone().or(sid_hint.clone());
                    }
                    if e.project.is_none() {
                        e.project = proj_name.clone();
                    }
                } else {
                    // Treat as cumulative totals; keep maxima/latest
                    if tsd > e.ts {
                        e.ts = tsd;
                    }
                    if input > e.input {
                        e.input = input;
                    }
                    if output > e.output {
                        e.output = output;
                    }
                    if cache_create > e.cache_create {
                        e.cache_create = cache_create;
                    }
                    if cache_read > e.cache_read {
                        e.cache_read = cache_read;
                    }
                    if web_search_reqs > e.web_search_requests {
                        e.web_search_requests = web_search_reqs;
                    }
                    if cost > e.cost {
                        e.cost = cost;
                    }
                    if e.service_tier.is_none() {
                        e.service_tier = service_tier.clone();
                    }
                    if e.model.is_none() {
                        e.model = model.clone();
                    }
                    if e.session_id.is_none() {
                        e.session_id = sid.clone().or(sid_hint.clone());
                    }
                    if e.project.is_none() {
                        e.project = proj_name.clone();
                    }
                }
                if web_search_reqs > e.web_search_requests {
                    e.web_search_requests = web_search_reqs;
                }
                if e.service_tier.is_none() {
                    e.service_tier = service_tier.clone();
                }
                if e.model.is_none() {
                    e.model = model.clone();
                }
                if e.session_id.is_none() {
                    e.session_id = sid.clone().or(sid_hint);
                }
                if e.project.is_none() {
                    e.project = proj_name.clone();
                }
            }
        }
    }

    // Offline-only: never perform API probes. If a persisted reset exists in the future, use it.
    let now = Utc::now();
    let rl_info: Option<RateLimitInfo> = None;
    if latest_reset.is_none() {
        if let Some(state) = read_persisted_reset_state() {
            if let Some(reset_at) = state.reset_at {
                if reset_at > now {
                    latest_reset = Some(reset_at);
                }
            }
        }
    }
    // Finalize aggregated entries and compute totals
    let mut entries: Vec<Entry> = aggregated.into_values().collect();
    entries.sort_by_key(|e| e.ts);
    let mut session_today_cost = 0.0f64; // This session's cost for today only
    for e in &entries {
        let ts_s = e.ts.to_rfc3339();
        let is_today = if let Some(d) = parse_iso_date(&ts_s) {
            d == today
        } else {
            false
        };

        if e.session_id.as_deref() == Some(session_id) {
            session_cost += e.cost;
            if is_today {
                session_today_cost += e.cost; // Track this session's today cost
            }
        }

        if is_today {
            today_cost += e.cost; // Global today cost (all sessions)
        }
    }
    // Prefer result-derived session cost if present
    if session_cost_via_results > 0.0 {
        session_cost = session_cost_via_results;
    }

    // Cache the results before returning
    crate::cache::cache_usage(
        session_id,
        project_dir,
        entries.clone(),
        today_cost,
        latest_reset,
        api_key_source.clone(),
    );

    // Persist log-derived reset too so we don't need to re-probe until after expiry
    if let Some(dt) = latest_reset {
        let prev = read_persisted_reset_state();
        if prev
            .as_ref()
            .and_then(|p| p.reset_at)
            .map(|p| p < dt)
            .unwrap_or(true)
        {
            let prev_last_checked = prev.as_ref().and_then(|p| p.last_checked);
            let prev_status = prev.as_ref().and_then(|p| p.status.as_deref());
            let prev_fallback = prev.as_ref().and_then(|p| p.fallback.as_deref());
            write_persisted_reset_state(
                Some(dt),
                prev_last_checked,
                prev_status,
                prev_fallback,
                prev.as_ref().and_then(|p| p.rate_limit_type.as_deref()),
                prev.as_ref().and_then(|p| p.overage_status.as_deref()),
                prev.as_ref().and_then(|p| p.overage_resets_at),
                prev.as_ref().and_then(|p| p.fallback_percentage),
            );
        }
    }

    Ok((
        session_cost,
        session_today_cost,
        today_cost,
        entries,
        latest_reset,
        api_key_source,
        rl_info,
    ))
}

// Normalize reset anchor number into epoch seconds.
// Some providers emit an absolute epoch (e.g., 172xxxxxxx). Others may emit seconds-until-reset (e.g., 5400).
// Heuristic: treat values >= 1_000_000_000 as epoch seconds; otherwise as seconds-from-now.
fn normalize_reset_anchor(n: i64) -> i64 {
    let now = Utc::now().timestamp();
    if n >= 1_000_000_000 { n } else { now + n }
}

// Calculate message complexity weight based on token usage
// Pro plan limits are based on message complexity, not just count
pub fn calculate_message_weight(entry: &Entry) -> f64 {
    // Average Claude Code message is ~500 tokens
    const AVERAGE_MESSAGE_TOKENS: f64 = 500.0;

    // Total tokens for this message
    let total_tokens = (entry.input + entry.cache_create + entry.cache_read) as f64;

    if total_tokens > 0.0 {
        // Calculate weight relative to average
        let weight = total_tokens / AVERAGE_MESSAGE_TOKENS;

        // Cap at reasonable limits (0.1 to 5.0)
        weight.clamp(0.1, 5.0)
    } else {
        // Default to average weight if no token data
        1.0
    }
}

// Calculate session message complexity for accurate limit tracking
pub fn calculate_session_complexity(entries: &[Entry], session_id: &str) -> MessageComplexity {
    let session_entries: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.session_id.as_deref() == Some(session_id))
        .collect();

    let mut complexity = MessageComplexity::default();

    for entry in session_entries {
        let weight = calculate_message_weight(entry);
        complexity.total_weight += weight;
        complexity.message_count += 1;

        if weight < 0.5 {
            complexity.short_messages += 1;
        } else if weight > 2.0 {
            complexity.long_messages += 1;
        }
    }

    if complexity.message_count > 0 {
        complexity.average_weight = complexity.total_weight / complexity.message_count as f64;
    }

    complexity
}

// Detect rapid message exchange patterns for burn rate calculation
pub fn detect_rapid_exchange(
    entries: &[Entry],
    session_id: &str,
    window_minutes: i64,
) -> (bool, f64) {
    let now = Utc::now();
    let window_start = now - Duration::minutes(window_minutes);

    // Filter entries for this session within the window
    let mut session_entries: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.session_id.as_deref() == Some(session_id) && e.ts >= window_start)
        .collect();

    if session_entries.len() < 2 {
        return (false, 0.0);
    }

    session_entries.sort_by_key(|e| e.ts);

    // Calculate average time between messages
    let mut total_gap_minutes = 0.0;
    let mut gap_count = 0;

    for i in 1..session_entries.len() {
        let gap = session_entries[i].ts - session_entries[i - 1].ts;
        total_gap_minutes += gap.num_minutes() as f64;
        gap_count += 1;
    }

    if gap_count == 0 {
        return (false, 0.0);
    }

    let avg_gap_minutes = total_gap_minutes / gap_count as f64;

    // Rapid exchange if average gap is less than 5 minutes
    let is_rapid = avg_gap_minutes < 5.0;

    // Calculate burn rate (tokens per minute) for the window
    let total_tokens: u64 = session_entries
        .iter()
        .map(|e| e.input + e.output + e.cache_create + e.cache_read)
        .sum();
    let window_duration_minutes = (session_entries.last().unwrap().ts
        - session_entries.first().unwrap().ts)
        .num_minutes()
        .max(1) as f64;
    let burn_rate = total_tokens as f64 / window_duration_minutes;

    (is_rapid, burn_rate)
}

// --- Persisted reset state (on-disk), used only for log-derived anchors ---

#[derive(Debug, Clone)]
struct ResetState {
    reset_at: Option<chrono::DateTime<chrono::Utc>>,
    last_checked: Option<chrono::DateTime<chrono::Utc>>,
    status: Option<String>,
    fallback: Option<String>,
    rate_limit_type: Option<String>,
    overage_status: Option<String>,
    overage_resets_at: Option<chrono::DateTime<chrono::Utc>>,
    fallback_percentage: Option<f64>,
}

fn reset_state_path() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().join(".claude").join("statusline-reset.json"))
}

fn read_persisted_reset_state() -> Option<ResetState> {
    let p = reset_state_path()?;
    let txt = std::fs::read_to_string(&p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    let reset_at = v
        .get("reset_at")
        .and_then(|x| x.as_i64())
        .and_then(|e| chrono::DateTime::<chrono::Utc>::from_timestamp(e, 0))
        .map(normalize_reset_time);
    let last_checked = v
        .get("last_checked")
        .and_then(|x| x.as_i64())
        .and_then(|e| chrono::DateTime::<chrono::Utc>::from_timestamp(e, 0));
    let status = v
        .get("status")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let fallback = v
        .get("fallback")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let rate_limit_type = v
        .get("rate_limit_type")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let overage_status = v
        .get("overage_status")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let overage_resets_at = v
        .get("overage_resets_at")
        .and_then(|x| x.as_i64())
        .and_then(|e| chrono::DateTime::<chrono::Utc>::from_timestamp(e, 0))
        .map(normalize_reset_time);
    let fallback_percentage = v.get("fallback_percentage").and_then(|x| x.as_f64());
    Some(ResetState {
        reset_at,
        last_checked,
        status,
        fallback,
        rate_limit_type,
        overage_status,
        overage_resets_at,
        fallback_percentage,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_persisted_reset_state(
    reset_at: Option<chrono::DateTime<chrono::Utc>>,
    last_checked: Option<chrono::DateTime<chrono::Utc>>,
    status: Option<&str>,
    fallback: Option<&str>,
    rate_limit_type: Option<&str>,
    overage_status: Option<&str>,
    overage_resets_at: Option<chrono::DateTime<chrono::Utc>>,
    fallback_percentage: Option<f64>,
) {
    if let Some(p) = reset_state_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let obj = serde_json::json!({
            "reset_at": reset_at.map(|d| d.timestamp()),
            "last_checked": last_checked.map(|d| d.timestamp()),
            "status": status,
            "fallback": fallback,
            "rate_limit_type": rate_limit_type,
            "overage_status": overage_status,
            "overage_resets_at": overage_resets_at.map(|d| d.timestamp()),
            "fallback_percentage": fallback_percentage,
        });
        let _ = std::fs::write(p, obj.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    fn write_transcript_line(
        session_id: &str,
        line: serde_json::Value,
    ) -> Result<tempfile::TempDir> {
        let dir = tempdir()?;
        let project = dir.path().join("projects").join("project");
        fs::create_dir_all(&project)?;
        let transcript = project.join(format!("{}.jsonl", session_id));
        fs::write(&transcript, format!("{}\n", line))?;
        Ok(dir)
    }

    #[test]
    fn scan_usage_reads_nested_fast_speed_and_keeps_web_search_flat() -> Result<()> {
        let session_id = format!(
            "fast-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let line = json!({
            "type": "assistant",
            "sessionId": session_id,
            "timestamp": Local::now().to_rfc3339(),
            "message": {
                "role": "assistant",
                "id": "msg-fast",
                "model": "claude-opus-4-6",
                "usage": {
                    "input_tokens": 1_000_000,
                    "output_tokens": 1_000_000,
                    "cache_creation_input_tokens": 1_000_000,
                    "cache_read_input_tokens": 1_000_000,
                    "server_tool_use": { "web_search_requests": 2 },
                    "speed": "fast"
                }
            }
        });
        let dir = write_transcript_line(&session_id, line)?;
        let base = dir.path().to_path_buf();

        let (session_cost, session_today_cost, today_cost, entries, _, _, _) =
            scan_usage(&[base], &session_id, None, None)?;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].speed.as_deref(), Some("fast"));
        assert!((entries[0].cost - 220.52).abs() < 1e-10);
        assert!((session_cost - 220.52).abs() < 1e-10);
        assert!((session_today_cost - 220.52).abs() < 1e-10);
        assert!((today_cost - 220.52).abs() < 1e-10);
        Ok(())
    }

    #[test]
    fn scan_usage_includes_advisor_iteration_cost() -> Result<()> {
        let session_id = format!(
            "advisor-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let line = json!({
            "type": "assistant",
            "sessionId": session_id,
            "timestamp": Local::now().to_rfc3339(),
            "message": {
                "role": "assistant",
                "id": "msg-advisor",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 1_000_000,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "iterations": [
                        {
                            "type": "advisor_message",
                            "model": "claude-opus-4-6",
                            "input_tokens": 1_000_000,
                            "output_tokens": 0,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0,
                            "speed": "fast"
                        }
                    ]
                }
            }
        });
        let dir = write_transcript_line(&session_id, line)?;
        let base = dir.path().to_path_buf();

        let (session_cost, _, _, entries, _, _, _) = scan_usage(&[base], &session_id, None, None)?;

        assert_eq!(entries.len(), 1);
        assert!((entries[0].cost - 33.0).abs() < 1e-10);
        assert!((session_cost - 33.0).abs() < 1e-10);
        Ok(())
    }

    #[test]
    fn parse_session_state_detects_prompt_cache_ttl_buckets() -> Result<()> {
        let dir = tempdir()?;
        let transcript = dir.path().join("session.jsonl");
        let ts = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        let line = json!({
            "type": "assistant",
            "timestamp": ts.to_rfc3339(),
            "message": {
                "role": "assistant",
                "id": "msg-cache",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 3000,
                    "cache_read_input_tokens": 4000,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 1000,
                        "ephemeral_1h_input_tokens": 2000
                    }
                }
            }
        });
        fs::write(&transcript, format!("{}\n", line))?;

        let state = parse_session_state(&transcript);
        let prompt_cache = state.prompt_cache.expect("prompt cache activity");

        assert_eq!(prompt_cache.last_cache_write_at, Some(ts));
        assert_eq!(prompt_cache.last_cache_read_at, Some(ts));
        assert_eq!(prompt_cache.last_activity_at(), Some(ts));
        assert_eq!(prompt_cache.cache_write_input_tokens, 3000);
        assert_eq!(prompt_cache.cache_read_input_tokens, 4000);
        assert_eq!(prompt_cache.buckets.len(), 2);
        assert!(prompt_cache.buckets.iter().any(|bucket| {
            bucket.kind == PromptCacheBucketKind::FiveMinute
                && bucket.ttl_seconds == PROMPT_CACHE_5M_TTL_SECONDS
                && bucket.input_tokens == 1000
        }));
        assert!(prompt_cache.buckets.iter().any(|bucket| {
            bucket.kind == PromptCacheBucketKind::OneHour
                && bucket.ttl_seconds == PROMPT_CACHE_1H_TTL_SECONDS
                && bucket.input_tokens == 2000
        }));
        Ok(())
    }

    #[test]
    fn parse_session_state_keeps_split_cache_without_aggregate_total() -> Result<()> {
        let dir = tempdir()?;
        let transcript = dir.path().join("session.jsonl");
        let ts = Utc.with_ymd_and_hms(2026, 5, 1, 12, 5, 0).unwrap();
        let line = json!({
            "type": "assistant",
            "timestamp": ts.to_rfc3339(),
            "message": {
                "role": "assistant",
                "id": "msg-cache-split",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 1000,
                        "ephemeral_1h_input_tokens": 2000
                    }
                }
            }
        });
        fs::write(&transcript, format!("{}\n", line))?;

        let state = parse_session_state(&transcript);
        let prompt_cache = state.prompt_cache.expect("prompt cache activity");

        assert_eq!(prompt_cache.last_cache_write_at, Some(ts));
        assert_eq!(prompt_cache.last_cache_read_at, None);
        assert_eq!(prompt_cache.last_activity_at(), Some(ts));
        assert_eq!(prompt_cache.cache_write_input_tokens, 3000);
        assert_eq!(prompt_cache.buckets.len(), 2);
        assert!(
            prompt_cache
                .buckets
                .iter()
                .any(|bucket| bucket.kind == PromptCacheBucketKind::FiveMinute)
        );
        assert!(
            prompt_cache
                .buckets
                .iter()
                .any(|bucket| bucket.kind == PromptCacheBucketKind::OneHour)
        );
        Ok(())
    }

    #[test]
    fn parse_session_state_preserves_unknown_cache_remainder() -> Result<()> {
        let dir = tempdir()?;
        let transcript = dir.path().join("session.jsonl");
        let ts = Utc.with_ymd_and_hms(2026, 5, 1, 12, 10, 0).unwrap();
        let line = json!({
            "type": "assistant",
            "timestamp": ts.to_rfc3339(),
            "message": {
                "role": "assistant",
                "id": "msg-cache-remainder",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 3500,
                    "cache_read_input_tokens": 0,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 1000,
                        "ephemeral_1h_input_tokens": 2000
                    }
                }
            }
        });
        fs::write(&transcript, format!("{}\n", line))?;

        let state = parse_session_state(&transcript);
        let prompt_cache = state.prompt_cache.expect("prompt cache activity");

        assert_eq!(prompt_cache.cache_write_input_tokens, 3500);
        assert_eq!(prompt_cache.buckets.len(), 3);
        assert!(prompt_cache.buckets.iter().any(|bucket| {
            bucket.kind == PromptCacheBucketKind::Unknown && bucket.input_tokens == 500
        }));
        Ok(())
    }

    #[test]
    fn parse_session_state_separates_cache_write_from_later_read() -> Result<()> {
        let dir = tempdir()?;
        let transcript = dir.path().join("session.jsonl");
        let write_ts = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        let read_ts = Utc.with_ymd_and_hms(2026, 5, 1, 12, 2, 0).unwrap();
        let write_line = json!({
            "type": "assistant",
            "timestamp": write_ts.to_rfc3339(),
            "message": {
                "role": "assistant",
                "id": "msg-cache-write",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 2000,
                    "cache_read_input_tokens": 0,
                    "cache_creation": {
                        "ephemeral_1h_input_tokens": 2000
                    }
                }
            }
        });
        let read_line = json!({
            "type": "assistant",
            "timestamp": read_ts.to_rfc3339(),
            "message": {
                "role": "assistant",
                "id": "msg-cache-read",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 1800
                }
            }
        });
        fs::write(&transcript, format!("{}\n{}\n", write_line, read_line))?;

        let state = parse_session_state(&transcript);
        let prompt_cache = state.prompt_cache.expect("prompt cache activity");

        assert_eq!(prompt_cache.last_cache_write_at, Some(write_ts));
        assert_eq!(prompt_cache.last_cache_read_at, Some(read_ts));
        assert_eq!(prompt_cache.last_activity_at(), Some(read_ts));
        assert_eq!(prompt_cache.cache_write_input_tokens, 2000);
        assert_eq!(prompt_cache.cache_read_input_tokens, 1800);
        assert_eq!(
            prompt_cache
                .buckets
                .iter()
                .find(|bucket| bucket.kind == PromptCacheBucketKind::OneHour)
                .map(|bucket| bucket.created_at),
            Some(write_ts)
        );
        Ok(())
    }
}
