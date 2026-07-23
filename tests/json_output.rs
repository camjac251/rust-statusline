use serde_json::Value;
use std::env;

use chrono::{TimeZone, Utc};
use claude_statusline::display::build_json_output;
use claude_statusline::models::hook::{
    HookContextWindow, HookCost, HookEffort, HookJson, HookModel, HookRemote, HookThinking,
    HookWorkspace, OutputStyle,
};

fn default_hook_cost() -> HookCost {
    HookCost {
        total_cost_usd: 0.0,
        total_duration_ms: 0,
        total_api_duration_ms: 0,
        total_lines_added: 0,
        total_lines_removed: 0,
    }
}

fn default_hook_context_window() -> HookContextWindow {
    HookContextWindow {
        total_input_tokens: 0,
        total_output_tokens: 0,
        context_window_size: 200_000,
        current_usage: None,
        used_percentage: 0,
        remaining_percentage: 100,
    }
}

fn default_output_style() -> OutputStyle {
    OutputStyle {
        name: "default".to_string(),
    }
}
use claude_statusline::models::{PromptCacheBucketInfo, PromptCacheBucketKind, PromptCacheInfo};
use claude_statusline::models::{RemoteTask, SessionActivity, WorkflowProgressEntry, WorkflowRun};
use claude_statusline::provenance::{
    CostProvenance, PricingSource, SessionCostSource, TodayCostSource,
};

#[test]
fn json_output_shape_minimal() {
    let hook = HookJson {
        session_id: "s1".to_string(),
        transcript_path: "/tmp/transcript.jsonl".to_string(),
        model: HookModel {
            id: "claude-sonnet-4-6".to_string(),
            display_name: "Claude Sonnet 4.6".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: "/tmp/project".to_string(),
            added_dirs: vec!["/tmp/project/packages/docs".to_string()],
            git_worktree: Some("feature-footer".to_string()),
            repo: None,
        },
        version: "test".to_string(),
        output_style: default_output_style(),
        cost: default_hook_cost(),
        context_window: default_hook_context_window(),
        exceeds_200k_tokens: false,
        fast_mode: true,
        effort: Some(HookEffort {
            level: "high".to_string(),
        }),
        thinking: HookThinking { enabled: false },
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: Some(HookRemote {
            session_id: "remote-123".to_string(),
        }),
        pr: None,
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
        None,                    // session_activity
    );

    // High-level keys exist
    for key in [
        "model",
        "version",
        "workspace",
        "provider",
        "reset_at",
        "session",
        "today",
        "window",
        "context",
        "git",
    ] {
        assert!(json.get(key).is_some(), "missing key: {}", key);
    }

    // Model sub-keys
    assert_eq!(json["model"]["id"], "claude-sonnet-4-6");
    assert_eq!(json["model"]["display_name"], "Claude Sonnet 4.6");
    assert_eq!(json["model"]["fast_mode"], true);
    assert_eq!(json["effort"], "high");
    assert_eq!(json["thinking"]["enabled"], false);
    assert_eq!(json["workspace"]["current_dir"], "/tmp/project");
    assert_eq!(json["workspace"]["project_dir"], "/tmp/project");
    assert_eq!(json["workspace"]["git_worktree"], "feature-footer");
    assert_eq!(
        json["workspace"]["added_dirs"][0],
        "/tmp/project/packages/docs"
    );
    assert_eq!(json["remote"]["session_id"], "remote-123");
    assert!(json.get("cwd").is_none());
    assert!(json.get("project_dir").is_none());
    assert!(json.get("fast_mode").is_none());
    assert!(json.get("block").is_none());

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
        model: HookModel {
            id: "claude-sonnet-4-6".to_string(),
            display_name: "Claude Sonnet 4.6 [1m]".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: "/tmp/project".to_string(),
            added_dirs: Vec::new(),
            git_worktree: None,
            repo: None,
        },
        version: "test".to_string(),
        output_style: default_output_style(),
        cost: default_hook_cost(),
        context_window: default_hook_context_window(),
        exceeds_200k_tokens: false,
        fast_mode: false,
        effort: None,
        thinking: HookThinking { enabled: false },
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: None,
        pr: None,
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
        None,  // session_activity
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
        model: HookModel {
            id: "some-proxy-model".to_string(), // Unknown model
            display_name: "Custom Proxy Model".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: "/tmp/project".to_string(),
            added_dirs: Vec::new(),
            git_worktree: None,
            repo: None,
        },
        version: "test".to_string(),
        output_style: default_output_style(),
        cost: default_hook_cost(),
        context_window: default_hook_context_window(),
        exceeds_200k_tokens: false,
        fast_mode: false,
        effort: None,
        thinking: HookThinking { enabled: false },
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: None,
        pr: None,
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
        None,  // session_activity
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
        None,            // session_activity
    );
    assert_eq!(json_with_override["context"]["limit"], 1_048_576);
    assert_eq!(json_with_override["context"]["limit_full"], 1_048_576);
}

#[test]
#[serial_test::serial]
fn json_output_reports_output_reserve_used_after_usable_limit() {
    // SAFETY: Test runs serially, no concurrent env access
    unsafe {
        env::set_var("CLAUDE_SYSTEM_OVERHEAD", "1234");
    }
    let hook = HookJson {
        session_id: "s1".to_string(),
        transcript_path: "/tmp/transcript.jsonl".to_string(),
        model: HookModel {
            id: "claude-sonnet-4-5".to_string(),
            display_name: "Claude Sonnet 4.5".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: "/tmp/project".to_string(),
            added_dirs: Vec::new(),
            git_worktree: None,
            repo: None,
        },
        version: "test".to_string(),
        output_style: default_output_style(),
        cost: default_hook_cost(),
        context_window: default_hook_context_window(),
        exceeds_200k_tokens: false,
        fast_mode: false,
        effort: None,
        thinking: HookThinking { enabled: false },
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: None,
        pr: None,
    };

    let json: Value = build_json_output(
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
        Some((170_000, 85)),
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
        None,
        None,
        None,
    );

    assert_eq!(json["context"]["limit"], 200_000);
    assert_eq!(json["context"]["output_reserve"], 32_000);
    assert_eq!(json["context"]["usable_limit"], 168_000);
    assert_eq!(json["context"]["output_reserve_used"], 2_000);
    assert_eq!(json["context"]["tokens_raw"], 170_000);
    assert!(json["context"]["system_overhead_tokens"].is_null());
    // SAFETY: Test runs serially, no concurrent env access
    unsafe {
        env::remove_var("CLAUDE_SYSTEM_OVERHEAD");
    }
}

#[test]
fn json_output_includes_provenance_and_prompt_cache() {
    let hook = HookJson {
        session_id: "s1".to_string(),
        transcript_path: "/tmp/transcript.jsonl".to_string(),
        model: HookModel {
            id: "claude-sonnet-4-5".to_string(),
            display_name: "Claude Sonnet 4.5".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: "/tmp/project".to_string(),
            added_dirs: Vec::new(),
            git_worktree: None,
            repo: None,
        },
        version: "test".to_string(),
        output_style: default_output_style(),
        cost: default_hook_cost(),
        context_window: default_hook_context_window(),
        exceeds_200k_tokens: false,
        fast_mode: false,
        effort: None,
        thinking: HookThinking { enabled: false },
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: None,
        pr: None,
    };
    let provenance = CostProvenance {
        session_cost: SessionCostSource::TranscriptResult,
        today_cost: TodayCostSource::DbGlobalUsage,
        pricing: PricingSource::Embedded,
    };
    let prompt_cache = PromptCacheInfo {
        buckets: vec![PromptCacheBucketInfo {
            kind: PromptCacheBucketKind::FiveMinute,
            created_at: Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap(),
            ttl_seconds: 300,
            input_tokens: 5000,
        }],
        last_cache_write_at: Some(Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap()),
        last_cache_read_at: Some(Utc.with_ymd_and_hms(2026, 5, 1, 12, 2, 0).unwrap()),
        cache_write_input_tokens: 5000,
        cache_read_input_tokens: 8000,
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
        None,
    );

    assert_eq!(json["session"]["cost_source"], "transcript_result");
    assert_eq!(json["today"]["cost_source"], "db_global_usage");
    assert_eq!(json["provenance"]["pricing"], "embedded");
    assert_eq!(json["prompt_cache"]["remaining_seconds"], 120);
    assert_eq!(json["prompt_cache"]["percent_remaining"], 40.0);
    assert_eq!(json["prompt_cache"]["age_seconds"], 60);
    assert_eq!(json["prompt_cache"]["write_age_seconds"], 180);
    assert_eq!(json["prompt_cache"]["read_age_seconds"], 60);
    assert_eq!(json["prompt_cache"]["buckets"][0]["kind"], "5m");
    assert_eq!(json["prompt_cache"]["cache_write_input_tokens"], 5000);
    assert_eq!(json["prompt_cache"]["cache_read_input_tokens"], 8000);
    assert!(json["context"]["usable_limit"].is_number());
    assert!(json["context"]["usable_percent"].is_number());
}

#[test]
fn json_output_injects_enriched_subagent_breakdown() {
    let hook = HookJson {
        session_id: "s1".to_string(),
        transcript_path: "/tmp/transcript.jsonl".to_string(),
        model: HookModel {
            id: "claude-sonnet-4-6".to_string(),
            display_name: "Claude Sonnet 4.6".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: "/tmp/project".to_string(),
            added_dirs: Vec::new(),
            git_worktree: None,
            repo: None,
        },
        version: "test".to_string(),
        output_style: default_output_style(),
        cost: default_hook_cost(),
        context_window: default_hook_context_window(),
        exceeds_200k_tokens: false,
        fast_mode: false,
        effort: None,
        thinking: HookThinking { enabled: false },
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: None,
        pr: None,
    };

    let breakdown = serde_json::json!([
        {
            "agent_id": "aaa111",
            "cost_usd": 0.12,
            "input_tokens": 5000,
            "output_tokens": 200,
            "agent_type": "code-reviewer",
            "name": "reviewer",
            "model": "claude-opus-4-6",
            "spawn_depth": 1,
            "parent_agent_id": "root-agent"
        },
        {
            "agent_id": "bbb222",
            "cost_usd": 0.03,
            "input_tokens": 1000,
            "output_tokens": 50,
            "agent_type": "workflow-subagent",
            "spawn_depth": 2,
            "workflow_run_id": "wf_run42"
        }
    ]);

    let json: Value = build_json_output(
        &hook,
        0.42,
        3.13,
        1,
        1.23,
        123456.0,
        100000.0,
        90000,
        10000,
        20000,
        13456,
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
        Some((12345, 6)),
        Some("transcript"),
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
        Some(breakdown), // subagent_breakdown
        None,
        None,
        None,
    );

    let subagents = json["session"]["subagents"]
        .as_array()
        .expect("session.subagents should be a non-empty array");
    assert_eq!(subagents.len(), 2);

    assert_eq!(subagents[0]["agent_id"], "aaa111");
    assert_eq!(subagents[0]["cost_usd"], 0.12);
    assert_eq!(subagents[0]["input_tokens"], 5000);
    assert_eq!(subagents[0]["agent_type"], "code-reviewer");
    assert_eq!(subagents[0]["name"], "reviewer");
    assert_eq!(subagents[0]["spawn_depth"], 1);
    assert_eq!(subagents[0]["parent_agent_id"], "root-agent");
    assert!(subagents[0].get("workflow_run_id").is_none());

    assert_eq!(subagents[1]["agent_id"], "bbb222");
    assert_eq!(subagents[1]["agent_type"], "workflow-subagent");
    assert_eq!(subagents[1]["workflow_run_id"], "wf_run42");
}

#[test]
fn json_output_includes_workflows_and_remote_tasks() {
    let hook = HookJson {
        session_id: "s1".to_string(),
        transcript_path: "/tmp/transcript.jsonl".to_string(),
        model: HookModel {
            id: "claude-sonnet-4-6".to_string(),
            display_name: "Claude Sonnet 4.6".to_string(),
        },
        workspace: HookWorkspace {
            current_dir: "/tmp/project".to_string(),
            project_dir: "/tmp/project".to_string(),
            added_dirs: Vec::new(),
            git_worktree: None,
            repo: None,
        },
        version: "test".to_string(),
        output_style: default_output_style(),
        cost: default_hook_cost(),
        context_window: default_hook_context_window(),
        exceeds_200k_tokens: false,
        fast_mode: false,
        effort: None,
        thinking: HookThinking { enabled: false },
        rate_limits: None,
        session_name: None,
        vim: None,
        agent: None,
        worktree: None,
        remote: None,
        pr: None,
    };

    let activity = SessionActivity {
        workflows: vec![WorkflowRun {
            run_id: "run1".to_string(),
            workflow_name: Some("review".to_string()),
            status: Some("running".to_string()),
            start_time: Some(1000),
            total_tokens: Some(1234),
            agent_count: Some(6),
            workflow_progress: vec![
                WorkflowProgressEntry {
                    kind: Some("workflow_agent".to_string()),
                    state: Some("done".to_string()),
                },
                WorkflowProgressEntry {
                    kind: Some("workflow_agent".to_string()),
                    state: Some("done".to_string()),
                },
                WorkflowProgressEntry {
                    kind: Some("workflow_agent".to_string()),
                    state: Some("running".to_string()),
                },
            ],
        }],
        remote_tasks: vec![RemoteTask {
            task_id: "t1".to_string(),
            remote_task_type: Some("cloud".to_string()),
            title: Some("build".to_string()),
        }],
    };

    let json: Value = build_json_output(
        &hook,
        0.0,
        0.0,
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
        None,
        None,
        Some(&activity),
    );

    assert_eq!(json["workflows"][0]["run_id"], "run1");
    assert_eq!(json["workflows"][0]["name"], "review");
    assert_eq!(json["workflows"][0]["status"], "running");
    assert_eq!(json["workflows"][0]["agents_done"], 2);
    assert_eq!(json["workflows"][0]["agents_total"], 6);
    assert_eq!(json["workflows"][0]["total_tokens"], 1234);

    assert_eq!(json["remote_tasks"][0]["task_id"], "t1");
    assert_eq!(json["remote_tasks"][0]["task_type"], "cloud");
    assert_eq!(json["remote_tasks"][0]["title"], "build");

    // Absent activity leaves both fields null.
    let json_absent: Value = build_json_output(
        &hook,
        0.0,
        0.0,
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
        None,
        None,
        None,
    );
    assert!(json_absent["workflows"].is_null());
    assert!(json_absent["remote_tasks"].is_null());
}
