use std::io::Write;
use std::process::{Command, Stdio};

use chrono::Utc;
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
