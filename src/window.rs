//! # Window Module
//!
//! Handles 5-hour window calculations for usage tracking

use crate::models::Entry;
use crate::utils::{sanitized_project_name, WINDOW_DURATION_HOURS, WINDOW_DURATION_SECONDS};
use chrono::{DateTime, Duration, Timelike, Utc};

/// Metrics calculated for a window period
#[derive(Debug, Clone)]
pub struct WindowMetrics {
    pub total_cost: f64,
    pub total_tokens: f64,
    pub noncache_tokens: f64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_cache_create: u64,
    pub tokens_cache_read: u64,
    pub web_search_requests: u64,
    pub service_tier: Option<String>,
    pub tpm: f64,
    pub tpm_indicator: f64,
    pub session_nc_tpm: f64,
    pub global_nc_tpm: f64,
    pub cost_per_hour: f64,
    pub remaining_minutes: f64,
    pub usage_percent: Option<f64>,
    pub projected_percent: Option<f64>,
}

/// Scope for window calculations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowScope {
    Global,
    Project,
}

/// Scope for burn rate calculations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnScope {
    Session,
    Global,
}

/// Calculate window metrics for the current 5-hour window
#[allow(clippy::too_many_arguments)]
pub fn calculate_window_metrics(
    entries: &[Entry],
    session_id: &str,
    project_dir: Option<&str>,
    now_utc: DateTime<Utc>,
    latest_reset: Option<DateTime<Utc>>,
    window_scope: WindowScope,
    burn_scope: BurnScope,
    plan_max: Option<f64>,
) -> WindowMetrics {
    // Calculate window start and end: prefer provider reset anchor; otherwise use
    // a heuristic active-block finder similar to ccstatusline/claude-powerline.
    let (start, end) = if latest_reset.is_some() {
        window_bounds(now_utc, latest_reset)
    } else if let Some((hs, he)) = heuristic_active_block_bounds(entries, now_utc) {
        (hs, he)
    } else {
        // Fallback: rolling 5-hour window ending now
        window_bounds(now_utc, None)
    };

    // Filter entries for the window
    let mut window_entries: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.ts >= start && e.ts < end)
        .collect();

    // Apply project scope filter if requested
    if let WindowScope::Project = window_scope {
        if let Some(pd) = project_dir {
            let proj = sanitized_project_name(pd);
            window_entries.retain(|e| e.project.as_deref() == Some(&proj));
        }
    }

    window_entries.sort_by_key(|e| e.ts);

    // Aggregate metrics
    let web_search_requests: u64 = window_entries.iter().map(|e| e.web_search_requests).sum();
    // pick the latest (most recent) non-None service tier in this window
    let service_tier: Option<String> = window_entries
        .iter()
        .rev()
        .find_map(|e| e.service_tier.clone());

    // Calculate token totals
    let mut tokens_input: u64 = 0;
    let mut tokens_output: u64 = 0;
    let mut tokens_cache_create: u64 = 0;
    let mut tokens_cache_read: u64 = 0;
    let mut total_cost: f64 = 0.0;

    for e in &window_entries {
        tokens_input += e.input;
        tokens_output += e.output;
        tokens_cache_create += e.cache_create;
        tokens_cache_read += e.cache_read;
        total_cost += e.cost;
    }

    let total_tokens =
        (tokens_input + tokens_output + tokens_cache_create + tokens_cache_read) as f64;
    let noncache_tokens = (tokens_input + tokens_output) as f64;

    // Calculate session-specific burn rate
    let mut session_input: u64 = 0;
    let mut session_output: u64 = 0;
    let mut session_first: Option<DateTime<Utc>> = None;
    let mut session_last: Option<DateTime<Utc>> = None;

    for e in &window_entries {
        if e.session_id.as_deref() == Some(session_id) {
            session_input += e.input;
            session_output += e.output;
            session_first = Some(session_first.unwrap_or(e.ts));
            session_last = Some(e.ts);
        }
    }

    // Calculate duration and rates
    let duration_minutes_global = if window_entries.len() >= 2 {
        match (window_entries.first(), window_entries.last()) {
            (Some(first), Some(last)) => ((last.ts - first.ts).num_seconds().max(0) as f64) / 60.0,
            _ => 0.0,
        }
    } else {
        0.0
    };

    let duration_minutes_session = match (session_first, session_last) {
        (Some(f), Some(l)) => ((l - f).num_seconds().max(0) as f64) / 60.0,
        _ => 0.0,
    };

    let tpm = if duration_minutes_global > 0.0 {
        total_tokens / duration_minutes_global
    } else {
        0.0
    };

    let global_nc_tpm = if duration_minutes_global > 0.0 {
        noncache_tokens / duration_minutes_global
    } else {
        0.0
    };

    let session_nc_tpm = if duration_minutes_session > 0.0 {
        (session_input as f64 + session_output as f64) / duration_minutes_session
    } else {
        0.0
    };

    let tpm_indicator = match burn_scope {
        BurnScope::Session => session_nc_tpm,
        BurnScope::Global => global_nc_tpm,
    };

    let cost_per_hour = if duration_minutes_global > 0.0 {
        (total_cost / duration_minutes_global) * 60.0
    } else {
        0.0
    };

    let remaining_minutes = ((end - now_utc).num_minutes()).max(0) as f64;
    // Usage percent should reflect plan caps which apply to non-cache tokens (input+output).
    // Project using global non-cache rate to best approximate account-level consumption.
    let projected_nc_tokens = noncache_tokens + global_nc_tpm * remaining_minutes;
    let usage_percent = plan_max.map(|pm| (noncache_tokens * 100.0 / pm).max(0.0));
    let projected_percent = plan_max.map(|pm| (projected_nc_tokens * 100.0 / pm).max(0.0));

    WindowMetrics {
        total_cost,
        total_tokens,
        noncache_tokens,
        tokens_input,
        tokens_output,
        tokens_cache_create,
        tokens_cache_read,
        web_search_requests,
        service_tier,
        tpm,
        tpm_indicator,
        session_nc_tpm,
        global_nc_tpm,
        cost_per_hour,
        remaining_minutes,
        usage_percent,
        projected_percent,
    }
}

/// Compute the active 5-hour window [start, end).
/// - If a provider reset anchor is known, align windows to it.
/// - Otherwise, align to local 5-hour buckets starting at 00:00 local time.
pub fn window_bounds(
    now_utc: chrono::DateTime<chrono::Utc>,
    latest_reset: Option<chrono::DateTime<chrono::Utc>>,
) -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    if let Some(r) = latest_reset {
        let delta = now_utc - r;
        let k = (delta.num_seconds() / WINDOW_DURATION_SECONDS).max(0);
        let start = r + chrono::TimeDelta::seconds(k * WINDOW_DURATION_SECONDS);
        let end = start + chrono::TimeDelta::hours(WINDOW_DURATION_HOURS);
        (start, end)
    } else {
        // Fallback: rolling 5-hour window ending at 'now'
        let start = now_utc - chrono::TimeDelta::hours(WINDOW_DURATION_HOURS);
        (start, now_utc)
    }
}

/// Floor a timestamp to the beginning of the hour
fn floor_to_hour(ts: DateTime<Utc>) -> DateTime<Utc> {
    ts.with_minute(0)
        .and_then(|d| d.with_second(0))
        .and_then(|d| d.with_nanosecond(0))
        .unwrap_or(ts)
}

/// Find session boundaries by detecting gaps in activity
#[allow(dead_code)]
fn find_session_boundaries(entries: &[Entry], gap_threshold: Duration) -> Vec<DateTime<Utc>> {
    let mut boundaries = Vec::new();
    let mut sorted_entries = entries.to_vec();
    sorted_entries.sort_by_key(|e| e.ts);
    
    for i in 1..sorted_entries.len() {
        let gap = sorted_entries[i].ts - sorted_entries[i - 1].ts;
        if gap >= gap_threshold {
            boundaries.push(sorted_entries[i].ts);
        }
    }
    boundaries
}

/// Progressive lookback to find active block with optimization
fn progressive_lookback_block(
    entries: &[Entry],
    now_utc: DateTime<Utc>,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    if entries.is_empty() {
        return None;
    }
    
    let session_duration = Duration::hours(WINDOW_DURATION_HOURS as i64);
    let lookback_windows = [
        Duration::hours(10),  // 2x session duration - catches most cases
        Duration::hours(24),  // Full day for longer sessions
        Duration::hours(48),  // Extended sessions
    ];
    
    for lookback in &lookback_windows {
        let cutoff = now_utc - *lookback;
        let recent_entries: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.ts >= cutoff)
            .collect();
        
        if recent_entries.is_empty() {
            continue;
        }
        
        // Sort timestamps
        let mut timestamps: Vec<DateTime<Utc>> = recent_entries.iter().map(|e| e.ts).collect();
        timestamps.sort_unstable();
        
        // Find the most recent continuous work session
        let mut continuous_start = *timestamps.last()?;
        for i in (1..timestamps.len()).rev() {
            let gap = timestamps[i] - timestamps[i - 1];
            if gap >= session_duration {
                // Found a session boundary
                continuous_start = timestamps[i];
                break;
            }
            continuous_start = timestamps[i - 1];
        }
        
        // Floor to hour for cleaner boundaries
        let floored_start = floor_to_hour(continuous_start);
        
        // Calculate how long we've been working from the floored start
        let total_work_time = now_utc - floored_start;
        
        // If we've been working for more than one session, find the current block
        let block_start = if total_work_time > session_duration {
            let completed_blocks = (total_work_time.num_seconds() / session_duration.num_seconds()) as i64;
            floored_start + Duration::seconds(completed_blocks * session_duration.num_seconds())
        } else {
            floored_start
        };
        
        let block_end = block_start + session_duration;
        
        // Check if block is still active (activity within session duration)
        if let Some(last_ts) = timestamps.last() {
            if now_utc - *last_ts <= session_duration {
                return Some((block_start, block_end));
            }
        }
    }
    
    None
}

/// Heuristic active block bounds when no provider reset anchor is known.
/// Uses progressive lookback with gap detection for accurate session boundaries.
fn heuristic_active_block_bounds(
    entries: &[Entry],
    now_utc: DateTime<Utc>,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    // Try progressive lookback first for better accuracy
    if let Some(bounds) = progressive_lookback_block(entries, now_utc) {
        return Some(bounds);
    }
    
    // Fallback to original implementation if progressive fails
    if entries.is_empty() {
        return None;
    }
    let mut ts: Vec<DateTime<Utc>> = entries.iter().map(|e| e.ts).collect();
    ts.sort_unstable();
    let session_dur = chrono::TimeDelta::hours(WINDOW_DURATION_HOURS);
    
    // Start from the newest and walk backward until a gap >= session duration
    let mut earliest = *ts.last()?;
    for w in ts.windows(2).rev() {
        let prev = w[0];
        let next = w[1];
        let gap = next - prev;
        if gap >= session_dur {
            // Boundary found; earliest is next segment start
            break;
        }
        earliest = prev;
    }
    
    // Floor to the hour in UTC
    let floored = floor_to_hour(earliest);
    let end = floored + chrono::TimeDelta::hours(WINDOW_DURATION_HOURS);
    
    // If the latest activity is older than a window, consider no active block
    if now_utc - *ts.last().unwrap() > session_dur {
        // Still return bounds to compute historical block metrics; caller will compute remaining=0
        return Some((floored, end));
    }
    Some((floored, end))
}
