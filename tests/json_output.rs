use serde_json::Value;

use chrono::{TimeZone, Utc};
use claude_statusline::display::build_json_output;
use claude_statusline::models::PromptCacheInfo;
use claude_statusline::models::hook::{HookJson, HookModel, HookRemote, HookWorkspace};
use claude_statusline::provenance::{
    CostProvenance, PricingSource, SessionCostSource, TodayCostSource,
};

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
            added_dirs: vec!["/tmp/project/packages/docs".to_string()],
            git_worktree: Some("feature-footer".to_string()),
        },
        version: Some("test".to_string()),
        output_style: None,
        cost: None,
        context_window: None,
        exceeds_200k_tokens: None,
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: Some(HookRemote {
            session_id: "remote-123".to_string(),
        }),
    };

    let json: Value = build_json_output(
        &hook,
        0.42,     // session_cost
        3.13,     // today_cost
        1,        // sessions_count
        1.23,     // total_cost
        123456.0, // total_tokens
        100000.0, // noncache_tokens
        90000,    // input tokens
        10000,    // output tokens
        20000,    // cache_create tokens
        13456,    // cache_read tokens
        0,        // sess input
        0,        // sess output
        0,        // sess cache_create
        0,        // sess cache_read
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
        None,                    // git_info
        None,                    // rate_limit
        None,                    // oauth_org_type
        None,                    // oauth_rate_tier
        None,                    // usage_limits
        None,                    // context_limit_override
        None,                    // beads_info
        None,                    // gastown_info
        false,                   // is_fast_mode
        None,                    // subagent_breakdown
        None,                    // cost_provenance
        None,                    // prompt_cache
    );

    // High-level keys exist
    for key in [
        "model",
        "cwd",
        "project_dir",
        "version",
        "workspace",
        "provider",
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
    assert_eq!(json["cwd"], "/tmp/project");
    assert_eq!(json["project_dir"], "/tmp/project");
    assert_eq!(json["workspace"]["current_dir"], "/tmp/project");
    assert_eq!(json["workspace"]["project_dir"], "/tmp/project");
    assert_eq!(json["workspace"]["git_worktree"], "feature-footer");
    assert_eq!(
        json["workspace"]["added_dirs"][0],
        "/tmp/project/packages/docs"
    );
    assert_eq!(json["remote"]["session_id"], "remote-123");

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

    // Usage limits include extended buckets
    assert!(json["usage_limits"].is_null() || json["usage_limits"].is_object());
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
            added_dirs: Vec::new(),
            git_worktree: None,
        },
        version: Some("test".to_string()),
        output_style: None,
        cost: None,
        context_window: None,
        exceeds_200k_tokens: None,
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: None,
    };

    let json: Value = build_json_output(
        &hook,
        0.0,
        0.0,
        0, // sessions_count
        0.0,
        0.0,
        0.0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0, // web_search_requests
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
        None,
        None,
        None,  // context_limit_override
        None,  // beads_info
        None,  // gastown_info
        false, // is_fast_mode
        None,  // subagent_breakdown
        None,  // cost_provenance
        None,  // prompt_cache
    );

    // 1M context (full limit, percentage calculated against this)
    assert_eq!(json["context"]["limit"], 1_000_000);
}

#[test]
fn json_output_context_limit_override_from_hook() {
    // Test that context_limit_override takes precedence over model detection
    let hook = HookJson {
        session_id: "s1".to_string(),
        transcript_path: "/tmp/transcript.jsonl".to_string(),
        cwd: None,
        model: HookModel {
            id: "some-proxy-model".to_string(), // Unknown model
            display_name: "Custom Proxy Model".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: Some("/tmp/project".to_string()),
            added_dirs: Vec::new(),
            git_worktree: None,
        },
        version: Some("test".to_string()),
        output_style: None,
        cost: None,
        context_window: None,
        exceeds_200k_tokens: None,
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: None,
    };

    // Without override, unknown model defaults to 200k
    let json_no_override: serde_json::Value = build_json_output(
        &hook,
        0.0,
        0.0,
        0,
        0.0,
        0.0,
        0.0,
        0,
        0,
        0,
        0,
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
        Some((50000, 25)),
        Some("hook"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,  // No override
        None,  // beads_info
        None,  // gastown_info
        false, // is_fast_mode
        None,  // subagent_breakdown
        None,  // cost_provenance
        None,  // prompt_cache
    );
    assert_eq!(json_no_override["context"]["limit"], 200_000);

    // With override (simulating Gemini 1M context from proxy)
    let json_with_override: serde_json::Value = build_json_output(
        &hook,
        0.0,
        0.0,
        0,
        0.0,
        0.0,
        0.0,
        0,
        0,
        0,
        0,
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
        Some((50000, 5)),
        Some("hook"),
        None,
        None,
        None,
        None,
        None,
        None,
        Some(1_048_576), // Gemini 1M context override
        None,            // beads_info
        None,            // gastown_info
        false,           // is_fast_mode
        None,            // subagent_breakdown
        None,            // cost_provenance
        None,            // prompt_cache
    );
    assert_eq!(json_with_override["context"]["limit"], 1_048_576);
    assert_eq!(json_with_override["context"]["limit_full"], 1_048_576);
}

#[test]
fn json_output_includes_provenance_and_prompt_cache() {
    let hook = HookJson {
        session_id: "s1".to_string(),
        transcript_path: "/tmp/transcript.jsonl".to_string(),
        cwd: None,
        model: HookModel {
            id: "claude-sonnet-4-5".to_string(),
            display_name: "Claude Sonnet 4.5".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: Some("/tmp/project".to_string()),
            added_dirs: Vec::new(),
            git_worktree: None,
        },
        version: None,
        output_style: None,
        cost: None,
        context_window: None,
        exceeds_200k_tokens: None,
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: None,
    };
    let provenance = CostProvenance {
        session_cost: SessionCostSource::TranscriptResult,
        today_cost: TodayCostSource::DbGlobalUsage,
        pricing: PricingSource::Embedded,
    };
    let prompt_cache = PromptCacheInfo {
        last_response_at: Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap(),
        ttl_seconds: 300,
        now: Utc.with_ymd_and_hms(2026, 5, 1, 12, 3, 0).unwrap(),
    };

    let json: Value = build_json_output(
        &hook,
        1.0,
        2.0,
        1,
        0.0,
        0.0,
        0.0,
        0,
        0,
        0,
        0,
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
        Some((100_000, 50)),
        Some("hook"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        Some(&provenance),
        Some(&prompt_cache),
    );

    assert_eq!(json["session"]["cost_source"], "transcript_result");
    assert_eq!(json["today"]["cost_source"], "db_global_usage");
    assert_eq!(json["provenance"]["pricing"], "embedded");
    assert_eq!(json["prompt_cache"]["remaining_seconds"], 120);
    assert_eq!(json["prompt_cache"]["percent_remaining"], 40.0);
    assert!(json["context"]["usable_limit"].is_number());
    assert!(json["context"]["usable_percent"].is_number());
}
