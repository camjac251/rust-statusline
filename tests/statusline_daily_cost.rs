use std::io::Write;
use std::process::{Command, Stdio};

use chrono::{Local, Utc};
use serde_json::json;
use serial_test::serial;
use tempfile::TempDir;

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for seq_ch in chars.by_ref() {
                if ('@'..='~').contains(&seq_ch) {
                    break;
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

#[test]
#[serial]
fn warmed_db_fast_path_does_not_count_cross_day_session_total_as_today() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("statusline.db");
    let transcript_path = temp_dir.path().join("transcript.jsonl");
    let session_id = "cross-day-statusline-session";
    let yesterday = Local::now() - chrono::TimeDelta::days(1);

    let usage_line = json!({
        "type": "assistant",
        "sessionId": session_id,
        "timestamp": yesterday.to_rfc3339(),
        "message": {
            "role": "assistant",
            "id": "msg-cross-day-statusline",
            "model": "claude-sonnet-4-6",
            "usage": {
                "input_tokens": 1_000,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }
    });
    let result_line = json!({
        "type": "result",
        "sessionId": session_id,
        "timestamp": Local::now().to_rfc3339(),
        "total_cost_usd": 1.0
    });
    std::fs::write(
        &transcript_path,
        format!("{}\n{}\n", usage_line, result_line),
    )
    .unwrap();

    unsafe { std::env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };
    claude_statusline::db::store_metadata(
        "usage_scan:last_full_scan_at",
        &Utc::now().timestamp().to_string(),
    )
    .unwrap();

    let hook = json!({
        "session_id": session_id,
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
        "output_style": {
            "name": "default"
        },
        "cost": {
            "total_cost_usd": 1.0,
            "total_duration_ms": 1000,
            "total_api_duration_ms": 1000,
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
        "thinking": {
            "enabled": false
        }
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_claude_statusline"))
        .args([
            "--no-subsystem-git",
            "--no-subsystem-beads",
            "--no-subsystem-gastown",
            "--no-subsystem-usage-api",
            "--no-usage-five-hour",
            "--no-usage-weekly",
            "--no-usage-opus",
            "--no-usage-sonnet",
            "--no-usage-extra",
            "--no-context-tokens",
            "--no-context-percent",
            "--no-cost-window",
        ])
        .env("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap())
        .env("CLAUDE_TERMINAL_WIDTH", "320")
        .env("LINES", "32")
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
    unsafe { std::env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };

    assert!(
        output.status.success(),
        "statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = strip_ansi(&String::from_utf8_lossy(&output.stdout));
    assert!(
        stdout.contains("session:$1.00"),
        "expected session total in output: {stdout}"
    );
    assert!(
        stdout.contains("today:$0.00"),
        "expected cross-day session total to stay out of today: {stdout}"
    );
}

#[test]
#[serial]
fn clodex_gpt_56_sol_renders_friendly_name_and_current_day_cost() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("statusline.db");
    let transcript_path = temp_dir.path().join("transcript.jsonl");
    let session_id = "clodex-gpt-56-statusline-session";
    let model_id = "clodex:openai-oauth:gpt-5.6-sol";

    let usage_line = json!({
        "type": "assistant",
        "sessionId": session_id,
        "timestamp": Local::now().to_rfc3339(),
        "message": {
            "role": "assistant",
            "id": "msg-clodex-gpt-56",
            "model": model_id,
            "usage": {
                "input_tokens": 200_000,
                "output_tokens": 100_000,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }
    });
    std::fs::write(&transcript_path, format!("{usage_line}\n")).unwrap();

    unsafe { std::env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };
    claude_statusline::db::store_metadata(
        "usage_scan:last_full_scan_at",
        &Utc::now().timestamp().to_string(),
    )
    .unwrap();

    let hook = json!({
        "session_id": session_id,
        "transcript_path": transcript_path,
        "model": {
            "id": model_id,
            "display_name": model_id
        },
        "workspace": {
            "current_dir": temp_dir.path(),
            "project_dir": temp_dir.path()
        },
        "version": "2.1.220",
        "output_style": {
            "name": "default"
        },
        "cost": {
            "total_cost_usd": 4.0,
            "total_duration_ms": 1000,
            "total_api_duration_ms": 1000,
            "total_lines_added": 0,
            "total_lines_removed": 0
        },
        "context_window": {
            "total_input_tokens": 200000,
            "total_output_tokens": 100000,
            "context_window_size": 272000,
            "current_usage": null,
            "used_percentage": 0,
            "remaining_percentage": 100
        },
        "exceeds_200k_tokens": false,
        "fast_mode": false,
        "thinking": {
            "enabled": false
        }
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_claude_statusline"))
        .args([
            "--no-subsystem-git",
            "--no-subsystem-beads",
            "--no-subsystem-gastown",
            "--no-subsystem-usage-api",
            "--no-usage-five-hour",
            "--no-usage-weekly",
            "--no-usage-opus",
            "--no-usage-sonnet",
            "--no-usage-extra",
            "--no-context-tokens",
            "--no-context-percent",
            "--no-cost-window",
        ])
        .env("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap())
        .env("CLAUDE_TERMINAL_WIDTH", "320")
        .env("LINES", "32")
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
    unsafe { std::env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };

    assert!(
        output.status.success(),
        "statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = strip_ansi(&String::from_utf8_lossy(&output.stdout));
    assert!(
        stdout.contains("GPT-5.6 Sol"),
        "expected friendly model name in output: {stdout}"
    );
    assert!(
        !stdout.contains(model_id),
        "raw clodex model id leaked into output: {stdout}"
    );
    assert!(
        stdout.contains("session:$4.00"),
        "expected session total in output: {stdout}"
    );
    assert!(
        stdout.contains("today:$4.00"),
        "expected current-day clodex usage in today total: {stdout}"
    );
}
