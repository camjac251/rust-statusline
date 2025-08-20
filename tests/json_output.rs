use serde_json::Value;

use claude_statusline::display::build_json_output;
use claude_statusline::models::hook::{HookJson, HookModel, HookWorkspace};

#[test]
fn json_output_shape_minimal() {
    let hook = HookJson {
        session_id: "s1".to_string(),
        transcript_path: "/tmp/transcript.jsonl".to_string(),
        cwd: None,
        model: HookModel {
            id: "claude-3.5-sonnet".to_string(),
            display_name: "Claude 3.5 Sonnet".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: Some("/tmp/project".to_string()),
        },
        version: Some("test".to_string()),
        output_style: None,
        cost: None,
    };

    let json: Value = build_json_output(
        &hook,
        0.42,     // session_cost
        3.13,     // today_cost
        1.23,     // total_cost
        123456.0, // total_tokens
        100000.0, // noncache_tokens
        90000,    // input tokens
        10000,    // output tokens
        20000,    // cache_create tokens
        13456,    // cache_read tokens
        3,        // web_search_requests
        Some("standard".to_string()),
        Some(12.3),
        Some(25.0),
        85.0,             // remaining_minutes
        None,             // active_block
        None,             // latest_reset
        1500.0,           // tpm
        1200.0,           // tpm_indicator
        1200.0,           // session_nc_tpm
        1500.0,           // global_nc_tpm
        1.50,             // cost_per_hour
        Some((12345, 6)), // context
        Some("transcript"),
        Some("env".to_string()), // api_key_source
        Some("pro".to_string()), // plan_tier
        Some(200_000.0),         // plan_max
        None,                    // git_info
    );

    // High-level keys exist
    for key in [
        "model",
        "cwd",
        "project_dir",
        "version",
        "provider",
        "plan",
        "reset_at",
        "session",
        "today",
        "block",
        "window",
        "context",
        "git",
    ] {
        assert!(json.get(key).is_some(), "missing key: {}", key);
    }

    // Model sub-keys
    assert_eq!(json["model"]["id"], "claude-3.5-sonnet");
    assert_eq!(json["model"]["display_name"], "Claude 3.5 Sonnet");

    // Basic numeric fields exist and are numbers
    assert!(json["session"]["cost_usd"].is_number());
    assert!(json["today"]["cost_usd"].is_number());
    assert!(json["window"]["tokens_per_minute"].is_number());
    assert!(json["window"]["total_tokens"].is_number());
    assert!(json["window"]["input_tokens"].is_number());
    assert!(json["window"]["output_tokens"].is_number());
    assert!(json["window"]["cache_creation_input_tokens"].is_number());
    assert!(json["window"]["cache_read_input_tokens"].is_number());
    assert!(json["window"]["web_search_requests"].is_number());
    assert!(json["window"]["cost_per_hour"].is_number());

    // Context section present
    assert!(json["context"]["limit"].is_number());
}

#[test]
fn json_output_1m_context_limit_when_display_has_1m_tag() {
    let hook = HookJson {
        session_id: "s1".to_string(),
        transcript_path: "/tmp/transcript.jsonl".to_string(),
        cwd: None,
        model: HookModel {
            id: "claude-3.5-sonnet".to_string(),
            display_name: "Claude 3.5 Sonnet [1m]".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: Some("/tmp/project".to_string()),
        },
        version: Some("test".to_string()),
        output_style: None,
        cost: None,
    };

    let json: Value = build_json_output(
        &hook,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0,
        0,
        0,
        0,
        0,
        None,
        None,
        None,
        0.0,
        None,
        None,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        Some((0, 0)),
        Some("transcript"),
        None,
        None,
        None,
        None,
    );

    assert_eq!(json["context"]["limit"], 1_000_000);
}
