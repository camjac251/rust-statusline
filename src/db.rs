//! SQLite-based persistent caching for global usage tracking across multiple
//! concurrent Claude Code sessions.
//!
//! This module provides:
//! - SQLite database initialization with schema versioning
//! - Session usage caching with mtime-based invalidation
//! - Global usage aggregation across all active sessions
//! - Concurrent access support via WAL mode

use crate::models::Entry;
use anyhow::{Context, Result, bail};
use chrono::{Local, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const SCHEMA_VERSION: i64 = 4;
const SCHEMA_VERSION_STR: &str = "4";
const USAGE_CACHE_VERSION: &str = "2";
const METADATA_KEY_SCHEMA_VERSION: &str = "schema_version";
const METADATA_KEY_USAGE_CACHE_VERSION: &str = "usage_cache_version";
const GLOBAL_SUM_CACHE_PREFIX: &str = "global_sum:";
const GLOBAL_SUM_CACHE_TTL_SECONDS: i64 = 5;
const OAUTH_USAGE_SUMMARY_CACHE_KEY: &str = "oauth_usage_summary";
const COST_EPSILON: f64 = 1e-9;

mod sql {
    pub const INIT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            session_key TEXT NOT NULL,
            transcript_path TEXT NOT NULL,
            transcript_mtime INTEGER NOT NULL CHECK (transcript_mtime >= 0),
            today_date TEXT NOT NULL CHECK (length(today_date) = 10),
            today_cost REAL NOT NULL CHECK (today_cost >= 0.0),
            entry_count INTEGER NOT NULL CHECK (entry_count >= 0),
            last_parsed_at INTEGER NOT NULL CHECK (last_parsed_at >= 0),
            created_at INTEGER NOT NULL CHECK (created_at >= 0),
            updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
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
        CREATE TABLE IF NOT EXISTS usage_events (
            event_key TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            transcript_path TEXT NOT NULL,
            ts INTEGER NOT NULL,
            today_date TEXT NOT NULL CHECK (length(today_date) = 10),
            model TEXT,
            input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
            output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
            cache_create_tokens INTEGER NOT NULL CHECK (cache_create_tokens >= 0),
            cache_read_tokens INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
            web_search_requests INTEGER NOT NULL CHECK (web_search_requests >= 0),
            cost REAL NOT NULL CHECK (cost >= 0.0),
            source TEXT NOT NULL,
            created_at INTEGER NOT NULL CHECK (created_at >= 0),
            updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
        );
        CREATE INDEX IF NOT EXISTS idx_usage_events_today_session
            ON usage_events(today_date, session_id);
        CREATE INDEX IF NOT EXISTS idx_usage_events_session_date
            ON usage_events(session_id, today_date);";

    pub const ADD_METADATA_UPDATED_AT: &str = "ALTER TABLE metadata ADD COLUMN updated_at INTEGER";
    pub const CREATE_SESSIONS_TODAY_DATE_INDEX: &str =
        "CREATE INDEX IF NOT EXISTS idx_today_date ON sessions(today_date)";
    pub const CREATE_SESSIONS_TRANSCRIPT_PATH_INDEX: &str =
        "CREATE INDEX IF NOT EXISTS idx_transcript_path ON sessions(transcript_path)";
    pub const CREATE_SESSIONS_TODAY_SESSION_ID_INDEX: &str = "CREATE INDEX IF NOT EXISTS idx_sessions_today_session_id ON sessions(today_date, session_id)";
    pub const ADD_SESSION_ID: &str =
        "ALTER TABLE sessions ADD COLUMN session_id TEXT NOT NULL DEFAULT ''";
    pub const BACKFILL_SESSION_ID: &str = "UPDATE sessions
         SET session_id = CASE
             WHEN instr(session_key, ':') > 0
             THEN substr(session_key, 1, instr(session_key, ':') - 1)
             ELSE session_key
         END
         WHERE session_id IS NULL OR session_id = ''";
    pub const DROP_SESSIONS_V3: &str = "DROP TABLE IF EXISTS sessions_v3";
    pub const CREATE_SESSIONS_V3: &str = "CREATE TABLE sessions_v3 (
            session_id TEXT PRIMARY KEY,
            session_key TEXT NOT NULL,
            transcript_path TEXT NOT NULL,
            transcript_mtime INTEGER NOT NULL CHECK (transcript_mtime >= 0),
            today_date TEXT NOT NULL CHECK (length(today_date) = 10),
            today_cost REAL NOT NULL CHECK (today_cost >= 0.0),
            entry_count INTEGER NOT NULL CHECK (entry_count >= 0),
            last_parsed_at INTEGER NOT NULL CHECK (last_parsed_at >= 0),
            created_at INTEGER NOT NULL CHECK (created_at >= 0),
            updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
        )";
    pub const COPY_SESSIONS_V3: &str = "WITH normalized AS (
            SELECT
                COALESCE(
                    NULLIF(session_id, ''),
                    CASE
                        WHEN instr(session_key, ':') > 0
                        THEN substr(session_key, 1, instr(session_key, ':') - 1)
                        ELSE session_key
                    END
                ) AS logical_session_id,
                session_key,
                transcript_path,
                transcript_mtime,
                today_date,
                today_cost,
                entry_count,
                last_parsed_at,
                created_at,
                updated_at
            FROM sessions
        ),
        ranked AS (
            SELECT
                *,
                ROW_NUMBER() OVER (
                    PARTITION BY logical_session_id
                    ORDER BY updated_at DESC, transcript_mtime DESC, today_cost DESC, session_key DESC
                ) AS row_rank
            FROM normalized
            WHERE logical_session_id != ''
        )
        INSERT INTO sessions_v3 (
            session_id,
            session_key,
            transcript_path,
            transcript_mtime,
            today_date,
            today_cost,
            entry_count,
            last_parsed_at,
            created_at,
            updated_at
        )
        SELECT
            logical_session_id,
            session_key,
            transcript_path,
            transcript_mtime,
            today_date,
            today_cost,
            entry_count,
            last_parsed_at,
            created_at,
            updated_at
        FROM ranked
        WHERE row_rank = 1";
    pub const DROP_SESSIONS: &str = "DROP TABLE sessions";
    pub const RENAME_SESSIONS_V3: &str = "ALTER TABLE sessions_v3 RENAME TO sessions";
    pub const DELETE_ALL_SESSIONS: &str = "DELETE FROM sessions";
    pub const DELETE_ALL_USAGE_EVENTS: &str = "DELETE FROM usage_events";
    pub const DELETE_GLOBAL_SUM_CACHE: &str = "DELETE FROM metadata WHERE key LIKE 'global_sum:%'";
    pub const CREATE_USAGE_EVENTS: &str = "CREATE TABLE IF NOT EXISTS usage_events (
            event_key TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            transcript_path TEXT NOT NULL,
            ts INTEGER NOT NULL,
            today_date TEXT NOT NULL CHECK (length(today_date) = 10),
            model TEXT,
            input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
            output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
            cache_create_tokens INTEGER NOT NULL CHECK (cache_create_tokens >= 0),
            cache_read_tokens INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
            web_search_requests INTEGER NOT NULL CHECK (web_search_requests >= 0),
            cost REAL NOT NULL CHECK (cost >= 0.0),
            source TEXT NOT NULL,
            created_at INTEGER NOT NULL CHECK (created_at >= 0),
            updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
        )";
    pub const CREATE_USAGE_EVENTS_TODAY_SESSION_INDEX: &str = "CREATE INDEX IF NOT EXISTS idx_usage_events_today_session ON usage_events(today_date, session_id)";
    pub const CREATE_USAGE_EVENTS_SESSION_DATE_INDEX: &str = "CREATE INDEX IF NOT EXISTS idx_usage_events_session_date ON usage_events(session_id, today_date)";
    pub const BACKFILL_USAGE_EVENTS_FROM_SESSIONS: &str = "INSERT OR IGNORE INTO usage_events (
            event_key,
            session_id,
            transcript_path,
            ts,
            today_date,
            model,
            input_tokens,
            output_tokens,
            cache_create_tokens,
            cache_read_tokens,
            web_search_requests,
            cost,
            source,
            created_at,
            updated_at
        )
        SELECT
            'session:' || session_id || ':' || today_date,
            session_id,
            transcript_path,
            updated_at,
            today_date,
            NULL,
            0,
            0,
            0,
            0,
            0,
            today_cost,
            'session_summary',
            created_at,
            updated_at
        FROM sessions
        WHERE session_id != '' AND today_cost >= 0.0";
    pub const GET_METADATA: &str = "SELECT value, updated_at FROM metadata WHERE key = ?1";
    pub const SET_METADATA: &str = "INSERT INTO metadata (key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at";
    pub const UPSERT_SESSION: &str = "INSERT INTO sessions (session_key, session_id, transcript_path, transcript_mtime, today_date, today_cost, entry_count, last_parsed_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(session_id) DO UPDATE SET
             session_key = excluded.session_key,
             session_id = excluded.session_id,
             transcript_path = excluded.transcript_path,
             transcript_mtime = excluded.transcript_mtime,
             today_date = excluded.today_date,
             today_cost = excluded.today_cost,
             entry_count = excluded.entry_count,
             last_parsed_at = excluded.last_parsed_at,
             updated_at = excluded.updated_at";
    pub const SELECT_CACHED_SESSION: &str = "SELECT transcript_mtime, today_cost, today_date, transcript_path, entry_count, session_key FROM sessions WHERE session_id = ?";
    pub const DELETE_STALE_SESSIONS: &str = "DELETE FROM sessions WHERE today_date != ?";
    pub const DELETE_STALE_USAGE_EVENTS: &str = "DELETE FROM usage_events WHERE today_date != ?";
    pub const DELETE_USAGE_EVENTS_FOR_SESSION_DATE: &str =
        "DELETE FROM usage_events WHERE session_id = ? AND today_date = ?";
    pub const HAS_USAGE_EVENTS_FOR_SESSION_DATE: &str =
        "SELECT 1 FROM usage_events WHERE session_id = ? AND today_date = ? LIMIT 1";
    pub const UPSERT_USAGE_EVENT: &str = "INSERT INTO usage_events (
            event_key,
            session_id,
            transcript_path,
            ts,
            today_date,
            model,
            input_tokens,
            output_tokens,
            cache_create_tokens,
            cache_read_tokens,
            web_search_requests,
            cost,
            source,
            created_at,
            updated_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(event_key) DO UPDATE SET
            session_id = excluded.session_id,
            transcript_path = excluded.transcript_path,
            ts = excluded.ts,
            today_date = excluded.today_date,
            model = excluded.model,
            input_tokens = excluded.input_tokens,
            output_tokens = excluded.output_tokens,
            cache_create_tokens = excluded.cache_create_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            web_search_requests = excluded.web_search_requests,
            cost = excluded.cost,
            source = excluded.source,
            updated_at = excluded.updated_at";
    pub const SELECT_GLOBAL_TODAY: &str = "WITH session_totals AS (
            SELECT session_id, SUM(cost) AS today_cost
            FROM usage_events
            WHERE today_date = ?
            GROUP BY session_id
        )
        SELECT COALESCE(SUM(today_cost), 0.0), COUNT(*) FROM session_totals";
    pub const GET_FRESH_API_CACHE: &str =
        "SELECT data FROM api_cache WHERE cache_key = ? AND expires_at > ?";
    pub const GET_STALE_API_CACHE: &str = "SELECT data FROM api_cache WHERE cache_key = ?";
    pub const DELETE_EXPIRED_API_CACHE_KEY: &str =
        "DELETE FROM api_cache WHERE cache_key = ? AND expires_at <= ?";
    pub const TRY_INSERT_API_CACHE: &str =
        "INSERT INTO api_cache (cache_key, data, fetched_at, expires_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(cache_key) DO NOTHING";
    pub const UPSERT_API_CACHE: &str =
        "INSERT INTO api_cache (cache_key, data, fetched_at, expires_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(cache_key) DO UPDATE SET
             data = excluded.data,
             fetched_at = excluded.fetched_at,
             expires_at = excluded.expires_at";
    pub const DELETE_EXPIRED_API_CACHE: &str =
        "DELETE FROM api_cache WHERE expires_at <= ? AND cache_key != ?";
}

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

impl MetadataEntry {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let value: String = row.get(0)?;
        let updated_at = row.get::<_, Option<i64>>(1).unwrap_or(None);
        Ok(Self { value, updated_at })
    }
}

#[derive(Debug, Clone)]
struct CachedSessionRow {
    transcript_mtime: i64,
    today_cost: f64,
    today_date: String,
    transcript_path: String,
    entry_count: usize,
    session_key: String,
}

impl CachedSessionRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let entry_count: i64 = row.get(4)?;
        Ok(Self {
            transcript_mtime: row.get(0)?,
            today_cost: row.get(1)?,
            today_date: row.get(2)?,
            transcript_path: row.get(3)?,
            entry_count: usize::try_from(entry_count).unwrap_or(0),
            session_key: row.get(5)?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct GlobalTodayRow {
    total_cost: f64,
    sessions_count: usize,
}

impl GlobalTodayRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let count: i64 = row.get(1)?;
        Ok(Self {
            total_cost: row.get(0)?,
            sessions_count: usize::try_from(count).unwrap_or(0),
        })
    }
}

#[derive(Debug, Clone)]
struct UsageEvent {
    event_key: String,
    session_id: String,
    transcript_path: String,
    ts: i64,
    today_date: String,
    model: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cache_create_tokens: u64,
    cache_read_tokens: u64,
    web_search_requests: u64,
    cost: f64,
    source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DbHealth {
    pub path: String,
    pub exists: bool,
    pub parent_exists: bool,
    pub writable: bool,
    pub journal_mode: Option<String>,
    pub schema_version: Option<String>,
    pub user_version: Option<i64>,
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
                user_version: None,
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
            let schema_version = get_metadata(&conn, METADATA_KEY_SCHEMA_VERSION)
                .ok()
                .flatten()
                .map(|entry| entry.value);
            let user_version = sqlite_user_version(&conn).ok();
            let usage_cache_version = get_metadata(&conn, METADATA_KEY_USAGE_CACHE_VERSION)
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
                user_version,
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
            user_version: None,
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
    conn.execute_batch(sql::INIT_SCHEMA)?;

    migrate_schema(conn)?;

    match get_metadata(conn, METADATA_KEY_USAGE_CACHE_VERSION)? {
        Some(cache_version) if cache_version.value == USAGE_CACHE_VERSION => {}
        Some(_) => {
            conn.execute(sql::DELETE_ALL_SESSIONS, [])?;
            conn.execute(sql::DELETE_ALL_USAGE_EVENTS, [])?;
            clear_global_sum_cache(conn)?;
            set_metadata(conn, METADATA_KEY_USAGE_CACHE_VERSION, USAGE_CACHE_VERSION)?;
        }
        None => {
            clear_global_sum_cache(conn)?;
            set_metadata(conn, METADATA_KEY_USAGE_CACHE_VERSION, USAGE_CACHE_VERSION)?;
        }
    }

    Ok(())
}

fn migrate_schema(conn: &Connection) -> Result<()> {
    let user_version = sqlite_user_version(conn)?;
    let mut schema_changed = false;

    if user_version > SCHEMA_VERSION {
        bail!(
            "SQLite schema version {} is newer than supported version {}",
            user_version,
            SCHEMA_VERSION
        );
    }

    if !table_has_column(conn, "metadata", "updated_at")? {
        conn.execute(sql::ADD_METADATA_UPDATED_AT, [])?;
        schema_changed = true;
    }

    if user_version < 2 || !table_has_column(conn, "sessions", "session_id")? {
        migrate_sessions_session_id(conn)?;
        schema_changed = true;
    }

    if !table_column_is_primary_key(conn, "sessions", "session_id")? {
        migrate_sessions_session_id_primary_key(conn)?;
        schema_changed = true;
    }

    create_session_indexes(conn)?;

    create_usage_events_schema(conn)?;
    if user_version < 4 {
        conn.execute(sql::BACKFILL_USAGE_EVENTS_FROM_SESSIONS, [])?;
        schema_changed = true;
    }

    let metadata_version = get_metadata(conn, METADATA_KEY_SCHEMA_VERSION)?;
    if metadata_version.as_ref().map(|m| m.value.as_str()) != Some(SCHEMA_VERSION_STR) {
        set_metadata(conn, METADATA_KEY_SCHEMA_VERSION, SCHEMA_VERSION_STR)?;
        schema_changed = true;
    }

    if user_version != SCHEMA_VERSION {
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        schema_changed = true;
    }

    if schema_changed {
        clear_global_sum_cache(conn)?;
    }

    Ok(())
}

fn migrate_sessions_session_id(conn: &Connection) -> Result<()> {
    if !table_has_column(conn, "sessions", "session_id")? {
        conn.execute(sql::ADD_SESSION_ID, [])?;
    }

    conn.execute(sql::BACKFILL_SESSION_ID, [])?;

    Ok(())
}

fn migrate_sessions_session_id_primary_key(conn: &Connection) -> Result<()> {
    run_schema_change(conn, |conn| {
        conn.execute(sql::DROP_SESSIONS_V3, [])?;
        conn.execute(sql::CREATE_SESSIONS_V3, [])?;
        conn.execute(sql::COPY_SESSIONS_V3, [])?;
        conn.execute(sql::DROP_SESSIONS, [])?;
        conn.execute(sql::RENAME_SESSIONS_V3, [])?;
        Ok(())
    })
}

fn create_session_indexes(conn: &Connection) -> Result<()> {
    conn.execute(sql::CREATE_SESSIONS_TODAY_DATE_INDEX, [])?;
    conn.execute(sql::CREATE_SESSIONS_TRANSCRIPT_PATH_INDEX, [])?;
    conn.execute(sql::CREATE_SESSIONS_TODAY_SESSION_ID_INDEX, [])?;
    Ok(())
}

fn create_usage_events_schema(conn: &Connection) -> Result<()> {
    conn.execute(sql::CREATE_USAGE_EVENTS, [])?;
    conn.execute(sql::CREATE_USAGE_EVENTS_TODAY_SESSION_INDEX, [])?;
    conn.execute(sql::CREATE_USAGE_EVENTS_SESSION_DATE_INDEX, [])?;
    Ok(())
}

fn run_schema_change(
    conn: &Connection,
    change: impl FnOnce(&Connection) -> Result<()>,
) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = change(conn);
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

fn sqlite_user_version(conn: &Connection) -> Result<i64> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(Into::into)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    debug_assert!(table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_column_is_primary_key(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    debug_assert!(table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        let pk: i64 = row.get(5)?;
        if name == column {
            return Ok(pk > 0);
        }
    }
    Ok(false)
}

fn clear_global_sum_cache(conn: &Connection) -> Result<usize> {
    conn.execute(sql::DELETE_GLOBAL_SUM_CACHE, [])
        .map_err(Into::into)
}

fn global_sum_cache_key(today: &str) -> String {
    format!("{GLOBAL_SUM_CACHE_PREFIX}{today}")
}

fn encode_global_sum_cache(total_cost: f64, sessions_count: usize) -> String {
    format!("{total_cost}:{sessions_count}")
}

fn decode_global_sum_cache(value: &str) -> Option<GlobalTodayRow> {
    let (sum_str, count_str) = value.split_once(':')?;
    Some(GlobalTodayRow {
        total_cost: sum_str.parse().ok()?,
        sessions_count: count_str.parse().ok()?,
    })
}

/// Fetch metadata value and optional timestamp
pub fn get_metadata(conn: &Connection, key: &str) -> Result<Option<MetadataEntry>> {
    let mut stmt = conn.prepare(sql::GET_METADATA)?;
    let result = stmt
        .query_row(params![key], MetadataEntry::from_row)
        .optional()?;
    Ok(result)
}

/// Set metadata value with current timestamp
pub fn set_metadata(conn: &Connection, key: &str, value: &str) -> Result<()> {
    let now = Utc::now().timestamp();
    conn.execute(sql::SET_METADATA, params![key, value, now])?;
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
    let session_id = logical_session_id(session_key);

    let mut stmt = conn.prepare(sql::UPSERT_SESSION)?;

    stmt.execute(params![
        session_key,
        session_id,
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

fn logical_session_id(session_key: &str) -> &str {
    session_key
        .split_once(':')
        .map_or(session_key, |(id, _)| id)
}

fn i64_from_u64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn event_hash_key(prefix: &str, material: &str) -> String {
    use std::fmt::Write;

    let mut hasher = Sha256::new();
    hasher.update(material.as_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    format!("{prefix}:{encoded}")
}

fn event_key(
    source: &str,
    session_id: &str,
    today: &str,
    identity: &str,
    fingerprint: &str,
) -> String {
    event_hash_key(
        "usage",
        &format!("{source}|{session_id}|{today}|{identity}|{fingerprint}"),
    )
}

fn synthetic_usage_event(
    session_id: &str,
    transcript_path: &str,
    today: &str,
    cost: f64,
    source: &'static str,
) -> UsageEvent {
    let now = Utc::now().timestamp();
    UsageEvent {
        event_key: event_key(source, session_id, today, source, &cost.to_string()),
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        ts: now,
        today_date: today.to_string(),
        model: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_create_tokens: 0,
        cache_read_tokens: 0,
        web_search_requests: 0,
        cost: cost.max(0.0),
        source,
    }
}

fn usage_event_from_entry(
    session_id: &str,
    transcript_path: &str,
    today: &str,
    entry: &Entry,
) -> Option<UsageEvent> {
    if entry.session_id.as_deref() != Some(session_id) {
        return None;
    }

    let local_date = entry
        .ts
        .with_timezone(&Local)
        .format("%Y-%m-%d")
        .to_string();
    if local_date != today {
        return None;
    }

    let identity = if let Some(req_id) = &entry.req_id {
        format!("R:{req_id}")
    } else if let Some(msg_id) = &entry.msg_id {
        format!("M:{msg_id}")
    } else {
        format!(
            "F:{}:{}:{}:{}:{}:{}",
            entry.ts.to_rfc3339(),
            entry.model.as_deref().unwrap_or_default(),
            entry.input,
            entry.output,
            entry.cache_create,
            entry.cache_read
        )
    };
    let fingerprint = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        entry.ts.to_rfc3339(),
        entry.model.as_deref().unwrap_or_default(),
        entry.input,
        entry.output,
        entry.cache_create,
        entry.cache_read,
        entry.web_search_requests,
        entry.agent_id.as_deref().unwrap_or_default()
    );

    Some(UsageEvent {
        event_key: event_key("scan_entry", session_id, today, &identity, &fingerprint),
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        ts: entry.ts.timestamp(),
        today_date: today.to_string(),
        model: entry.model.clone(),
        input_tokens: entry.input,
        output_tokens: entry.output,
        cache_create_tokens: entry.cache_create,
        cache_read_tokens: entry.cache_read,
        web_search_requests: entry.web_search_requests,
        cost: entry.cost.max(0.0),
        source: "scan_entry",
    })
}

fn events_cost(events: &[UsageEvent]) -> f64 {
    events.iter().map(|event| event.cost).sum()
}

fn reconcile_events_with_provided_cost(
    mut events: Vec<UsageEvent>,
    session_id: &str,
    transcript_path: &str,
    today: &str,
    provided_cost: Option<f64>,
) -> Vec<UsageEvent> {
    let Some(provided_cost) = provided_cost else {
        return events;
    };

    let event_cost = events_cost(&events);
    let diff = provided_cost - event_cost;
    if diff.abs() <= COST_EPSILON {
        return events;
    }

    if events.is_empty() || diff < 0.0 {
        return vec![synthetic_usage_event(
            session_id,
            transcript_path,
            today,
            provided_cost,
            "scan_summary",
        )];
    }

    events.push(synthetic_usage_event(
        session_id,
        transcript_path,
        today,
        diff,
        "scan_adjustment",
    ));
    events
}

fn usage_events_from_entries(
    session_id: &str,
    transcript_path: &str,
    today: &str,
    entries: &[Entry],
) -> Vec<UsageEvent> {
    entries
        .iter()
        .filter_map(|entry| usage_event_from_entry(session_id, transcript_path, today, entry))
        .collect()
}

fn summarize_usage_events(events: &[UsageEvent]) -> (f64, usize) {
    let entry_count = events
        .iter()
        .filter(|event| {
            matches!(
                event.source,
                "scan_entry" | "transcript_cost" | "transcript_usage"
            )
        })
        .count();
    (events_cost(events), entry_count)
}

fn has_usage_events_for_session_date(
    conn: &Connection,
    session_id: &str,
    today: &str,
) -> Result<bool> {
    let exists = conn
        .query_row(
            sql::HAS_USAGE_EVENTS_FOR_SESSION_DATE,
            params![session_id, today],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(exists)
}

#[cfg(test)]
fn replace_usage_events_for_session_date(
    conn: &Connection,
    session_id: &str,
    today: &str,
    events: &[UsageEvent],
) -> Result<()> {
    run_schema_change(conn, |conn| {
        replace_usage_events_for_session_date_in_transaction(conn, session_id, today, events)
    })
}

fn replace_usage_events_for_session_date_in_transaction(
    conn: &Connection,
    session_id: &str,
    today: &str,
    events: &[UsageEvent],
) -> Result<()> {
    conn.execute(
        sql::DELETE_USAGE_EVENTS_FOR_SESSION_DATE,
        params![session_id, today],
    )?;
    let now = Utc::now().timestamp();
    let mut stmt = conn.prepare(sql::UPSERT_USAGE_EVENT)?;
    for event in events {
        stmt.execute(params![
            &event.event_key,
            &event.session_id,
            &event.transcript_path,
            event.ts,
            &event.today_date,
            event.model.as_deref(),
            i64_from_u64(event.input_tokens),
            i64_from_u64(event.output_tokens),
            i64_from_u64(event.cache_create_tokens),
            i64_from_u64(event.cache_read_tokens),
            i64_from_u64(event.web_search_requests),
            event.cost,
            event.source,
            now,
            now
        ])?;
    }
    Ok(())
}

struct SessionUsageUpdate<'a> {
    session_key: &'a str,
    transcript_path: &'a Path,
    mtime: i64,
    today: &'a str,
    cost: f64,
    count: usize,
    events: &'a [UsageEvent],
}

fn upsert_session_and_usage_events(
    conn: &Connection,
    update: SessionUsageUpdate<'_>,
) -> Result<()> {
    run_schema_change(conn, |conn| {
        replace_usage_events_for_session_date_in_transaction(
            conn,
            update.session_key,
            update.today,
            update.events,
        )?;
        upsert_session(
            conn,
            update.session_key,
            update.transcript_path,
            update.mtime,
            update.today,
            update.cost,
            update.count,
        )
    })
}

fn transcript_cost_event(
    session_id: &str,
    transcript_path: &str,
    today: &str,
    agg_key: &str,
    ts: i64,
    cost: f64,
) -> UsageEvent {
    UsageEvent {
        event_key: event_key("transcript_cost", session_id, today, agg_key, "cost_usd"),
        session_id: session_id.to_string(),
        transcript_path: transcript_path.to_string(),
        ts,
        today_date: today.to_string(),
        model: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_create_tokens: 0,
        cache_read_tokens: 0,
        web_search_requests: 0,
        cost: cost.max(0.0),
        source: "transcript_cost",
    }
}

struct TranscriptUsageEventInput<'a> {
    session_id: &'a str,
    transcript_path: &'a str,
    today: &'a str,
    agg_key: &'a str,
    fingerprint: &'a str,
    ts: i64,
    model_id: &'a str,
    input: u64,
    output: u64,
    cache_create: u64,
    cache_read: u64,
    web_search_requests: u64,
    cost: f64,
}

fn transcript_usage_event(input: TranscriptUsageEventInput<'_>) -> UsageEvent {
    UsageEvent {
        event_key: event_key(
            "transcript_usage",
            input.session_id,
            input.today,
            input.agg_key,
            input.fingerprint,
        ),
        session_id: input.session_id.to_string(),
        transcript_path: input.transcript_path.to_string(),
        ts: input.ts,
        today_date: input.today.to_string(),
        model: Some(input.model_id.to_string()),
        input_tokens: input.input,
        output_tokens: input.output,
        cache_create_tokens: input.cache_create,
        cache_read_tokens: input.cache_read,
        web_search_requests: input.web_search_requests,
        cost: input.cost.max(0.0),
        source: "transcript_usage",
    }
}

/// Parse transcript file into normalized cost events for today's usage.
fn parse_transcript_today_events(
    session_id: &str,
    transcript_path: &Path,
    today: &str,
) -> Result<Vec<UsageEvent>> {
    use serde_json::Value;
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let file = File::open(transcript_path)?;
    let reader = BufReader::new(file);
    let transcript_str = transcript_path
        .to_str()
        .context("Invalid transcript path")?;

    let mut aggregated_events: HashMap<String, UsageEvent> = HashMap::new();
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
        let ts = chrono::DateTime::parse_from_rfc3339(timestamp_str.unwrap_or_default())
            .map(|parsed| parsed.with_timezone(&Utc).timestamp())
            .unwrap_or_else(|_| Utc::now().timestamp());

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
            let event =
                transcript_cost_event(session_id, transcript_str, today, &agg_key, ts, cost);
            match aggregated_events.get_mut(&agg_key) {
                Some(current) if cost > current.cost => {
                    *current = event;
                }
                None => {
                    aggregated_events.insert(agg_key, event);
                }
                Some(_) => {}
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
                let event = transcript_usage_event(TranscriptUsageEventInput {
                    session_id,
                    transcript_path: transcript_str,
                    today,
                    agg_key: &agg_key,
                    fingerprint: &composite,
                    ts,
                    model_id,
                    input,
                    output,
                    cache_create,
                    cache_read,
                    web_search_requests,
                    cost,
                });

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
                    let current = aggregated_events.entry(agg_key).or_insert(event);
                    current.ts = current.ts.max(ts);
                    current.input_tokens = current.input_tokens.saturating_add(input);
                    current.output_tokens = current.output_tokens.saturating_add(output);
                    current.cache_create_tokens =
                        current.cache_create_tokens.saturating_add(cache_create);
                    current.cache_read_tokens =
                        current.cache_read_tokens.saturating_add(cache_read);
                    current.web_search_requests = current
                        .web_search_requests
                        .saturating_add(web_search_requests);
                    current.cost += cost;
                    if current.model.is_none() {
                        current.model = Some(model_id.to_string());
                    }
                } else {
                    last_seen_raw
                        .insert(agg_key.clone(), (input, output, cache_create, cache_read));
                    match aggregated_events.get_mut(&agg_key) {
                        Some(current) if cost > current.cost => {
                            *current = event;
                        }
                        None => {
                            aggregated_events.insert(agg_key, event);
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }

    Ok(aggregated_events.into_values().collect())
}

/// Parse transcript file to calculate today's cost.
///
/// This wrapper keeps parser assertions focused on the same normalized events
/// used by the main DB path.
#[cfg(test)]
fn parse_transcript_today_cost(transcript_path: &Path, today: &str) -> Result<(f64, usize)> {
    let events = parse_transcript_today_events("transcript-parser", transcript_path, today)?;
    Ok(summarize_usage_events(&events))
}

fn session_usage_for_today(
    session_id: &str,
    transcript_path: &Path,
    today: &str,
    provided_cost: Option<f64>,
    session_entries: Option<&[Entry]>,
) -> Result<(Vec<UsageEvent>, f64, usize)> {
    let transcript_str = transcript_path
        .to_str()
        .context("Invalid transcript path")?;
    let mut events = if let Some(entries) = session_entries {
        usage_events_from_entries(session_id, transcript_str, today, entries)
    } else {
        Vec::new()
    };

    if events.is_empty() {
        if let Some(cost) = provided_cost {
            events.push(synthetic_usage_event(
                session_id,
                transcript_str,
                today,
                cost,
                "scan_summary",
            ));
        } else {
            events = parse_transcript_today_events(session_id, transcript_path, today)?;
        }
    } else {
        events = reconcile_events_with_provided_cost(
            events,
            session_id,
            transcript_str,
            today,
            provided_cost,
        );
    }

    if events.is_empty() {
        events.push(synthetic_usage_event(
            session_id,
            transcript_str,
            today,
            0.0,
            "transcript_empty",
        ));
    }

    let (cost, count) = summarize_usage_events(&events);
    Ok((events, cost, count))
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
    session_entries: Option<&[Entry]>,
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
            sql::SELECT_CACHED_SESSION,
            params![session_key],
            CachedSessionRow::from_row,
        )
        .optional()?;

    // Track whether we modified the DB (to invalidate global sum cache)
    let mut db_was_modified = false;

    let current_session_cost = if let Some(cached_session) = cached {
        // Only use cached cost if both mtime and date match (prevents using yesterday's cost after midnight)
        if cached_session.transcript_mtime == current_mtime && cached_session.today_date == today {
            let events_missing = !has_usage_events_for_session_date(&conn, &session_key, &today)?;
            if cached_session.transcript_path != transcript_str
                || cached_session.session_key != session_key
                || events_missing
            {
                if events_missing || cached_session.transcript_path != transcript_str {
                    let (events, _, _) = session_usage_for_today(
                        &session_key,
                        transcript_path,
                        &today,
                        Some(cached_session.today_cost),
                        session_entries,
                    )?;
                    upsert_session_and_usage_events(
                        &conn,
                        SessionUsageUpdate {
                            session_key: &session_key,
                            transcript_path,
                            mtime: current_mtime,
                            today: &today,
                            cost: cached_session.today_cost,
                            count: cached_session.entry_count,
                            events: &events,
                        },
                    )?;
                } else {
                    upsert_session(
                        &conn,
                        &session_key,
                        transcript_path,
                        current_mtime,
                        &today,
                        cached_session.today_cost,
                        cached_session.entry_count,
                    )?;
                }
                db_was_modified = true;
            }
            cached_session.today_cost
        } else {
            let (events, cost, count) = session_usage_for_today(
                &session_key,
                transcript_path,
                &today,
                session_today_cost,
                session_entries,
            )?;
            upsert_session_and_usage_events(
                &conn,
                SessionUsageUpdate {
                    session_key: &session_key,
                    transcript_path,
                    mtime: current_mtime,
                    today: &today,
                    cost,
                    count,
                    events: &events,
                },
            )?;
            db_was_modified = true;
            cost
        }
    } else {
        let (events, cost, count) = session_usage_for_today(
            &session_key,
            transcript_path,
            &today,
            session_today_cost,
            session_entries,
        )?;
        upsert_session_and_usage_events(
            &conn,
            SessionUsageUpdate {
                session_key: &session_key,
                transcript_path,
                mtime: current_mtime,
                today: &today,
                cost,
                count,
                events: &events,
            },
        )?;
        db_was_modified = true;
        cost
    };

    if conn.execute(sql::DELETE_STALE_SESSIONS, params![today])? > 0 {
        db_was_modified = true;
    }
    if conn.execute(sql::DELETE_STALE_USAGE_EVENTS, params![today])? > 0 {
        db_was_modified = true;
    }

    // Check cache for global sum (5s TTL to reduce redundant SUM queries across concurrent sessions)
    // Skip cache if we just modified the DB (invalidates cache)
    let cache_key = global_sum_cache_key(&today);
    let now = Utc::now().timestamp();
    let cached_sum: Option<GlobalTodayRow> = if !db_was_modified {
        if let Ok(Some(entry)) = get_metadata(&conn, &cache_key) {
            if let Some(updated_at) = entry.updated_at {
                if now - updated_at < GLOBAL_SUM_CACHE_TTL_SECONDS {
                    // Cache is fresh (< 5 seconds old)
                    decode_global_sum_cache(&entry.value)
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

    let (global_today, sessions_count) = if let Some(cached_sum) = cached_sum {
        // Use cached value
        (cached_sum.total_cost, cached_sum.sessions_count)
    } else {
        // Cache miss or expired - run the query
        let row = conn.query_row(
            sql::SELECT_GLOBAL_TODAY,
            params![today],
            GlobalTodayRow::from_row,
        )?;

        // Cache the result for 5 seconds
        let cache_value = encode_global_sum_cache(row.total_cost, row.sessions_count);
        let _ = set_metadata(&conn, &cache_key, &cache_value); // Ignore errors on cache write

        (row.total_cost, row.sessions_count)
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
        .query_row(sql::GET_FRESH_API_CACHE, params![cache_key, now], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;

    Ok(result)
}

/// Get cached API response, ignoring expiration (for stale fallback)
pub fn get_stale_api_cache(cache_key: &str) -> Result<Option<String>> {
    let conn = open_db()?;

    let result = conn
        .query_row(sql::GET_STALE_API_CACHE, params![cache_key], |row| {
            row.get::<_, String>(0)
        })
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
    conn.execute(sql::DELETE_EXPIRED_API_CACHE_KEY, params![cache_key, now])?;

    // INSERT ... ON CONFLICT DO NOTHING -- only the first writer wins
    let rows = conn.execute(
        sql::TRY_INSERT_API_CACHE,
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
        sql::UPSERT_API_CACHE,
        params![cache_key, data, now, expires_at],
    )?;

    // Clean up expired entries, but keep the main usage cache for stale fallback
    conn.execute(
        sql::DELETE_EXPIRED_API_CACHE,
        params![now, OAUTH_USAGE_SUMMARY_CACHE_KEY],
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

        assert_eq!(version, SCHEMA_VERSION_STR);
        assert_eq!(sqlite_user_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(table_has_column(&conn, "sessions", "session_id").unwrap());
        assert!(table_column_is_primary_key(&conn, "sessions", "session_id").unwrap());
        assert!(table_has_column(&conn, "usage_events", "event_key").unwrap());
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_schema_migration_backfills_session_id() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("legacy.db");
        let legacy_conn = Connection::open(&db_path).unwrap();
        legacy_conn
            .execute_batch(
                "CREATE TABLE sessions (
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
                CREATE TABLE metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                INSERT INTO metadata (key, value) VALUES ('schema_version', '1');
                INSERT INTO metadata (key, value) VALUES ('usage_cache_version', '2');",
            )
            .unwrap();
        legacy_conn
            .execute(
                "INSERT INTO sessions (
                    session_key,
                    transcript_path,
                    transcript_mtime,
                    today_date,
                    today_cost,
                    entry_count,
                    last_parsed_at,
                    created_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "legacy-session:/old/project",
                    "/tmp/transcript.jsonl",
                    1,
                    "2025-10-18",
                    1.23,
                    1,
                    1,
                    1,
                    1
                ],
            )
            .unwrap();
        drop(legacy_conn);

        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let session_id: String = conn
            .query_row(
                "SELECT session_id FROM sessions WHERE session_key = ?1",
                params!["legacy-session:/old/project"],
                |row| row.get(0),
            )
            .unwrap();
        let schema_version = get_metadata(&conn, METADATA_KEY_SCHEMA_VERSION)
            .unwrap()
            .unwrap();
        let metadata_has_updated_at = table_has_column(&conn, "metadata", "updated_at").unwrap();
        let usage_event_cost: f64 = conn
            .query_row(
                "SELECT cost FROM usage_events WHERE session_id = ?",
                params!["legacy-session"],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(session_id, "legacy-session");
        assert_eq!(schema_version.value, SCHEMA_VERSION_STR);
        assert_eq!(sqlite_user_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(metadata_has_updated_at);
        assert!(table_column_is_primary_key(&conn, "sessions", "session_id").unwrap());
        assert!((usage_event_cost - 1.23).abs() < 1e-10);
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_schema_migration_collapses_duplicate_session_rows() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("duplicate-legacy.db");
        let legacy_conn = Connection::open(&db_path).unwrap();
        legacy_conn
            .execute_batch(
                "CREATE TABLE sessions (
                    session_key TEXT PRIMARY KEY,
                    session_id TEXT,
                    transcript_path TEXT NOT NULL,
                    transcript_mtime INTEGER NOT NULL,
                    today_date TEXT NOT NULL,
                    today_cost REAL NOT NULL,
                    entry_count INTEGER NOT NULL,
                    last_parsed_at INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL,
                    updated_at INTEGER
                );
                INSERT INTO metadata (key, value, updated_at)
                VALUES ('schema_version', '2', 1);
                INSERT INTO metadata (key, value, updated_at)
                VALUES ('usage_cache_version', '2', 1);",
            )
            .unwrap();
        legacy_conn.pragma_update(None, "user_version", 2).unwrap();
        legacy_conn
            .execute(
                "INSERT INTO sessions (
                    session_key,
                    session_id,
                    transcript_path,
                    transcript_mtime,
                    today_date,
                    today_cost,
                    entry_count,
                    last_parsed_at,
                    created_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    "dup-session:/old/project",
                    "dup-session",
                    "/tmp/old.jsonl",
                    1,
                    "2025-10-18",
                    1.23,
                    1,
                    1,
                    1,
                    1
                ],
            )
            .unwrap();
        legacy_conn
            .execute(
                "INSERT INTO sessions (
                    session_key,
                    session_id,
                    transcript_path,
                    transcript_mtime,
                    today_date,
                    today_cost,
                    entry_count,
                    last_parsed_at,
                    created_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    "dup-session:/new/project",
                    "dup-session",
                    "/tmp/new.jsonl",
                    2,
                    "2025-10-18",
                    2.34,
                    2,
                    2,
                    1,
                    2
                ],
            )
            .unwrap();
        drop(legacy_conn);

        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        let (session_key, transcript_path, cost): (String, String, f64) = conn
            .query_row(
                "SELECT session_key, transcript_path, today_cost FROM sessions WHERE session_id = ?",
                params!["dup-session"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let (event_rows, event_cost): (i64, f64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(cost), 0.0) FROM usage_events WHERE session_id = ?",
                params!["dup-session"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(rows, 1);
        assert_eq!(session_key, "dup-session:/new/project");
        assert_eq!(transcript_path, "/tmp/new.jsonl");
        assert!((cost - 2.34).abs() < 1e-10);
        assert_eq!(event_rows, 1);
        assert!((event_cost - 2.34).abs() < 1e-10);
        assert_eq!(sqlite_user_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(table_column_is_primary_key(&conn, "sessions", "session_id").unwrap());
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_missing_usage_cache_version_does_not_delete_sessions() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("missing-cache-version.db");
        let legacy_conn = Connection::open(&db_path).unwrap();
        legacy_conn
            .execute_batch(
                "CREATE TABLE sessions (
                    session_key TEXT PRIMARY KEY,
                    session_id TEXT,
                    transcript_path TEXT NOT NULL,
                    transcript_mtime INTEGER NOT NULL,
                    today_date TEXT NOT NULL,
                    today_cost REAL NOT NULL,
                    entry_count INTEGER NOT NULL,
                    last_parsed_at INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL,
                    updated_at INTEGER
                );
                INSERT INTO metadata (key, value, updated_at)
                VALUES ('schema_version', '2', 1);",
            )
            .unwrap();
        legacy_conn
            .execute(
                "INSERT INTO sessions (
                    session_key,
                    session_id,
                    transcript_path,
                    transcript_mtime,
                    today_date,
                    today_cost,
                    entry_count,
                    last_parsed_at,
                    created_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    "session-kept",
                    "session-kept",
                    "/tmp/transcript.jsonl",
                    1,
                    "2025-10-18",
                    1.23,
                    1,
                    1,
                    1,
                    1
                ],
            )
            .unwrap();
        drop(legacy_conn);

        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let sessions_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        let usage_cache_version = get_metadata(&conn, METADATA_KEY_USAGE_CACHE_VERSION)
            .unwrap()
            .unwrap();

        assert_eq!(sessions_count, 1);
        assert_eq!(usage_cache_version.value, USAGE_CACHE_VERSION);
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
        let session_id: String = conn
            .query_row(
                "SELECT session_id FROM sessions WHERE session_key = ?",
                params!["sess1:/path/to/project"],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(cost, 1.23);
        assert_eq!(session_id, "sess1");
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_global_usage_path_refresh_preserves_entry_count() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("path-refresh.db");
        let old_transcript_path = temp_dir.path().join("old-transcript.jsonl");
        let new_transcript_path = temp_dir.path().join("new-transcript.jsonl");
        std::fs::write(&new_transcript_path, "{}\n").unwrap();
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let today = Local::now().format("%Y-%m-%d").to_string();
        let current_mtime = fs::metadata(&new_transcript_path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        upsert_session(
            &conn,
            "sess-path-refresh",
            &old_transcript_path,
            current_mtime,
            &today,
            4.56,
            7,
        )
        .unwrap();

        let usage = get_global_usage(
            "sess-path-refresh",
            "/project",
            &new_transcript_path,
            None,
            None,
        )
        .unwrap();
        let (entry_count, transcript_path): (i64, String) = conn
            .query_row(
                "SELECT entry_count, transcript_path FROM sessions WHERE session_key = ?",
                params!["sess-path-refresh"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert!((usage.session_cost - 4.56).abs() < 1e-10);
        assert!((usage.global_today - 4.56).abs() < 1e-10);
        assert_eq!(entry_count, 7);
        assert_eq!(transcript_path, new_transcript_path.display().to_string());
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_global_usage_normalizes_legacy_session_key_on_cache_hit() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("legacy-key-refresh.db");
        let transcript_path = temp_dir.path().join("transcript.jsonl");
        std::fs::write(&transcript_path, "{}\n").unwrap();
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let conn = open_db().unwrap();
        let today = Local::now().format("%Y-%m-%d").to_string();
        let current_mtime = fs::metadata(&transcript_path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        upsert_session(
            &conn,
            "sess-normalize:/old/project",
            &transcript_path,
            current_mtime,
            &today,
            4.56,
            7,
        )
        .unwrap();

        let usage =
            get_global_usage("sess-normalize", "/project", &transcript_path, None, None).unwrap();
        let session_key: String = conn
            .query_row(
                "SELECT session_key FROM sessions WHERE session_id = ?",
                params!["sess-normalize"],
                |row| row.get(0),
            )
            .unwrap();

        assert!((usage.session_cost - 4.56).abs() < 1e-10);
        assert!((usage.global_today - 4.56).abs() < 1e-10);
        assert_eq!(session_key, "sess-normalize");
        unsafe { env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
    }

    #[test]
    #[serial_test::serial]
    fn test_global_usage_writes_usage_events_from_scan_entries() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("events.db");
        let transcript_path = temp_dir.path().join("transcript.jsonl");
        std::fs::write(&transcript_path, "{}\n").unwrap();
        // SAFETY: Test runs serially, no concurrent env access
        unsafe { env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

        let ts = Utc::now();
        let entries = vec![
            Entry {
                ts,
                input: 100,
                output: 200,
                cache_create: 50,
                cache_read: 25,
                web_search_requests: 1,
                speed: None,
                service_tier: None,
                cost: 1.0,
                model: Some("claude-sonnet-4-6".to_string()),
                session_id: Some("event-session".to_string()),
                msg_id: Some("msg-1".to_string()),
                req_id: Some("req-1".to_string()),
                project: Some("project".to_string()),
                agent_id: None,
            },
            Entry {
                ts,
                input: 50,
                output: 100,
                cache_create: 25,
                cache_read: 10,
                web_search_requests: 2,
                speed: None,
                service_tier: None,
                cost: 0.5,
                model: Some("claude-sonnet-4-6".to_string()),
                session_id: Some("event-session".to_string()),
                msg_id: Some("msg-2".to_string()),
                req_id: Some("req-2".to_string()),
                project: Some("project".to_string()),
                agent_id: Some("agent-1".to_string()),
            },
            Entry {
                ts,
                input: 1,
                output: 1,
                cache_create: 1,
                cache_read: 1,
                web_search_requests: 1,
                speed: None,
                service_tier: None,
                cost: 9.0,
                model: Some("claude-opus-4-6".to_string()),
                session_id: Some("other-session".to_string()),
                msg_id: Some("msg-other".to_string()),
                req_id: Some("req-other".to_string()),
                project: Some("project".to_string()),
                agent_id: None,
            },
        ];

        let usage = get_global_usage(
            "event-session",
            "/project",
            &transcript_path,
            Some(1.75),
            Some(&entries),
        )
        .unwrap();

        let conn = open_db().unwrap();
        let (event_rows, cost, input, output, cache_create, cache_read, searches): (
            i64,
            f64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(cost), 0.0),
                        COALESCE(SUM(input_tokens), 0),
                        COALESCE(SUM(output_tokens), 0),
                        COALESCE(SUM(cache_create_tokens), 0),
                        COALESCE(SUM(cache_read_tokens), 0),
                        COALESCE(SUM(web_search_requests), 0)
                 FROM usage_events
                 WHERE session_id = ?",
                params!["event-session"],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();
        let adjustment_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM usage_events WHERE session_id = ? AND source = ?",
                params!["event-session", "scan_adjustment"],
                |row| row.get(0),
            )
            .unwrap();

        assert!((usage.session_cost - 1.75).abs() < 1e-10);
        assert!((usage.global_today - 1.75).abs() < 1e-10);
        assert_eq!(usage.sessions_count, 1);
        assert_eq!(event_rows, 3);
        assert_eq!(adjustment_rows, 1);
        assert!((cost - 1.75).abs() < 1e-10);
        assert_eq!(input, 150);
        assert_eq!(output, 300);
        assert_eq!(cache_create, 75);
        assert_eq!(cache_read, 35);
        assert_eq!(searches, 3);
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
            None,
        )
        .unwrap();
        let second = get_global_usage(
            "sess-moved",
            "/new/project",
            &new_transcript_path,
            Some(1.23),
            None,
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
        let event_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(event_rows, 1);
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

        replace_usage_events_for_session_date(
            &conn,
            "legacy-session",
            &today,
            &[synthetic_usage_event(
                "legacy-session",
                transcript_path.to_str().unwrap(),
                &today,
                2.0,
                "session_summary",
            )],
        )
        .unwrap();

        let usage = get_global_usage(
            "current-session",
            "/current",
            &transcript_path,
            Some(3.0),
            None,
        )
        .unwrap();

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

        let usage = get_global_usage(
            "legacy-session",
            "/current",
            &transcript_path,
            Some(3.0),
            None,
        )
        .unwrap();

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
