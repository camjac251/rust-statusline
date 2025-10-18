//! # Window Module
//!
//! Handles 5-hour window calculations for usage tracking

use crate::models::Entry;
use crate::usage::{calculate_session_complexity, detect_rapid_exchange};
use crate::utils::{sanitized_project_name, WINDOW_DURATION_HOURS, WINDOW_DURATION_SECONDS};
use chrono::{DateTime, Duration, Local, Timelike, Utc};

// Session reset hours in local time: 1am, 7am, 1pm, 7pm (from JavaScript implementation)
// These align with Claude's actual reset schedule
pub const RESET_HOURS: [u32; 4] = [1, 7, 13, 19];

// Calculate the next reset time based on fixed reset hours
pub fn calculate_next_reset(now: DateTime<Utc>) -> DateTime<Utc> {
    let local_now = now.with_timezone(&Local);
    let current_hour = local_now.hour();

    // Find the next reset hour
    let next_reset_hour = RESET_HOURS
        .iter()
        .find(|&&h| h > current_hour)
        .copied()
        .unwrap_or(RESET_HOURS[0]); // Wrap to first reset hour of next day

    // Calculate the reset time
    let reset_time = if next_reset_hour > current_hour {
        // Reset is later today
        local_now
            .with_hour(next_reset_hour)
            .unwrap()
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
    } else {
        // Reset is tomorrow
        (local_now + Duration::days(1))
            .with_hour(next_reset_hour)
            .unwrap()
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
    };

    reset_time.with_timezone(&Utc)
}

// Calculate the previous reset time based on fixed reset hours
pub fn calculate_previous_reset(now: DateTime<Utc>) -> DateTime<Utc> {
    let local_now = now.with_timezone(&Local);
    let current_hour = local_now.hour();

    // Find the previous reset hour
    let prev_reset_hour = RESET_HOURS
        .iter()
        .rev()
        .find(|&&h| h <= current_hour)
        .copied()
        .unwrap_or(RESET_HOURS[3]); // Wrap to last reset hour of previous day

    // Calculate the reset time
    let reset_time = if prev_reset_hour <= current_hour {
        // Reset was earlier today
        local_now
            .with_hour(prev_reset_hour)
            .unwrap()
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
    } else {
        // Reset was yesterday
        (local_now - Duration::days(1))
            .with_hour(prev_reset_hour)
            .unwrap()
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
    };

    reset_time.with_timezone(&Utc)
}

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
    // Session-scoped token breakdown within the current window
    pub session_tokens_input: u64,
    pub session_tokens_output: u64,
    pub session_tokens_cache_create: u64,
    pub session_tokens_cache_read: u64,
    pub web_search_requests: u64,
    pub service_tier: Option<String>,
    pub tpm: f64,
    pub tpm_indicator: f64,
    pub session_nc_tpm: f64,
    pub global_nc_tpm: f64,
    pub cost_per_hour: f64,
    pub remaining_minutes: f64,
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
) -> WindowMetrics {
    // Calculate window start and end: prefer provider reset anchor; otherwise use
    // a heuristic active-block finder similar to ccstatusline/claude-powerline.
    let ignore_anchor = match std::env::var("CLAUDE_WINDOW_ANCHOR") {
        Ok(v) => {
            let v = v.to_lowercase();
            v == "log" || v == "heuristic" || v == "none"
        }
        Err(_) => false,
    };

    let (start, end) = if latest_reset.is_some() && !ignore_anchor {
        window_bounds(now_utc, latest_reset)
    } else if let Some((hs, he)) = heuristic_active_block_bounds(entries, now_utc) {
        (hs, he)
    } else {
        // Fallback: rolling 5-hour window ending now
        window_bounds(now_utc, None)
    };

    // Global (account-wide) entries for the window — always used for block totals and percent
    let mut global_entries: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.ts >= start && e.ts < end)
        .collect();
    global_entries.sort_by_key(|e| e.ts);

    // Optional project-scoped view retained only for display/burn by scope
    let mut window_entries: Vec<&Entry> = global_entries.clone();
    if let WindowScope::Project = window_scope {
        if let Some(pd) = project_dir {
            let proj = sanitized_project_name(pd);
            window_entries.retain(|e| e.project.as_deref() == Some(&proj));
        }
    }

    // Aggregate global metrics for block usage (account-wide)
    let web_search_requests: u64 = global_entries.iter().map(|e| e.web_search_requests).sum();
    let service_tier: Option<String> = global_entries
        .iter()
        .rev()
        .find_map(|e| e.service_tier.clone());

    let mut tokens_input: u64 = 0;
    let mut tokens_output: u64 = 0;
    let mut tokens_cache_create: u64 = 0;
    let mut tokens_cache_read: u64 = 0;
    let mut total_cost: f64 = 0.0;
    for e in &global_entries {
        tokens_input += e.input;
        tokens_output += e.output;
        tokens_cache_create += e.cache_create;
        tokens_cache_read += e.cache_read;
        total_cost += e.cost;
    }
    // Cost is already computed per entry in usage.rs (including web_search when recomputed);
    // do not add web_search again here to avoid double counting.

    if matches!(window_scope, WindowScope::Project) {
        tokens_input = window_entries.iter().map(|e| e.input).sum();
        tokens_output = window_entries.iter().map(|e| e.output).sum();
        tokens_cache_create = window_entries.iter().map(|e| e.cache_create).sum();
        tokens_cache_read = window_entries.iter().map(|e| e.cache_read).sum();
        total_cost = window_entries.iter().map(|e| e.cost).sum();
    }

    let total_tokens =
        (tokens_input + tokens_output + tokens_cache_create + tokens_cache_read) as f64;
    let noncache_tokens = (tokens_input + tokens_output) as f64;

    // Calculate session-specific burn rate
    let mut session_input: u64 = 0;
    let mut session_output: u64 = 0;
    let mut session_cache_create: u64 = 0;
    let mut session_cache_read: u64 = 0;
    let mut session_first: Option<DateTime<Utc>> = None;
    let mut session_last: Option<DateTime<Utc>> = None;

    for e in &window_entries {
        if e.session_id.as_deref() == Some(session_id) {
            session_input += e.input;
            session_output += e.output;
            session_cache_create += e.cache_create;
            session_cache_read += e.cache_read;
            session_first = Some(session_first.unwrap_or(e.ts));
            session_last = Some(e.ts);
        }
    }

    // Calculate duration and rates
    // Global duration/burn (account-wide)
    let duration_minutes_global = if global_entries.len() >= 2 {
        match (global_entries.first(), global_entries.last()) {
            (Some(first), Some(last)) => ((last.ts - first.ts).num_seconds().max(60) as f64) / 60.0,
            _ => 0.0,
        }
    } else {
        0.0
    };

    let duration_minutes_session = match (session_first, session_last) {
        (Some(f), Some(l)) => ((l - f).num_seconds().max(60) as f64) / 60.0,
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

    // Enhanced burn rate with rapid exchange detection (from JavaScript implementation)
    let (is_rapid, enhanced_burn_rate) = detect_rapid_exchange(entries, session_id, 15);

    // Adjust burn rate if rapid exchange detected (indicates active development)
    let adjusted_session_tpm = if is_rapid && enhanced_burn_rate > session_nc_tpm {
        enhanced_burn_rate
    } else {
        session_nc_tpm
    };

    // Calculate message complexity for more accurate session limits
    let complexity = calculate_session_complexity(entries, session_id);

    // Adjust tpm based on message complexity (heavier messages burn faster)
    let complexity_adjusted_tpm = adjusted_session_tpm * complexity.average_weight;

    // Recent activity (last 30 minutes) to smooth projections and burn indicators
    let recent_cutoff = now_utc - chrono::TimeDelta::minutes(30);
    let mut recent_input: u64 = 0;
    let mut recent_output: u64 = 0;
    let mut recent_first: Option<DateTime<Utc>> = None;
    let mut recent_last: Option<DateTime<Utc>> = None;
    for e in &window_entries {
        if e.ts >= recent_cutoff {
            recent_input += e.input;
            recent_output += e.output;
            recent_first = Some(match recent_first {
                Some(existing) => existing.min(e.ts),
                None => e.ts,
            });
            recent_last = Some(match recent_last {
                Some(existing) => existing.max(e.ts),
                None => e.ts,
            });
        }
    }
    let recent_duration_minutes = match (recent_first, recent_last) {
        (Some(first), Some(last)) if last > first => {
            ((last - first).num_seconds().max(60) as f64) / 60.0
        }
        _ => 0.0,
    };
    let recent_nc_tpm = if recent_duration_minutes >= 1.0 {
        (recent_input as f64 + recent_output as f64) / recent_duration_minutes
    } else {
        0.0
    };

    let blended_nc_tpm = if recent_nc_tpm > 0.0 && global_nc_tpm > 0.0 {
        let weight = (recent_duration_minutes / 30.0).clamp(0.0, 1.0);
        let candidate = weight * recent_nc_tpm + (1.0 - weight) * global_nc_tpm;
        let max_factor = 3.0;
        candidate
            .min(global_nc_tpm * max_factor)
            .max(global_nc_tpm / max_factor)
    } else if recent_nc_tpm > 0.0 {
        recent_nc_tpm
    } else {
        global_nc_tpm
    };

    let cost_per_hour = if duration_minutes_global > 0.0 {
        (total_cost / duration_minutes_global) * 60.0
    } else {
        0.0
    };

    let remaining_minutes = ((end - now_utc).num_minutes()).max(0) as f64;
    // Usage percent reflects account-level (global) consumption against cap.
    let tpm_indicator = match burn_scope {
        BurnScope::Session => complexity_adjusted_tpm,
        BurnScope::Global => blended_nc_tpm,
    };

    WindowMetrics {
        total_cost,
        total_tokens,
        noncache_tokens,
        tokens_input,
        tokens_output,
        tokens_cache_create,
        tokens_cache_read,
        session_tokens_input: session_input,
        session_tokens_output: session_output,
        session_tokens_cache_create: session_cache_create,
        session_tokens_cache_read: session_cache_read,
        web_search_requests,
        service_tier,
        tpm,
        tpm_indicator,
        session_nc_tpm,
        global_nc_tpm,
        cost_per_hour,
        remaining_minutes,
    }
}

/// Compute the active 5-hour window [start, end).
/// - If a provider reset anchor is known, align windows to it.
/// - Otherwise, use fixed reset hours [1,7,13,19] in local time.
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
        // Fallback: Use fixed reset hours [1,7,13,19] in local time
        let prev_reset = calculate_previous_reset(now_utc);
        let next_reset = calculate_next_reset(now_utc);

        // Current window is from previous reset to next reset
        // Note: Windows may not be exactly 5 hours due to reset schedule
        (prev_reset, next_reset)
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
        Duration::hours(10), // 2x session duration - catches most cases
        Duration::hours(24), // Full day for longer sessions
        Duration::hours(48), // Extended sessions
    ];

    for lookback in &lookback_windows {
        let cutoff = now_utc - *lookback;
        let recent_entries: Vec<&Entry> = entries.iter().filter(|e| e.ts >= cutoff).collect();

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
            let completed_blocks =
                (total_work_time.num_seconds() / session_duration.num_seconds()) as i64;
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
    if entries.is_empty() {
        return None;
    }

    // Sort entries by timestamp
    let mut sorted_entries = entries.to_vec();
    sorted_entries.sort_by_key(|e| e.ts);

    let session_duration = chrono::TimeDelta::hours(WINDOW_DURATION_HOURS);
    let session_duration_ms = session_duration.num_milliseconds();

    // Identify session blocks using the same algorithm as claude-powerline-rust
    let mut blocks: Vec<Vec<DateTime<Utc>>> = Vec::new();
    let mut current_block: Vec<DateTime<Utc>> = Vec::new();
    let mut current_block_start: Option<DateTime<Utc>> = None;

    for entry in &sorted_entries {
        let entry_time = entry.ts;

        match current_block_start {
            None => {
                // Start first block - floor to the hour
                current_block_start = Some(floor_to_hour(entry_time));
                current_block.push(entry_time);
            }
            Some(block_start) => {
                let time_since_block_start = (entry_time - block_start).num_milliseconds();
                let time_since_last_entry = if let Some(last) = current_block.last() {
                    (entry_time - *last).num_milliseconds()
                } else {
                    0
                };

                // New block starts if: time since block start > 5 hours OR time since last entry > 5 hours
                if time_since_block_start > session_duration_ms
                    || time_since_last_entry > session_duration_ms
                {
                    // Finalize current block
                    if !current_block.is_empty() {
                        blocks.push(current_block.clone());
                    }

                    // Start new block
                    current_block_start = Some(floor_to_hour(entry_time));
                    current_block = vec![entry_time];
                } else {
                    current_block.push(entry_time);
                }
            }
        }
    }

    // Don't forget the last block
    if !current_block.is_empty() {
        blocks.push(current_block);
    }

    // Find the active block (most recent that's still within session duration)
    for block in blocks.iter().rev() {
        if let (Some(first), Some(last)) = (block.first(), block.last()) {
            let block_start = floor_to_hour(*first);
            let block_end = block_start + session_duration;

            // Check if block is active: current time within 5 hours of last entry AND before theoretical end
            let time_since_last = now_utc - *last;
            if time_since_last < session_duration && now_utc < block_end {
                return Some((block_start, block_end));
            }
        }
    }

    // Fallback: Use progressive lookback if the block-based approach fails
    if let Some(bounds) = progressive_lookback_block(entries, now_utc) {
        return Some(bounds);
    }

    // Last resort: rolling 5-hour window ending at 'now'
    let start = now_utc - session_duration;
    Some((start, now_utc))
}
