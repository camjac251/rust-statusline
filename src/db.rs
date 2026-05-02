//! SQLite-based persistent caching for global usage tracking across multiple
//! concurrent Claude Code sessions.
//!
//! This module provides:
//! - SQLite database initialization with schema versioning
//! - Session usage caching with mtime-based invalidation
//! - Global usage aggregation across all active sessions
//! - Concurrent access support via WAL mode

use anyhow::{Context, Result};
use chrono::{Local, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const USAGE_CACHE_VERSION: &str = "2";

/// Global usage result containing both session-specific and global costs
#[derive(Debug, Clone)]
pub struct GlobalUsage {
    /// Cost for the current session only
    pub session_cost: f64,
    /// Total cost across all sessions for today
    pub global_today: f64,
    /// Number of sessions contributing to global total
    pub sessions_count: usize,
}

/// Metadata value with optional timestamp
#[derive(Debug, Clone)]
pub struct MetadataEntry {
    pub value: String,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DbHealth {
    pub path: String,
    pub exists: bool,
    pub parent_exists: bool,
    pub writable: bool,
    pub journal_mode: Option<String>,
    pub schema_version: Option<String>,
    pub usage_cache_version: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
}

/// Get the database file path
///
/// Checks `CLAUDE_STATUSLINE_DB_PATH` environment variable first,
/// falls back to `~/.claude/statusline.db`
fn get_db_path() -> Result<PathBuf> {
    if let Ok(custom_path) = env::var("CLAUDE_STATUSLINE_DB_PATH") {
        return Ok(PathBuf::from(custom_path));
    }

    let base_dirs = directories::BaseDirs::new().context("Failed to find home directory")?;
    let home_dir = base_dirs.home_dir();
    let claude_dir = home_dir.join(".claude");

    if !claude_dir.exists() {
        fs::create_dir_all(&claude_dir)?;
    }

    Ok(claude_dir.join("statusline.db"))
}

/// Open database connection with WAL mode and retry logic
///
/// Implements retry logic for "database locked" errors with exponential backoff.
/// Configures WAL mode for concurrent access and sets busy timeout.
fn open_db() -> Result<Connection> {
    let db_path = get_db_path()?;

    let mut attempts = 0;
    let max_attempts = 3;

    loop {
        match Connection::open(&db_path) {
            Ok(conn) => {
                conn.pragma_update(None, "journal_mode", "WAL")?;
                conn.pragma_update(None, "busy_timeout", 5000)?;
                init_schema(&conn)?;
                return Ok(conn);
            }
            Err(e) if e.to_string().contains("locked") && attempts < max_attempts => {
                attempts += 1;
                thread::sleep(Duration::from_millis(100 * attempts));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

pub fn inspect_health() -> DbHealth {
    let path = match get_db_path() {
        Ok(path) => path,
        Err(err) => {
            return DbHealth {
                path: String::new(),
                exists: false,
                parent_exists: false,
                writable: false,
                journal_mode: None,
                schema_version: None,
                usage_cache_version: None,
                ok: false,
                error: Some(err.to_string()),
            };
        }
    };
    let exists = path.exists();
    let parent_exists = path.parent().is_some_and(|parent| parent.is_dir());

    match open_db() {
        Ok(conn) => {
            let exists = path.exists();
            let parent_exists = path.parent().is_some_and(|parent| parent.is_dir());
            let journal_mode = conn
                .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
                .ok();
            let schema_version = get_metadata(&conn, "schema_version")
                .ok()
                .flatten()
                .map(|entry| entry.value);
            let usage_cache_version = get_metadata(&conn, "usage_cache_version")
                .ok()
                .flatten()
                .map(|entry| entry.value);
            let writable = conn
                .execute_batch("CREATE TEMP TABLE IF NOT EXISTS statusline_write_check (id INTEGER); DROP TABLE statusline_write_check;")
                .is_ok();

            DbHealth {
                path: path.display().to_string(),
                exists,
                parent_exists,
                writable,
                journal_mode,
                schema_version,
                usage_cache_version,
                ok: writable,
                error: None,
            }
        }
        Err(err) => DbHealth {
            path: path.display().to_string(),
            exists,
            parent_exists,
            writable: false,
            journal_mode: None,
            schema_version: None,
            usage_cache_version: None,
            ok: false,
            error: Some(err.to_string()),
        },
    }
}

/// Fetch metadata value by key (opens a short-lived connection)
pub fn load_metadata(key: &str) -> Result<Option<MetadataEntry>> {
    let conn = open_db()?;
    get_metadata(&conn, key)
}

/// Persist metadata value by key (opens a short-lived connection)
pub fn store_metadata(key: &str, value: &str) -> Result<()> {
    let conn = open_db()?;
    set_metadata(&conn, key, value)
}

/// Initialize database schema
///
/// Creates tables and indexes if they don't exist.
/// Handles schema versioning via metadata table.
fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            session_key TEXT PRIMARY KEY,
            transcript_path TEXT NOT NULL,
            transcript_mtime INTEGER NOT NULL,
            today_date TEXT NOT NULL,
            today_cost REAL NOT NULL,
            entry_count INTEGER NOT NULL,
            last_parsed_at INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_today_date ON sessions(today_date);
        CREATE INDEX IF NOT EXISTS idx_transcript_path ON sessions(transcript_path);
        CREATE TABLE IF NOT EXISTS api_cache (
            cache_key TEXT PRIMARY KEY,
            data TEXT NOT NULL,
            fetched_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_expires_at ON api_cache(expires_at);
        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at INTEGER
        );
        INSERT OR IGNORE INTO metadata (key, value) VALUES ('schema_version', '1');",
    )?;

    // Ensure updated_at column exists (older installs may lack it)
    if let Err(e) = conn.execute("ALTER TABLE metadata ADD COLUMN updated_at INTEGER", []) {
        let msg = e.to_string();
        if !msg.contains("duplicate column name") {
            return Err(e.into());
        }
    }

    let cache_version = get_metadata(conn, "usage_cache_version")?;
    if cache_version.as_ref().map(|m| m.value.as_str()) != Some(USAGE_CACHE_VERSION) {
        conn.execute("DELETE FROM sessions", [])?;
        conn.execute("DELETE FROM metadata WHERE key LIKE 'global_sum:%'", [])?;
        set_metadata(conn, "usage_cache_version", USAGE_CACHE_VERSION)?;
    }

    Ok(())
}

/// Fetch metadata value and optional timestamp
pub fn get_metadata(conn: &Connection, key: &str) -> Result<Option<MetadataEntry>> {
    let mut stmt = conn.prepare("SELECT value, updated_at FROM metadata WHERE key = ?1")?;
    let result = stmt
        .query_row(params![key], |row| {
            let value: String = row.get(0)?;
            let updated_at: Option<i64> = row.get::<_, Option<i64>>(1).unwrap_or(None);
            Ok(MetadataEntry { value, updated_at })
        })
        .optional()?;
    Ok(result)
}

/// Set metadata value with current timestamp
pub fn set_metadata(conn: &Connection, key: &str, value: &str) -> Result<()> {
    let now = Utc::now().timestamp();
    conn.execute(
        "INSERT INTO metadata (key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, now],
    )?;
    Ok(())
}

/// Insert or update session entry
///
/// Uses UPSERT to handle both new sessions and updates to existing ones.
fn upsert_session(
    conn: &Connection,
    session_key: &str,
    transcript_path: &Path,
    mtime: i64,
    today: &str,
    cost: f64,
    count: usize,
) -> Result<()> {
    let now = Utc::now().timestamp();
    let transcript_str = transcript_path
        .to_str()
        .context("Invalid transcript path")?;

    let mut stmt = conn.prepare(
        "INSERT INTO sessions (session_key, transcript_path, transcript_mtime, today_date, today_cost, entry_count, last_parsed_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(session_key) DO UPDATE SET
             transcript_path = excluded.transcript_path,
             transcript_mtime = excluded.transcript_mtime,
             today_date = excluded.today_date,
             today_cost = excluded.today_cost,
             entry_count = excluded.entry_count,
             last_parsed_at = excluded.last_parsed_at,
             updated_at = excluded.updated_at",
    )?;

    stmt.execute(params![
        session_key,
        transcript_str,
        mtime,
        today,
        cost,
        count as i64,
        now,
        now,
        now
    ])?;

    Ok(())
}

fn stable_session_key(session_id: &str) -> String {
    session_id.to_string()
}

fn delete_legacy_project_session_rows(conn: &Connection, session_id: &str) -> Result<usize> {
    let session_len = session_id.len() as i64;
    let colon_pos = session_len + 1;
    let deleted = conn.execute(
        "DELETE FROM sessions
         WHERE substr(session_key, 1, ?) = ?
           AND substr(session_key, ?, 1) = ':'",
        params![session_len, session_id, colon_pos],
    )?;
    Ok(deleted)
}

/// Parse transcript file to calculate today's cost
///
/// This is a simplified parser that reads a single transcript file,
/// extracts usage blocks, calculates costs, and filters by today's date.
fn parse_transcript_today_cost(transcript_path: &Path, today: &str) -> Result<(f64, usize)> {
    use serde_json::Value;
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let file = File::open(transcript_path)?;
    let reader = BufReader::new(file);

    let mut aggregated_costs: HashMap<String, f64> = HashMap::new();
    let mut last_seen_raw: HashMap<String, (u64, u64, u64, u64)> = HashMap::new();
    let mut force_delta_mode: HashMap<String, bool> = HashMap::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let v: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let timestamp_str = v.get("timestamp").and_then(|t| t.as_str());

        let timestamp_date = if let Some(ts) = timestamp_str {
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) {
                let utc = parsed.with_timezone(&Utc);
                let local_dt = utc.with_timezone(&Local);
                local_dt.format("%Y-%m-%d").to_string()
            } else {
                continue;
            }
        } else {
            continue;
        };

        if timestamp_date != today {
            continue;
        }

        let message = v.get("message");
        let mid = message
            .and_then(|m| m.get("id"))
            .and_then(|m| m.as_str())
            .map(|s| s.to_string());
        let rid = v
            .get("requestId")
            .or_else(|| v.get("request_id"))
            .and_then(|r| r.as_str())
            .map(|s| s.to_string());

        if let Some(cost_val) = v.get("costUSD").or_else(|| v.get("cost_usd"))
            && let Some(cost) = cost_val
                .as_f64()
                .or_else(|| cost_val.as_str().and_then(|s| s.parse::<f64>().ok()))
        {
            let agg_key = if let Some(ref r) = rid {
                format!("R:{}", r)
            } else if let Some(ref m) = mid {
                format!("M:{}", m)
            } else {
                format!("C:{}|{}", timestamp_str.unwrap_or_default(), cost)
            };
            let current = aggregated_costs.entry(agg_key).or_insert(cost);
            if cost > *current {
                *current = cost;
            }
            continue;
        }

        if let Some(message) = message
            && let Some(usage) = message.get("usage")
        {
            let input = usage
                .get("input_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let output = usage
                .get("output_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let cache_create_reported = usage
                .get("cache_creation_input_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let cache_read = usage
                .get("cache_read_input_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let cache_create_1h = usage
                .get("cache_creation")
                .and_then(|creation| creation.get("ephemeral_1h_input_tokens"))
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let cache_create_5m = usage
                .get("cache_creation")
                .and_then(|creation| creation.get("ephemeral_5m_input_tokens"))
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let cache_create = cache_create_reported.max(cache_create_1h + cache_create_5m);
            let web_search_requests = usage
                .get("server_tool_use")
                .and_then(|o| o.get("web_search_requests"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            let speed = v
                .get("speed")
                .and_then(|s| s.as_str())
                .or_else(|| usage.get("speed").and_then(|s| s.as_str()));

            let model_id = v
                .get("model")
                .or_else(|| message.get("model"))
                .and_then(|m| m.as_str())
                .unwrap_or("claude-sonnet-4");
            let cost = crate::pricing::calculate_cost_for_usage_with_speed(model_id, usage, speed);

            if cost > 0.0
                || input > 0
                || output > 0
                || cache_create > 0
                || cache_create_1h > 0
                || cache_create_5m > 0
                || cache_read > 0
                || web_search_requests > 0
            {
                let composite = format!(
                    "{}|{}|{}|{}|{}|{}|{}|{}",
                    timestamp_str.unwrap_or_default(),
                    model_id,
                    input,
                    output,
                    cache_create,
                    cache_create_1h,
                    cache_create_5m,
                    cache_read
                );
                let agg_key = if let Some(ref r) = rid {
                    format!("R:{}", r)
                } else if let Some(ref m) = mid {
                    format!("M:{}", m)
                } else {
                    format!("F:{}", composite)
                };

                let prev_raw = last_seen_raw.get(&agg_key).copied();
                let mut is_delta = *force_delta_mode.get(&agg_key).unwrap_or(&false);
                if let Some((prev_input, prev_output, prev_cache_create, prev_cache_read)) =
                    prev_raw
                {
                    if input < prev_input
                        || output < prev_output
                        || cache_create < prev_cache_create
                        || cache_read < prev_cache_read
                    {
                        force_delta_mode.insert(agg_key.clone(), true);
                        is_delta = true;
                    }
                }

                if is_delta {
                    let (prev_input, prev_output, prev_cache_create, prev_cache_read) =
                        prev_raw.unwrap_or((0, 0, 0, 0));
                    last_seen_raw.insert(
                        agg_key.clone(),
                        (
                            prev_input + input,
                            prev_output + output,
                            prev_cache_create + cache_create,
                            prev_cache_read + cache_read,
                        ),
                    );
                    *aggregated_costs.entry(agg_key).or_insert(0.0) += cost;
                } else {
                    last_seen_raw
                        .insert(agg_key.clone(), (input, output, cache_create, cache_read));
                    let current = aggregated_costs.entry(agg_key).or_insert(cost);
                    if cost > *current {
                        *current = cost;
                    }
                }
            }
        }
    }

    let today_cost = aggregated_costs.values().sum();
    let entry_count = aggregated_costs.len();
    Ok((today_cost, entry_count))
}

/// Get global usage across all sessions
///
/// This is the main entry point for retrieving usage data. It:
/// 1. Checks if current session's transcript needs re-parsing (via mtime)
/// 2. Updates the cache if needed
/// 3. Cleans up stale entries from previous days
/// 4. Aggregates global usage across all active sessions
///
/// If `session_today_cost` is provided, it will be used instead of re-parsing
/// the transcript file (optimization to avoid double-parsing).
pub fn get_global_usage(
    session_id: &str,
    _project_dir: &str,
    transcript_path: &Path,
    session_today_cost: Option<f64>,
) -> Result<GlobalUsage> {
    if let Ok(val) = env::var("CLAUDE_DB_CACHE_DISABLE")
        && val == "1"
    {
        return Err(anyhow::anyhow!("DB cache disabled via env var"));
    }

    let conn = open_db()?;
    let session_key = stable_session_key(session_id);
    let today = Local::now().format("%Y-%m-%d").to_string();

    let metadata = fs::metadata(transcript_path)?;
    let current_mtime = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    let transcript_str = transcript_path
        .to_str()
        .context("Invalid transcript path")?;

    let cached = conn
        .query_row(
            "SELECT transcript_mtime, today_cost, today_date, transcript_path FROM sessions WHERE session_key = ?",
            params![session_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;

    // Track whether we modified the DB (to invalidate global sum cache)
    let mut db_was_modified = false;

    let current_session_cost =
        if let Some((cached_mtime, cached_cost, cached_date, cached_transcript_path)) = cached {
            // Only use cached cost if both mtime and date match (prevents using yesterday's cost after midnight)
            if cached_mtime == current_mtime && cached_date == today {
                if cached_transcript_path != transcript_str {
                    upsert_session(
                        &conn,
                        &session_key,
                        transcript_path,
                        current_mtime,
                        &today,
                        cached_cost,
                        0,
                    )?;
                    db_was_modified = true;
                }
                cached_cost
            } else {
                // Use provided session_today_cost if available (avoids re-parsing)
                let (cost, count) = if let Some(provided_cost) = session_today_cost {
                    (provided_cost, 0) // entry_count not needed when cost is provided
                } else {
                    parse_transcript_today_cost(transcript_path, &today)?
                };
                upsert_session(
                    &conn,
                    &session_key,
                    transcript_path,
                    current_mtime,
                    &today,
                    cost,
                    count,
                )?;
                db_was_modified = true;
                cost
            }
        } else {
            // Use provided session_today_cost if available (avoids re-parsing)
            let (cost, count) = if let Some(provided_cost) = session_today_cost {
                (provided_cost, 0) // entry_count not needed when cost is provided
            } else {
                parse_transcript_today_cost(transcript_path, &today)?
            };
            upsert_session(
                &conn,
                &session_key,
                transcript_path,
                current_mtime,
                &today,
                cost,
                count,
            )?;
            db_was_modified = true;
            cost
        };

    if delete_legacy_project_session_rows(&conn, session_id)? > 0 {
        db_was_modified = true;
    }

    conn.execute("DELETE FROM sessions WHERE today_date != ?", params![today])?;

    // Check cache for global sum (5s TTL to reduce redundant SUM queries across concurrent sessions)
    // Skip cache if we just modified the DB (invalidates cache)
    let cache_key = format!("global_sum:{}", today);
    let now = Utc::now().timestamp();
    let cached_sum: Option<(f64, usize)> = if !db_was_modified {
        if let Ok(Some(entry)) = get_metadata(&conn, &cache_key) {
            if let Some(updated_at) = entry.updated_at {
                if now - updated_at < 5 {
                    // Cache is fresh (< 5 seconds old)
                    entry
                        .value
                        .split_once(':')
                        .and_then(|(sum_str, count_str)| {
                            sum_str
                                .parse::<f64>()
                                .ok()
                                .zip(count_str.parse::<usize>().ok())
                        })
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None // DB was modified, so cache is invalid
    };

    let (global_today, sessions_count) = if let Some((sum, count)) = cached_sum {
        // Use cached value
        (sum, count)
    } else {
        // Cache miss or expired - run the query
        let (global_today, sessions_count): (f64, usize) = conn.query_row(
            "WITH logical_sessions AS (
                SELECT
                    CASE
                        WHEN instr(session_key, ':') > 0
                        THEN substr(session_key, 1, instr(session_key, ':') - 1)
                        ELSE session_key
                    END AS logical_session_key,
                    MAX(today_cost) AS today_cost
                FROM sessions
                WHERE today_date = ?
                GROUP BY logical_session_key
            )
            SELECT COALESCE(SUM(today_cost), 0.0), COUNT(*) FROM logical_sessions",
            params![today],
            |row| {
                let sum: f64 = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((sum, count as usize))
            },
        )?;

        // Cache the result for 5 seconds
        let cache_value = format!("{}:{}", global_today, sessions_count);
        let _ = set_metadata(&conn, &cache_key, &cache_value); // Ignore errors on cache write

        (global_today, sessions_count)
    };

    Ok(GlobalUsage {
        session_cost: current_session_cost,
        global_today,
        sessions_count,
    })
}

/// Get cached API data if still valid
///
/// Returns cached data if it exists and hasn't expired.
pub fn get_api_cache(cache_key: &str) -> Result<Option<String>> {
    let conn = open_db()?;
    let now = Utc::now().timestamp();

    let result = conn
        .query_row(
            "SELECT data FROM api_cache WHERE cache_key = ? AND expires_at > ?",
            params![cache_key, now],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    Ok(result)
}

/// Get cached API response, ignoring expiration (for stale fallback)
pub fn get_stale_api_cache(cache_key: &str) -> Result<Option<String>> {
    let conn = open_db()?;

    let result = conn
        .query_row(
            "SELECT data FROM api_cache WHERE cache_key = ?",
            params![cache_key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;

    Ok(result)
}

/// Try to set a cache entry only if it doesn't already exist (non-expired).
///
/// Returns `true` if the entry was inserted (caller "won" the race),
/// `false` if a valid entry already existed. Used as a distributed fetch lock
/// to prevent multiple concurrent processes from calling the API simultaneously.
pub fn try_set_api_cache(cache_key: &str, data: &str, ttl_seconds: i64) -> Result<bool> {
    let conn = open_db()?;
    let now = Utc::now().timestamp();
    let expires_at = now + ttl_seconds;

    // Delete expired entry for this key first so INSERT can succeed
    conn.execute(
        "DELETE FROM api_cache WHERE cache_key = ? AND expires_at <= ?",
        params![cache_key, now],
    )?;

    // INSERT ... ON CONFLICT DO NOTHING -- only the first writer wins
    let rows = conn.execute(
        "INSERT INTO api_cache (cache_key, data, fetched_at, expires_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(cache_key) DO NOTHING",
        params![cache_key, data, now, expires_at],
    )?;

    Ok(rows > 0)
}

/// Store API response in cache with expiration
///
/// Stores the data and automatically cleans up expired entries.
pub fn set_api_cache(cache_key: &str, data: &str, ttl_seconds: i64) -> Result<()> {
    let conn = open_db()?;
    let now = Utc::now().timestamp();
    let expires_at = now + ttl_seconds;

    conn.execute(
        "INSERT INTO api_cache (cache_key, data, fetched_at, expires_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(cache_key) DO UPDATE SET
             data = excluded.data,
             fetched_at = excluded.fetched_at,
             expires_at = excluded.expires_at",
        params![cache_key, data, now, expires_at],
    )?;

    // Clean up expired entries, but keep the main usage cache for stale fallback
    conn.execute(
        "DELETE FROM api_cache WHERE expires_at <= ? AND cache_key != 'oauth_usage_summary'",
        params![now],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    #[serial_test::serial]
    fn test_db_init() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                params![],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, "1");
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_upsert_session() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let transcript_path = PathBuf::from("/tmp/test.jsonl");

        upsert_session(
            &conn,
            "sess1:/path/to/project",
            &transcript_path,
            12345,
            "2025-10-18",
            1.23,
            10,
        )
        .unwrap();

        let cost: f64 = conn
            .query_row(
                "SELECT today_cost FROM sessions WHERE session_key = ?",
                params!["sess1:/path/to/project"],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(cost, 1.23);
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    fn test_parse_transcript_today_cost_deduplicates_cumulative_usage() {
        let temp_dir = TempDir::new().unwrap();
        let transcript_path = temp_dir.path().join("transcript.jsonl");
        let ts = Local::now().to_rfc3339();
        let today = Local::now().format("%Y-%m-%d").to_string();
        let first = serde_json::json!({
            "timestamp": ts,
            "requestId": "req-1",
            "message": {
                "id": "msg-1",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 1000,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            }
        });
        let second = serde_json::json!({
            "timestamp": ts,
            "requestId": "req-1",
            "message": {
                "id": "msg-1",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 2000,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            }
        });
        std::fs::write(&transcript_path, format!("{}\n{}\n", first, second)).unwrap();

        let (cost, count) = parse_transcript_today_cost(&transcript_path, &today).unwrap();

        assert_eq!(count, 1);
        assert!((cost - 0.03).abs() < 1e-10);
    }

    #[test]
    fn test_parse_transcript_today_cost_charges_split_cache_creation() {
        let temp_dir = TempDir::new().unwrap();
        let transcript_path = temp_dir.path().join("transcript.jsonl");
        let today = Local::now().format("%Y-%m-%d").to_string();
        let line = serde_json::json!({
            "timestamp": Local::now().to_rfc3339(),
            "message": {
                "id": "msg-split-cache",
                "model": "claude-sonnet-4-6",
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "cache_creation": {
                        "ephemeral_1h_input_tokens": 1_000_000
                    }
                }
            }
        });
        std::fs::write(&transcript_path, format!("{}\n", line)).unwrap();

        let (cost, count) = parse_transcript_today_cost(&transcript_path, &today).unwrap();

        assert_eq!(count, 1);
        assert!((cost - 6.0).abs() < 1e-10);
    }

    #[test]
    fn test_parse_transcript_today_cost_prices_top_level_fast_speed() {
        let temp_dir = TempDir::new().unwrap();
        let transcript_path = temp_dir.path().join("transcript.jsonl");
        let today = Local::now().format("%Y-%m-%d").to_string();
        let line = serde_json::json!({
            "timestamp": Local::now().to_rfc3339(),
            "speed": "fast",
            "message": {
                "id": "msg-fast",
                "model": "claude-opus-4-6",
                "usage": {
                    "input_tokens": 1_000_000,
                    "output_tokens": 1_000_000,
                    "cache_creation_input_tokens": 1_000_000,
                    "cache_read_input_tokens": 1_000_000,
                    "server_tool_use": { "web_search_requests": 2 }
                }
            }
        });
        std::fs::write(&transcript_path, format!("{}\n", line)).unwrap();

        let (cost, count) = parse_transcript_today_cost(&transcript_path, &today).unwrap();

        assert_eq!(count, 1);
        assert!((cost - 220.52).abs() < 1e-10);
    }

    #[test]
    #[serial_test::serial]
    fn test_global_usage_deduplicates_project_move() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let old_transcript_path = temp_dir.path().join("old-transcript.jsonl");
        let new_transcript_path = temp_dir.path().join("new-transcript.jsonl");
        std::fs::write(&old_transcript_path, "{}\n").unwrap();
        std::fs::write(&new_transcript_path, "{}\n").unwrap();
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let first = get_global_usage(
            "sess-moved",
            "/old/project",
            &old_transcript_path,
            Some(1.23),
        )
        .unwrap();
        let second = get_global_usage(
            "sess-moved",
            "/new/project",
            &new_transcript_path,
            Some(1.23),
        )
        .unwrap();

        assert!((first.global_today - 1.23).abs() < 1e-10);
        assert_eq!(first.sessions_count, 1);
        assert!((second.session_cost - 1.23).abs() < 1e-10);
        assert!((second.global_today - 1.23).abs() < 1e-10);
        assert_eq!(second.sessions_count, 1);

        let conn = open_db().unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        let stored_transcript_path: String = conn
            .query_row(
                "SELECT transcript_path FROM sessions WHERE session_key = ?",
                params!["sess-moved"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            stored_transcript_path,
            new_transcript_path.display().to_string()
        );
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_global_usage_deduplicates_legacy_project_keys() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let transcript_path = temp_dir.path().join("transcript.jsonl");
        std::fs::write(&transcript_path, "{}\n").unwrap();
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let today = Local::now().format("%Y-%m-%d").to_string();
        upsert_session(
            &conn,
            "legacy-session:/old/project",
            &transcript_path,
            1,
            &today,
            1.0,
            0,
        )
        .unwrap();
        upsert_session(
            &conn,
            "legacy-session:/new/project",
            &transcript_path,
            1,
            &today,
            2.0,
            0,
        )
        .unwrap();

        let usage =
            get_global_usage("current-session", "/current", &transcript_path, Some(3.0)).unwrap();

        assert!((usage.global_today - 5.0).abs() < 1e-10);
        assert_eq!(usage.sessions_count, 2);
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_global_usage_removes_legacy_rows_for_active_session() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let transcript_path = temp_dir.path().join("transcript.jsonl");
        std::fs::write(&transcript_path, "{}\n").unwrap();
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let today = Local::now().format("%Y-%m-%d").to_string();
        upsert_session(
            &conn,
            "legacy-session:/old/project",
            &transcript_path,
            1,
            &today,
            1.0,
            0,
        )
        .unwrap();
        upsert_session(
            &conn,
            "legacy-session:/new/project",
            &transcript_path,
            1,
            &today,
            2.0,
            0,
        )
        .unwrap();

        let usage =
            get_global_usage("legacy-session", "/current", &transcript_path, Some(3.0)).unwrap();

        assert!((usage.global_today - 3.0).abs() < 1e-10);
        assert_eq!(usage.sessions_count, 1);
        let legacy_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions
                 WHERE substr(session_key, 1, ?) = ?
                   AND substr(session_key, ?, 1) = ':'",
                params![
                    "legacy-session".len() as i64,
                    "legacy-session",
                    "legacy-session".len() as i64 + 1
                ],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_rows, 0);
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_api_cache() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_api_cache.db");
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        // Ensure clean DB
        let _ = std::fs::remove_file(&db_path);

        // Set cache with longer TTL
        let test_data = r#"{"test":"data","value":123}"#;
        let result = set_api_cache("test_key_unique", test_data, 300);
        assert!(result.is_ok(), "Failed to set cache: {:?}", result.err());

        // Get cache - should succeed
        let cached = get_api_cache("test_key_unique");
        assert!(cached.is_ok(), "Failed to get cache: {:?}", cached.err());
        let cached_value = cached.unwrap();
        assert!(
            cached_value.is_some(),
            "Cache returned None when it should have data"
        );
        assert_eq!(cached_value.unwrap(), test_data.to_string());

        // Get non-existent key
        let missing = get_api_cache("missing_key").unwrap();
        assert_eq!(missing, None);

        // Set with 0 TTL (expires immediately)
        set_api_cache("expired_key", "expired", 0).unwrap();

        // Wait to ensure expiration
        thread::sleep(std::time::Duration::from_millis(100));

        let expired = get_api_cache("expired_key").unwrap();
        assert_eq!(expired, None);

        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_stale_cleanup() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let transcript_path = PathBuf::from("/tmp/test.jsonl");

        upsert_session(
            &conn,
            "sess1:/path",
            &transcript_path,
            12345,
            "2025-10-17",
            1.0,
            10,
        )
        .unwrap();
        upsert_session(
            &conn,
            "sess2:/path",
            &transcript_path,
            12345,
            "2025-10-18",
            2.0,
            10,
        )
        .unwrap();

        conn.execute(
            "DELETE FROM sessions WHERE today_date != ?",
            params!["2025-10-18"],
        )
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();

        assert_eq!(count, 1);
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }
}
