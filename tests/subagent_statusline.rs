use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use claude_statusline::subagent_statusline::{SubagentEffort, SubagentStatusInput};
use tempfile::TempDir;

fn epoch_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn run_statusline(args: &[&str], stdin_payload: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_claude_statusline"))
        .args(args)
        .env("NO_COLOR", "1")
        .env("CLAUDE_TERMINAL_WIDTH", "320")
        .env("LINES", "32")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// Same as `run_statusline` but with colors left on and the palette-detection
/// environment pinned, so a test states exactly which signals are present.
#[cfg(feature = "colors")]
fn run_statusline_colored(env: &[(&str, &str)], stdin_payload: &str) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_claude_statusline"));
    command
        .env_remove("NO_COLOR")
        .env_remove("COLORTERM")
        .env_remove("CLAUDE_TRUECOLOR")
        .env("TERM", "xterm")
        .env("CLAUDE_TERMINAL_WIDTH", "320")
        .env("LINES", "32");
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// One task per payload kind Claude Code's task registry can emit; only
/// `local_agent` arrives in practice, the rest exercise defensive tolerance.
/// The non-agent rows drop the optional fields (name, model, effort,
/// contextWindowSize, tokenSamples, cwd, and even tokenCount).
fn five_kind_payload(start_time: u64) -> Value {
    json!({
        "session_id": "session-test",
        "transcript_path": "/repo/session-test.jsonl",
        "cwd": "/repo",
        "prompt_id": "prompt-1",
        "columns": 120,
        "tasks": [
            {
                "id": "agent-1",
                "name": "Explore",
                "type": "local_agent",
                "status": "running",
                "description": "Inspect usage accounting",
                "label": "tracing transcript entries",
                "startTime": start_time,
                "model": "claude-opus-4-8",
                "effort": "high",
                "contextWindowSize": 1_000_000,
                "tokenCount": 48_213,
                "tokenSamples": [12_040, 25_890, 48_213],
                "cwd": "/repo",
                "toolUseId": "tu_1",
                "evictAfter": 1_753_238_500_000u64
            },
            {
                "id": "bash-1",
                "type": "local_bash",
                "status": "running",
                "description": "run the test suite",
                "label": "run the test suite",
                "startTime": start_time
            },
            {
                "id": "workflow-1",
                "type": "local_workflow",
                "status": "paused",
                "description": "review workflow",
                "label": "review workflow",
                "startTime": start_time,
                "effort": 2
            },
            {
                "id": "remote-1",
                "type": "remote_agent",
                "status": "pending",
                "description": "build the thing",
                "label": "build the thing",
                "startTime": start_time
            },
            {
                "id": "teammate-1",
                "type": "in_process_teammate",
                "status": "completed",
                "description": "triage findings",
                "label": "triage findings",
                "startTime": start_time
            }
        ]
    })
}

#[test]
fn parses_every_task_kind_and_tolerates_missing_optional_fields() {
    let input: SubagentStatusInput =
        serde_json::from_value(five_kind_payload(1_753_238_400_123)).unwrap();

    assert_eq!(input.columns, 120);
    assert_eq!(input.tasks.len(), 5);

    let agent = &input.tasks[0];
    assert_eq!(agent.id, "agent-1");
    assert_eq!(agent.task_type, "local_agent");
    assert_eq!(agent.name.as_deref(), Some("Explore"));
    assert_eq!(agent.status, "running");
    assert_eq!(agent.start_time, 1_753_238_400_123);
    assert_eq!(agent.model.as_deref(), Some("claude-opus-4-8"));
    assert!(matches!(agent.effort, Some(SubagentEffort::Label(ref level)) if level == "high"));
    assert_eq!(agent.context_window_size, Some(1_000_000));
    assert_eq!(agent.token_count, 48_213);
    assert_eq!(agent.token_samples, vec![12_040, 25_890, 48_213]);
    assert_eq!(agent.cwd.as_deref(), Some("/repo"));

    let bash = &input.tasks[1];
    assert_eq!(bash.task_type, "local_bash");
    assert!(bash.name.is_none());
    assert!(bash.model.is_none());
    assert!(bash.effort.is_none());
    assert!(bash.context_window_size.is_none());
    assert_eq!(bash.token_count, 0);
    assert!(bash.token_samples.is_empty());
    assert!(bash.cwd.is_none());

    let workflow = &input.tasks[2];
    assert_eq!(workflow.task_type, "local_workflow");
    assert_eq!(workflow.status, "paused");
    assert!(matches!(workflow.effort, Some(SubagentEffort::Level(2))));

    assert_eq!(input.tasks[3].task_type, "remote_agent");
    assert_eq!(input.tasks[3].status, "pending");
    assert_eq!(input.tasks[4].task_type, "in_process_teammate");
    assert_eq!(input.tasks[4].status, "completed");
}

#[test]
fn renders_subagent_statusline_jsonl_from_task_payload() {
    let start_time = epoch_ms_now();
    let payload = json!({
        "session_id": "session-test",
        "transcript_path": "/repo/session-test.jsonl",
        "cwd": "/repo",
        "agent_type": "main-session",
        "columns": 120,
        "tasks": [{
            "id": "agent-test",
            "name": "Explore",
            "type": "local_agent",
            "status": "running",
            "description": "Inspect usage accounting",
            "label": "tracing transcript entries",
            "startTime": start_time,
            "model": "claude-opus-4-8",
            "effort": "high",
            "contextWindowSize": 1_000_000,
            "tokenCount": 48_213,
            "tokenSamples": [12_040, 25_890, 48_213],
            "cwd": "/repo"
        }]
    });

    let output = run_statusline(&[], &payload.to_string());
    assert!(
        output.status.success(),
        "subagent statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1);
    let row: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(row["id"], "agent-test");
    let content = row["content"].as_str().unwrap();
    assert!(content.contains("Explore"), "content: {content}");
    assert!(content.contains("Opus 4.8"), "content: {content}");
    assert!(content.contains("48.2K/1M"), "content: {content}");
    assert!(content.contains("5%"), "content: {content}");
    assert!(content.contains("217.0K/m"), "content: {content}");
    assert!(
        content.contains("tracing transcript entries"),
        "content: {content}"
    );
}

#[test]
fn emits_one_decoration_line_per_task_mapped_by_id() {
    let payload = five_kind_payload(epoch_ms_now());
    let output = run_statusline(&[], &payload.to_string());
    assert!(
        output.status.success(),
        "subagent statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 5);

    let ids: Vec<&str> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        ["agent-1", "bash-1", "workflow-1", "remote-1", "teammate-1"]
    );
    for row in &rows {
        let content = row["content"].as_str().unwrap();
        assert!(!content.is_empty(), "row: {row}");
    }

    // Rows without a registered name fall back to a task-kind label.
    let bash_content = rows[1]["content"].as_str().unwrap();
    assert!(bash_content.contains("bash"), "content: {bash_content}");
    let teammate_content = rows[4]["content"].as_str().unwrap();
    assert!(
        teammate_content.contains("in process teammate"),
        "content: {teammate_content}"
    );
}

#[test]
fn json_flag_emits_structured_tasks_payload() {
    let start_time = epoch_ms_now();
    let payload = five_kind_payload(start_time);
    let output = run_statusline(&["--json"], &payload.to_string());
    assert!(
        output.status.success(),
        "subagent statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1);
    let json_output: Value = serde_json::from_str(lines[0]).unwrap();
    let tasks = json_output["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 5);

    let agent = &tasks[0];
    assert_eq!(agent["id"], "agent-1");
    assert_eq!(agent["task_type"], "local_agent");
    assert_eq!(agent["name"], "Explore");
    assert_eq!(agent["status"], "running");
    assert_eq!(agent["start_time"], start_time);
    assert_eq!(agent["model"], "claude-opus-4-8");
    assert_eq!(agent["effort"], "high");
    assert_eq!(agent["context_window_size"], 1_000_000);
    assert_eq!(agent["token_count"], 48_213);
    assert_eq!(agent["token_samples"], json!([12_040, 25_890, 48_213]));
    // Two five-second gaps carrying 36,173 tokens derive the burn rate.
    assert_eq!(agent["tokens_per_minute"], 217_038.0);
    assert_eq!(agent["cwd"], "/repo");

    // The integer effort form passes through unchanged.
    assert_eq!(tasks[2]["effort"], 2);

    // Absent optional fields are omitted rather than serialized as null; a
    // task without enough token samples omits the derived rate the same way.
    let remote = &tasks[3];
    assert_eq!(remote["task_type"], "remote_agent");
    for absent in [
        "name",
        "model",
        "effort",
        "context_window_size",
        "cwd",
        "tokens_per_minute",
    ] {
        assert!(remote.get(absent).is_none(), "field: {absent}");
    }
}

#[test]
fn statusline_hook_payload_still_routes_to_text_output() {
    let temp_dir = TempDir::new().unwrap();
    let hook = json!({
        "session_id": "dispatch-test-session",
        "transcript_path": temp_dir.path().join("transcript.jsonl"),
        "model": {
            "id": "claude-sonnet-4-6",
            "display_name": "Claude Sonnet 4.6"
        },
        "workspace": {
            "current_dir": temp_dir.path(),
            "project_dir": temp_dir.path()
        },
        "version": "2.5.4",
        "output_style": { "name": "default" },
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
        "thinking": { "enabled": false }
    });

    let claude_dir = temp_dir.path().to_str().unwrap().to_string();
    let output = run_statusline(
        &[
            "--no-config",
            "--no-subsystem-git",
            "--no-subsystem-beads",
            "--no-subsystem-gastown",
            "--no-subsystem-db-cache",
            "--no-subsystem-usage-api",
            "--claude-config-dir",
            &claude_dir,
        ],
        &hook.to_string(),
    );
    assert!(
        output.status.success(),
        "statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("session:$1.00"),
        "expected text statusline output: {stdout}"
    );
    for line in stdout.lines() {
        let decoration_shaped = serde_json::from_str::<Value>(line)
            .is_ok_and(|value| value.get("id").is_some() && value.get("content").is_some());
        assert!(!decoration_shaped, "line rendered as a decoration: {line}");
    }
}

/// Claude Code discards every decoration for the tick when the command exits
/// non-zero, so one task it sends in an unexpected shape must not silence the
/// rest of the panel.
#[test]
fn one_unreadable_task_does_not_drop_its_siblings() {
    let start_time = epoch_ms_now();
    let payload = json!({
        "columns": 120,
        "tasks": [
            {
                "id": "good-1",
                "name": "Explore",
                "type": "local_agent",
                "status": "running",
                "startTime": start_time,
                "tokenCount": 1_000
            },
            {
                // The object form the main statusline hook uses for effort.
                "id": "object-effort",
                "name": "Plan",
                "type": "local_agent",
                "status": "running",
                "startTime": start_time,
                "effort": { "level": "high" },
                "tokenCount": 2_000
            },
            {
                // No id, so there is nothing to key a decoration to.
                "type": "local_agent",
                "status": "running",
                "startTime": start_time
            },
            {
                "id": "good-2",
                "name": "Review",
                "type": "local_agent",
                "status": "running",
                "startTime": start_time,
                "tokenCount": 3_000
            }
        ]
    });

    let output = run_statusline(&[], &payload.to_string());
    assert!(
        output.status.success(),
        "subagent statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let ids: Vec<String> = stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    // The unreadable row keeps Claude Code's default rendering; the object-form
    // effort parses and simply renders no chip.
    assert_eq!(ids, ["good-1", "object-effort", "good-2"]);
    assert!(stdout.contains("Explore"), "stdout: {stdout}");
    assert!(stdout.contains("Review"), "stdout: {stdout}");
}

#[test]
fn missing_columns_falls_back_instead_of_failing() {
    let payload = json!({
        "tasks": [{
            "id": "solo",
            "name": "Explore",
            "type": "local_agent",
            "status": "running",
            "startTime": epoch_ms_now(),
            "tokenCount": 1_234
        }]
    });

    let output = run_statusline(&[], &payload.to_string());
    assert!(
        output.status.success(),
        "subagent statusline failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Explore"), "stdout: {stdout}");
    assert!(stdout.contains("1.2K"), "stdout: {stdout}");
}

/// An effort tier this build does not know still renders. A patched Claude Code
/// that grows a level should show it rather than silently drop the chip.
#[test]
fn unrecognized_effort_tier_still_renders_a_chip() {
    let payload = json!({
        "columns": 120,
        "tasks": [{
            "id": "agent-1",
            "name": "Explore",
            "type": "local_agent",
            "status": "running",
            "startTime": epoch_ms_now(),
            "model": "claude-opus-4-8",
            "effort": "turbo",
            "tokenCount": 1_000
        }]
    });

    let output = run_statusline(&[], &payload.to_string());
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("turbo"), "stdout: {stdout}");
}

/// The agent panel and the footer are one binary in one terminal, so they must
/// resolve the same palette from the same signals.
#[cfg(feature = "colors")]
#[test]
fn agent_rows_use_the_same_truecolor_detection_as_the_footer() {
    let payload = json!({
        "columns": 120,
        "tasks": [{
            "id": "agent-1",
            "name": "Explore",
            "type": "local_agent",
            "status": "running",
            "startTime": epoch_ms_now(),
            "tokenCount": 1_000
        }]
    });
    let payload = payload.to_string();

    for env in [
        [("COLORTERM", "truecolor")],
        [("CLAUDE_TRUECOLOR", "1")],
        [("TERM", "xterm-256color")],
    ] {
        let output = run_statusline_colored(&env, &payload);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("38;2;"),
            "expected truecolor from {env:?}: {stdout}"
        );
    }

    // With none of those signals present the ANSI fallback still applies.
    let output = run_statusline_colored(&[], &payload);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("38;2;"), "stdout: {stdout}");
    assert!(stdout.contains("\\u001b["), "stdout: {stdout}");
}
