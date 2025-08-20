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
use chrono::{DateTime, Local, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::models::{Entry, MessageUsage, TranscriptLine};
use crate::pricing::pricing_for_model;
use crate::utils::{context_limit_for_model_display, parse_iso_date, sanitized_project_name};

pub fn calc_context_from_transcript(
    transcript_path: &Path,
    model_id: &str,
    model_display_name: &str,
) -> Option<(u64, u32)> {
    // Stream the file line-by-line to avoid loading entire transcripts into memory.
    // Keep the last assistant message with usage; sum input + cache* only (exclude output from context window).
    let file = File::open(transcript_path).ok()?;
    let reader = BufReader::new(file);
    let mut last_total_in: Option<u64> = None;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let parsed: TranscriptLine = match serde_json::from_str(t) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if parsed.r#type.as_deref() != Some("assistant") {
            continue;
        }
        let usage = parsed
            .message
            .and_then(|m| m.usage)
            .unwrap_or(MessageUsage {
                input_tokens: None,
                output_tokens: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
        if let Some(inp) = usage.input_tokens {
            let total_in = inp
                + usage.cache_creation_input_tokens.unwrap_or(0)
                + usage.cache_read_input_tokens.unwrap_or(0);
            last_total_in = Some(total_in);
        }
    }

    if let Some(total_in) = last_total_in {
        let context_limit = context_limit_for_model_display(model_id, model_display_name);
        let pct = ((total_in as f64 / context_limit as f64) * 100.0).round() as u32;
        Some((total_in, pct.min(100)))
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
    // Exclude output; context is input + cache tokens only
    let total_in = last.input + last.cache_create + last.cache_read;
    let limit = context_limit_for_model_display(model_id, model_display_name);
    let pct = ((total_in as f64 / limit as f64) * 100.0).round() as u32;
    Some((total_in, pct.min(100)))
}

// Fallback: compute context from the most recent entry regardless of session.
pub fn calc_context_from_any(
    entries: &[Entry],
    model_id: &str,
    model_display_name: &str,
) -> Option<(u64, u32)> {
    if entries.is_empty() {
        return None;
    }
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.ts);
    let last = sorted.last()?;
    // Exclude output; context is input + cache tokens only
    let total_in = last.input + last.cache_create + last.cache_read;
    let limit = context_limit_for_model_display(model_id, model_display_name);
    let pct = ((total_in as f64 / limit as f64) * 100.0).round() as u32;
    Some((total_in, pct.min(100)))
}

#[allow(clippy::type_complexity)]
pub fn scan_usage(
    paths: &[PathBuf],
    session_id: &str,
    project_dir: Option<&str>,
) -> Result<(
    f64, /*session*/
    f64, /*today*/
    Vec<Entry>,
    Option<DateTime<Utc>>,
    Option<String>,
)> {
    let today = Local::now().date_naive();
    let mut session_cost = 0.0f64;
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
                                                    if latest_reset.map(|x| dt > x).unwrap_or(true)
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
                if let (Some(ref m), Some(ref s)) = (&mid, &sid) {
                    sid_by_mid.insert(m.clone(), s.clone());
                }
                if let (Some(ref r), Some(ref s)) = (&rid, &sid) {
                    sid_by_rid.insert(r.clone(), s.clone());
                }
                if cost == 0.0 {
                    if let Some(ref mdl) = model {
                        if let Some(mut p) = pricing_for_model(mdl) {
                            // Large-uncached-input bump for sonnet/haiku-style pricing when total input > 200k
                            let total_in = input + cache_create + cache_read;
                            let no_explicit_env = std::env::var("CLAUDE_PRICE_INPUT").is_err()
                                || std::env::var("CLAUDE_PRICE_OUTPUT").is_err()
                                || std::env::var("CLAUDE_PRICE_CACHE_CREATE").is_err()
                                || std::env::var("CLAUDE_PRICE_CACHE_READ").is_err();
                            let mdl_l = mdl.to_lowercase();
                            // Optional tiered pricing bump, disabled by default.
                            // Enable only if CLAUDE_ENABLE_TIERED_PRICING=1
                            if std::env::var("CLAUDE_ENABLE_TIERED_PRICING")
                                .ok()
                                .as_deref()
                                == Some("1")
                                && no_explicit_env
                                && total_in > 200_000
                                && (mdl_l.contains("sonnet") || mdl_l.contains("haiku"))
                            {
                                // Bump rates to a high-volume tier when above threshold
                                p.in_per_tok = 6e-6;
                                p.out_per_tok = 22.5e-6;
                                p.cache_create_per_tok = 7.5e-6;
                                p.cache_read_per_tok = 0.6e-6;
                            }
                            // Fallback: include cache + web search pricing
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
    // Finalize aggregated entries and compute totals
    let mut entries: Vec<Entry> = aggregated.into_values().collect();
    entries.sort_by_key(|e| e.ts);
    for e in &entries {
        if e.session_id.as_deref() == Some(session_id) {
            session_cost += e.cost;
        }
        let ts_s = e.ts.to_rfc3339();
        if let Some(d) = parse_iso_date(&ts_s) {
            if d == today {
                today_cost += e.cost;
            }
        }
    }
    Ok((
        session_cost,
        today_cost,
        entries,
        latest_reset,
        api_key_source,
    ))
}

// Normalize reset anchor number into epoch seconds.
// Some providers emit an absolute epoch (e.g., 172xxxxxxx). Others may emit seconds-until-reset (e.g., 5400).
// Heuristic: treat values >= 1_000_000_000 as epoch seconds; otherwise as seconds-from-now.
fn normalize_reset_anchor(n: i64) -> i64 {
    let now = Utc::now().timestamp();
    if n >= 1_000_000_000 {
        n
    } else {
        now + n
    }
}
