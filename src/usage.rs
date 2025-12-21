//! # Usage Module
//!
//! Tracks and analyzes Claude Code session usage data from transcript files and usage logs.
//!
//! ## Key Functions
//!
//! - `scan_usage`: Scans Claude config directories for usage JSONL files
//! - `identify_blocks`: Groups usage entries into 5-hour window blocks with gap detection
//! - `calc_context_from_*`: Calculates context window usage from various sources

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::models::{Entry, MessageUsage, RateLimitInfo, TranscriptLine};
use crate::pricing::{apply_tiered_pricing, pricing_for_model};
use crate::utils::{
    context_limit_for_model_display, parse_iso_date, sanitized_project_name, system_overhead_tokens,
};

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

    for base in paths {
        let root = base.join("projects");
        if !root.is_dir() {
            continue;
        }
        // Global reset anchor discovery across all project files under this root
        for entry in globwalk::GlobWalkerBuilder::from_patterns(&root, &["**/*.jsonl"])
            .build()
            .context("glob")?
        {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path().to_path_buf();

            // File mtime optimization: skip old files
            if let Ok(metadata) = fs::metadata(&path) {
                if let Ok(modified) = metadata.modified() {
                    let mtime: DateTime<Utc> = modified.into();
                    if mtime < cutoff_time {
                        continue; // Skip old files
                    }
                }
            }

            let file = match File::open(&path) {
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
        // Build a GLOBAL candidate set: include current project’s files (if derivable) AND all jsonl files under the root.
        // This ensures window usage reflects account-wide activity, not just the current project.
        use std::collections::HashSet;
        let mut candidate_set: HashSet<PathBuf> = HashSet::new();
        if let Some(pd) = project_dir {
            let sanitized = sanitized_project_name(pd);
            let proj_dir = root.join(sanitized);
            let session_path = proj_dir.join(format!("{}.jsonl", session_id));
            if session_path.is_file() {
                candidate_set.insert(session_path);
            }
            if proj_dir.is_dir() {
                for e in globwalk::GlobWalkerBuilder::from_patterns(&proj_dir, &["**/*.jsonl"])
                    .build()
                    .context("glob")?
                    .flatten()
                {
                    candidate_set.insert(e.path().to_path_buf());
                }
            }
        }
        for e in globwalk::GlobWalkerBuilder::from_patterns(&root, &["**/*.jsonl"])
            .build()
            .context("glob")?
            .flatten()
        {
            candidate_set.insert(e.path().to_path_buf());
        }
        let candidate_files: Vec<PathBuf> = candidate_set.into_iter().collect();

        for path in candidate_files {
            let file = match File::open(&path) {
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
                // Skip sidechain entries to avoid double counting non-main chain activity
                if v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true) {
                    continue;
                }

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
                let mut cost = v.get("costUSD").and_then(|n| n.as_f64()).unwrap_or(0.0);
                // Include web search request charges if present when we compute fallback cost
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
                if cost == 0.0 {
                    if let Some(ref mdl) = model {
                        if let Some(base_p) = pricing_for_model(mdl) {
                            // Apply tiered pricing if applicable (e.g., long-context pricing)
                            let total_in = input + cache_create + cache_read;
                            let p = apply_tiered_pricing(base_p, mdl, total_in);

                            // Calculate cost with applied pricing
                            cost = (input as f64) * p.in_per_tok
                                + (output as f64) * p.out_per_tok
                                + (cache_create as f64) * p.cache_create_per_tok
                                + (cache_read as f64) * p.cache_read_per_tok
                                + (web_search_reqs as f64) * 0.01; // per-request charge
                        }
                    }
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
                    service_tier: service_tier.clone(),
                    cost,
                    model: model.clone(),
                    session_id: sid.clone().or(sid_hint.clone()),
                    msg_id: mid.clone(),
                    req_id: rid.clone(),
                    project: proj_name.clone(),
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
