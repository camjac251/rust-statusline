use std::io::Write;
use std::process::{Command, Stdio};

use chrono::Utc;
use claude_statusline::usage_api::{
    UsageApiLimit, UsageLimit, UsageLimitScope, UsageLimitScopeModel, UsageSummary,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Run the statusline in JSON mode with only hook-provided rate limits (OAuth
/// disabled), returning the full parsed output. Resets are seconds since the
/// epoch; `seven_day` is an optional `(used_percentage, reset_epoch)` pair.
fn run_statusline(
    five_hour_reset: i64,
    five_hour_pct: f64,
    seven_day: Option<(f64, i64)>,
) -> Value {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("statusline.db");
    let transcript_path = temp_dir.path().join("transcript.jsonl");
    std::fs::write(&transcript_path, "").unwrap();

    let mut rate_limits = json!({
        "five_hour": { "used_percentage": five_hour_pct, "resets_at": five_hour_reset as f64 }
    });
    if let Some((pct, reset)) = seven_day {
        rate_limits["seven_day"] = json!({ "used_percentage": pct, "resets_at": reset as f64 });
    }

    let hook = json!({
        "session_id": "usage-reset-stale-session",
        "transcript_path": transcript_path,
        "model": {
            "id": "claude-sonnet-4-6",
            "display_name": "Claude Sonnet 4.6"
        },
        "workspace": {
            "current_dir": temp_dir.path(),
            "project_dir": temp_dir.path()
        },
        "version": "2.5.4",
        "output_style": {"name": "default"},
        "cost": {
            "total_cost_usd": 0.0,
            "total_duration_ms": 0,
            "total_api_duration_ms": 0,
            "total_lines_added": 0,
            "total_lines_removed": 0
        },
        "context_window": {
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "context_window_size": 200000,
            "current_usage": null,
            "used_percentage": 0,
            "remaining_percentage": 100
        },
        "exceeds_200k_tokens": false,
        "fast_mode": false,
        "thinking": {"enabled": false},
        "rate_limits": rate_limits
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_claude_statusline"))
        .args([
            "--json",
            "--no-subsystem-git",
            "--no-subsystem-beads",
            "--no-subsystem-gastown",
            "--no-subsystem-usage-api",
            "--no-subsystem-db-cache",
            "--claude-config-dir",
            temp_dir.path().to_str().unwrap(),
        ])
        .env("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap())
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(hook.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON: {e}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn window(parsed: &Value) -> Value {
    parsed
        .get("window")
        .cloned()
        .unwrap_or_else(|| panic!("no window object in output: {parsed}"))
}

fn seed_api_cache(db_path: &std::path::Path, entries: &[(&str, &str, i64)]) {
    let connection = Connection::open(db_path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE api_cache (
                cache_key TEXT PRIMARY KEY,
                data TEXT NOT NULL,
                fetched_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );",
        )
        .unwrap();
    let now = Utc::now().timestamp();
    for (key, data, ttl_seconds) in entries {
        connection
            .execute(
                "INSERT INTO api_cache (cache_key, data, fetched_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![key, data, now, now + ttl_seconds],
            )
            .unwrap();
    }
}

fn fable_summary(fetched_at: chrono::DateTime<Utc>) -> UsageSummary {
    let reset = Utc::now() + chrono::TimeDelta::hours(2);
    UsageSummary {
        window: UsageLimit {
            utilization: Some(88.0),
            used: Some(8.0),
            remaining: Some(2.0),
            limit: Some(10.0),
            resets_at: Some(reset),
        },
        seven_day: UsageLimit {
            utilization: Some(55.0),
            used: Some(7.0),
            remaining: Some(3.0),
            limit: Some(10.0),
            resets_at: Some(Utc::now() + chrono::TimeDelta::days(2)),
        },
        limits: vec![UsageApiLimit {
            kind: Some("weekly_scoped".to_string()),
            group: Some("weekly".to_string()),
            percent: Some(24.0),
            scope: Some(UsageLimitScope {
                model: Some(UsageLimitScopeModel {
                    id: None,
                    display_name: Some("Fable".to_string()),
                }),
                surface: None,
            }),
            ..UsageApiLimit::default()
        }],
        codename_limits: std::collections::BTreeMap::from([(
            "synthetic_limit".to_string(),
            UsageLimit {
                utilization: Some(12.0),
                ..UsageLimit::default()
            },
        )]),
        member_dashboard_available: Some(true),
        unknown_fields: vec!["synthetic_field".to_string()],
        fetched_at: Some(fetched_at),
        ..UsageSummary::default()
    }
}

fn run_statusline_with_cached_api(
    rate_limits: Value,
    summary: UsageSummary,
    cache_ttl_seconds: i64,
    negative_cache: bool,
) -> Value {
    run_statusline_with_cached_api_for_model(
        rate_limits,
        summary,
        cache_ttl_seconds,
        negative_cache,
        "claude-sonnet-4-6",
        "Claude Sonnet 4.6",
    )
}

fn run_statusline_with_cached_api_for_model(
    rate_limits: Value,
    summary: UsageSummary,
    cache_ttl_seconds: i64,
    negative_cache: bool,
    model_id: &str,
    model_display_name: &str,
) -> Value {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("statusline.db");
    let transcript_path = temp_dir.path().join("transcript.jsonl");
    std::fs::write(&transcript_path, "").unwrap();

    let encoded = serde_json::to_string(&summary).unwrap();
    let mut cache_entries = vec![("oauth_usage_summary", encoded.as_str(), cache_ttl_seconds)];
    if negative_cache {
        cache_entries.push(("oauth_usage_negative", "1", 300));
    }
    seed_api_cache(&db_path, &cache_entries);

    let hook = json!({
        "session_id": "usage-api-cache-session",
        "transcript_path": transcript_path,
        "model": {
            "id": model_id,
            "display_name": model_display_name
        },
        "workspace": {
            "current_dir": temp_dir.path(),
            "project_dir": temp_dir.path()
        },
        "version": "2.5.4",
        "output_style": {"name": "default"},
        "cost": {
            "total_cost_usd": 0.0,
            "total_duration_ms": 0,
            "total_api_duration_ms": 0,
            "total_lines_added": 0,
            "total_lines_removed": 0
        },
        "context_window": {
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "context_window_size": 200000,
            "current_usage": null,
            "used_percentage": 0,
            "remaining_percentage": 100
        },
        "exceeds_200k_tokens": false,
        "fast_mode": false,
        "thinking": {"enabled": false},
        "rate_limits": rate_limits
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_claude_statusline"))
        .args([
            "--json",
            "--no-config",
            "--no-subsystem-git",
            "--no-subsystem-beads",
            "--no-subsystem-gastown",
            "--no-subsystem-db-cache",
            "--claude-config-dir",
            temp_dir.path().to_str().unwrap(),
        ])
        .env("CLAUDE_STATUSLINE_DB_PATH", &db_path)
        .env("CLAUDE_CONFIG_DIR", temp_dir.path())
        .env("NO_COLOR", "1")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("https_proxy", "http://127.0.0.1:9")
        .env("http_proxy", "http://127.0.0.1:9")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("CLAUDE_STATUSLINE_CONFIG")
        .env_remove("CLAUDE_STATUSLINE_PRESET")
        .env_remove("CLAUDE_STATUSLINE_JSON_NO_USAGE_LIMITS")
        .env_remove("CLAUDE_STATUSLINE_SUBSYSTEM_NO_USAGE_API")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(hook.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON: {error}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn future_reset_shows_live_percentage_not_stale() {
    let reset = Utc::now().timestamp() + 3600; // one hour out
    let window = window(&run_statusline(reset, 44.0, None));

    assert_eq!(window["usage_percent"], json!(44.0));
    assert_eq!(
        window["usage_stale"],
        json!(false),
        "a window that has not reset must not be marked stale: {window}"
    );
}

#[test]
fn elapsed_reset_marks_percentage_stale() {
    // The hook keeps resending the same snapshot after the window has rolled over.
    // With no live source to refresh it, the percentage is retained but flagged
    // stale so the label renders with a "~" prefix rather than a frozen value.
    let reset = Utc::now().timestamp() - 600; // ten minutes past reset
    let window = window(&run_statusline(reset, 44.0, None));

    assert_eq!(window["usage_percent"], json!(44.0));
    assert_eq!(
        window["usage_stale"],
        json!(true),
        "a window past its reset with no live source must be marked stale: {window}"
    );
}

#[test]
fn seven_day_future_reset_keeps_percentage() {
    let five = Utc::now().timestamp() + 3600;
    let seven = Utc::now().timestamp() + 86_400; // one day out
    let parsed = run_statusline(five, 44.0, Some((74.0, seven)));

    assert_eq!(
        parsed["usage_limits"]["seven_day"]["utilization"],
        json!(74.0)
    );
}

#[test]
fn fresh_api_enrichment_preserves_metadata_and_hook_precedence() {
    let now = Utc::now();
    let fetched_at = now - chrono::TimeDelta::seconds(60);
    let parsed = run_statusline_with_cached_api(
        json!({
            "five_hour": {
                "used_percentage": 44.0,
                "resets_at": (now.timestamp() + 3600) as f64
            }
        }),
        fable_summary(fetched_at),
        300,
        false,
    );

    assert_eq!(window(&parsed)["usage_percent"], json!(44.0));
    assert_eq!(
        parsed["usage_limits"]["fetched_at"],
        json!(fetched_at.to_rfc3339())
    );
    assert_eq!(
        parsed["usage_limits"]["limits"][0]["scope"]["model"]["display_name"],
        json!("Fable")
    );
    assert_eq!(
        parsed["usage_limits"]["codename_limits"]["synthetic_limit"]["utilization"],
        json!(12.0)
    );
    assert_eq!(
        parsed["usage_limits"]["member_dashboard_available"],
        json!(true)
    );
    assert_eq!(
        parsed["usage_limits"]["unknown_fields"],
        json!(["synthetic_field"])
    );
}

#[test]
fn fresh_api_fills_missing_hook_percentage() {
    let now = Utc::now();
    let hook_reset = now.timestamp() + 3600;
    let parsed = run_statusline_with_cached_api(
        json!({
            "five_hour": {
                "used_percentage": null,
                "resets_at": hook_reset as f64
            }
        }),
        fable_summary(now),
        300,
        false,
    );
    let expected_reset = claude_statusline::usage::normalize_reset_time(
        chrono::DateTime::from_timestamp(hook_reset, 0).unwrap(),
    );

    assert_eq!(window(&parsed)["usage_percent"], json!(88.0));
    assert_eq!(parsed["reset_at"], json!(expected_reset.to_rfc3339()));
}

#[test]
fn fresh_api_fills_missing_hook_reset() {
    let now = Utc::now();
    let summary = fable_summary(now);
    let api_reset = summary.window.resets_at.as_ref().unwrap().to_owned();
    let parsed = run_statusline_with_cached_api(
        json!({
            "five_hour": {
                "used_percentage": 44.0,
                "resets_at": null
            }
        }),
        summary,
        300,
        false,
    );
    let expected_reset = claude_statusline::usage::normalize_reset_time(api_reset);

    assert_eq!(window(&parsed)["usage_percent"], json!(44.0));
    assert_eq!(parsed["reset_at"], json!(expected_reset.to_rfc3339()));
}

#[test]
fn fresh_api_replaces_elapsed_hook_window() {
    let now = Utc::now();
    let summary = fable_summary(now);
    let api_reset = summary.window.resets_at.as_ref().unwrap().to_owned();
    let parsed = run_statusline_with_cached_api(
        json!({
            "five_hour": {
                "used_percentage": 44.0,
                "resets_at": (now.timestamp() - 600) as f64
            }
        }),
        summary,
        300,
        false,
    );
    let expected_reset = claude_statusline::usage::normalize_reset_time(api_reset);

    assert_eq!(window(&parsed)["usage_percent"], json!(88.0));
    assert_eq!(window(&parsed)["usage_stale"], json!(false));
    assert_eq!(parsed["reset_at"], json!(expected_reset.to_rfc3339()));
}

#[test]
fn fresh_api_fills_seven_day_fields_without_overriding_hook() {
    let now = Utc::now();
    let five_hour_reset = now.timestamp() + 3600;
    let seven_day_reset = now.timestamp() + 86_400;
    let parsed = run_statusline_with_cached_api(
        json!({
            "five_hour": {
                "used_percentage": 44.0,
                "resets_at": five_hour_reset as f64
            },
            "seven_day": {
                "used_percentage": 74.0,
                "resets_at": seven_day_reset as f64
            }
        }),
        fable_summary(now),
        300,
        false,
    );
    let seven_day = &parsed["usage_limits"]["seven_day"];
    let expected_reset = chrono::DateTime::from_timestamp(seven_day_reset, 0).unwrap();

    assert_eq!(seven_day["utilization"], json!(74.0));
    assert_eq!(seven_day["resets_at"], json!(expected_reset.to_rfc3339()));
    assert_eq!(seven_day["used"], json!(7.0));
    assert_eq!(seven_day["remaining"], json!(3.0));
    assert_eq!(seven_day["limit"], json!(10.0));
}

/// A stale response must not move the hook's five-hour window, but the scoped
/// rows only exist in that response. Dropping them blinks `fable:` out of the
/// line for as long as a fetch is locked or backed off, while the hook-sourced
/// `usage:` and `7d:` tokens beside it stay put.
#[test]
fn stale_api_keeps_scoped_rows_beside_a_fresh_hook_window() {
    let now = Utc::now();
    let parsed = run_statusline_with_cached_api(
        json!({
            "five_hour": {
                "used_percentage": 44.0,
                "resets_at": (now.timestamp() + 3600) as f64
            }
        }),
        fable_summary(now - chrono::TimeDelta::hours(1)),
        0,
        true,
    );

    assert_eq!(window(&parsed)["usage_percent"], json!(44.0));
    assert_eq!(window(&parsed)["usage_stale"], json!(false));
    assert_eq!(
        parsed["usage_limits"]["limits"][0]["scope"]["model"]["display_name"],
        json!("Fable")
    );
    assert_eq!(parsed["usage_limits"]["limits"][0]["percent"], json!(24.0));
}

#[test]
fn stale_api_cannot_replace_elapsed_hook_window() {
    let now = Utc::now();
    let parsed = run_statusline_with_cached_api(
        json!({
            "five_hour": {
                "used_percentage": 44.0,
                "resets_at": (now.timestamp() - 600) as f64
            }
        }),
        fable_summary(now - chrono::TimeDelta::hours(1)),
        0,
        true,
    );

    assert_eq!(window(&parsed)["usage_percent"], json!(44.0));
    assert_eq!(window(&parsed)["usage_stale"], json!(true));
    assert_eq!(
        parsed["usage_limits"]["limits"][0]["scope"]["model"]["display_name"],
        json!("Fable")
    );
}

/// The subscription rows describe the account, not the turn in flight. A mixed
/// launcher routing one reply through a third-party model is still spending the
/// same subscription, so the cluster must not blank until the next Claude reply.
#[test]
fn subscription_rows_survive_a_third_party_model_turn() {
    let now = Utc::now();
    let rate_limits = json!({
        "five_hour": {
            "used_percentage": 44.0,
            "resets_at": (now.timestamp() + 3600) as f64
        }
    });

    let on_claude = run_statusline_with_cached_api_for_model(
        rate_limits.clone(),
        fable_summary(now),
        300,
        false,
        "claude-sonnet-4-6",
        "Claude Sonnet 4.6",
    );
    let on_third_party = run_statusline_with_cached_api_for_model(
        rate_limits,
        fable_summary(now),
        300,
        false,
        "clodex:openai-oauth:gpt-5.6-sol",
        "GPT-5.6 Sol",
    );

    assert_eq!(
        on_third_party["usage_limits"]["limits"],
        on_claude["usage_limits"]["limits"]
    );
    assert_eq!(
        on_third_party["usage_limits"]["limits"][0]["scope"]["model"]["display_name"],
        json!("Fable")
    );
    assert_eq!(window(&on_third_party)["usage_percent"], json!(44.0));
}

#[test]
fn stale_api_without_hook_remains_available_and_marked_stale() {
    let now = Utc::now();
    let fetched_at = now - chrono::TimeDelta::hours(1);
    let parsed = run_statusline_with_cached_api(Value::Null, fable_summary(fetched_at), 0, true);

    assert_eq!(window(&parsed)["usage_percent"], json!(88.0));
    assert_eq!(window(&parsed)["usage_stale"], json!(true));
    assert_eq!(
        parsed["usage_limits"]["fetched_at"],
        json!(fetched_at.to_rfc3339())
    );
    assert_eq!(
        parsed["usage_limits"]["limits"][0]["scope"]["model"]["display_name"],
        json!("Fable")
    );
}

#[test]
fn seven_day_elapsed_reset_defers_instead_of_freezing() {
    // Same idle-snapshot staleness as five_hour: once the weekly window has
    // reset, the frozen hook percentage is dropped in favor of the live OAuth
    // value. With OAuth disabled here, the stale weekly percentage is omitted
    // rather than shown, so its utilization is null.
    let five = Utc::now().timestamp() + 3600; // five-hour still fresh
    let seven = Utc::now().timestamp() - 600; // weekly window already reset
    let parsed = run_statusline(five, 44.0, Some((74.0, seven)));

    assert_eq!(
        parsed["usage_limits"]["seven_day"]["utilization"],
        Value::Null
    );
    // The unaffected five-hour percentage still shows.
    assert_eq!(window(&parsed)["usage_percent"], json!(44.0));
}
