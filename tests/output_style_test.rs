use claude_statusline::display::build_json_output;
use claude_statusline::models::hook::{HookJson, HookModel, HookWorkspace, OutputStyle};
use serde_json::Value;

#[test]
fn test_output_style_in_json() {
    // Test with output_style present
    let hook_with_style = HookJson {
        session_id: "test_session".to_string(),
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
        version: Some("1.0.0".to_string()),
        output_style: Some(OutputStyle {
            name: "verbose".to_string(),
        }),
        cost: None,
    };

    let json: Value = build_json_output(
        &hook_with_style,
        0.0,  // session_cost
        0.0,  // today_cost
        0,    // sessions_count
        0.0,  // total_cost
        0.0,  // total_tokens
        0.0,  // noncache_tokens
        0,    // input tokens
        0,    // output tokens
        0,    // cache_create tokens
        0,    // cache_read tokens
        0,    // sess input
        0,    // sess output
        0,    // sess cache_create
        0,    // sess cache_read
        0,    // web_search_requests
        None, // service_tier
        None, // usage_percent
        None, // projected_percent
        0.0,  // remaining_minutes
        None, // active_block
        None, // latest_reset
        0.0,  // tpm
        0.0,  // tpm_indicator
        0.0,  // session_nc_tpm
        0.0,  // global_nc_tpm
        0.0,  // cost_per_hour
        None, // context
        None, // context_source
        None, // api_key_source
        None, // git_info
        None, // rate_limit
        None, // oauth_org_type
        None, // oauth_rate_tier
        None, // usage_limits
        None, // sessions_info
    );

    // Verify output_style is present in JSON
    assert!(json.get("output_style").is_some());
    assert_eq!(json["output_style"]["name"], "verbose");

    // Test with output_style absent
    let hook_without_style = HookJson {
        session_id: "test_session".to_string(),
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
        version: Some("1.0.0".to_string()),
        output_style: None,
        cost: None,
    };

    let json_no_style: Value = build_json_output(
        &hook_without_style,
        0.0,  // session_cost
        0.0,  // today_cost
        0,    // sessions_count
        0.0,  // total_cost
        0.0,  // total_tokens
        0.0,  // noncache_tokens
        0,    // input tokens
        0,    // output tokens
        0,    // cache_create tokens
        0,    // cache_read tokens
        0,    // sess input
        0,    // sess output
        0,    // sess cache_create
        0,    // sess cache_read
        0,    // web_search_requests
        None, // service_tier
        None, // usage_percent
        None, // projected_percent
        0.0,  // remaining_minutes
        None, // active_block
        None, // latest_reset
        0.0,  // tpm
        0.0,  // tpm_indicator
        0.0,  // session_nc_tpm
        0.0,  // global_nc_tpm
        0.0,  // cost_per_hour
        None, // context
        None, // context_source
        None, // api_key_source
        None, // git_info
        None, // rate_limit
        None, // oauth_org_type
        None, // oauth_rate_tier
        None, // usage_limits
        None, // sessions_info
    );

    // Verify output_style is null when not present
    assert!(json_no_style["output_style"].is_null());
}

#[test]
fn test_multiple_output_styles() {
    let styles = vec!["default", "verbose", "compact", "json", "markdown"];

    for style_name in styles {
        let hook = HookJson {
            session_id: "test_session".to_string(),
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
            version: Some("1.0.0".to_string()),
            output_style: Some(OutputStyle {
                name: style_name.to_string(),
            }),
            cost: None,
        };

        let json: Value = build_json_output(
            &hook, 0.0,  // session_cost
            0.0,  // today_cost
            0,    // sessions_count
            0.0,  // total_cost
            0.0,  // total_tokens
            0.0,  // noncache_tokens
            0,    // input tokens
            0,    // output tokens
            0,    // cache_create tokens
            0,    // cache_read tokens
            0,    // sess input
            0,    // sess output
            0,    // sess cache_create
            0,    // sess cache_read
            0,    // web_search_requests
            None, // service_tier
            None, // usage_percent
            None, // projected_percent
            0.0,  // remaining_minutes
            None, // active_block
            None, // latest_reset
            0.0,  // tpm
            0.0,  // tpm_indicator
            0.0,  // session_nc_tpm
            0.0,  // global_nc_tpm
            0.0,  // cost_per_hour
            None, // context
            None, // context_source
            None, // api_key_source
            None, // git_info
            None, // rate_limit
            None, // oauth_org_type
            None, // oauth_rate_tier
            None, // usage_limits
            None, // sessions_info
        );

        assert_eq!(
            json["output_style"]["name"], style_name,
            "Failed for style: {}",
            style_name
        );
    }
}
