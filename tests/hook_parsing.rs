use claude_statusline::models::hook::HookJson;

#[test]
fn parses_claude_code_2113_hook_fields() {
    let hook: HookJson = serde_json::from_str(
        r#"{
          "session_id": "sess-1",
          "transcript_path": "/tmp/transcript.jsonl",
          "model": {
            "id": "claude-sonnet-4-5",
            "display_name": "Sonnet 4.5"
          },
          "workspace": {
            "current_dir": "/tmp/project",
            "project_dir": "/tmp/project",
            "added_dirs": ["/tmp/project/docs", "/tmp/project/scripts"],
            "git_worktree": "feature/footer"
          },
          "remote": {
            "session_id": "remote-abc"
          }
        }"#,
    )
    .expect("hook should parse");

    assert_eq!(hook.workspace.added_dirs.len(), 2);
    assert_eq!(hook.workspace.added_dirs[0], "/tmp/project/docs");
    assert_eq!(
        hook.workspace.git_worktree.as_deref(),
        Some("feature/footer")
    );
    assert_eq!(
        hook.remote
            .as_ref()
            .map(|remote| remote.session_id.as_str()),
        Some("remote-abc")
    );
}

#[test]
fn older_hook_payloads_remain_compatible() {
    let hook: HookJson = serde_json::from_str(
        r#"{
          "session_id": "sess-legacy",
          "transcript_path": "/tmp/transcript.jsonl",
          "model": {
            "id": "claude-3-5-sonnet",
            "display_name": "Claude 3.5 Sonnet"
          },
          "workspace": {
            "current_dir": "/tmp/project",
            "project_dir": "/tmp/project"
          }
        }"#,
    )
    .expect("legacy hook should parse");

    assert!(hook.workspace.added_dirs.is_empty());
    assert!(hook.workspace.git_worktree.is_none());
    assert!(hook.remote.is_none());
}

#[test]
fn parses_claude_code_2_1_128_reporting_fields() {
    let hook: HookJson = serde_json::from_str(
        r#"{
          "session_id": "sess-2128",
          "transcript_path": "/tmp/transcript.jsonl",
          "model": {
            "id": "claude-opus-4-6",
            "display_name": "Opus 4.6"
          },
          "workspace": {
            "current_dir": "/tmp/project",
            "project_dir": "/tmp/project"
          },
          "cost": {
            "total_cost_usd": 1.23,
            "total_duration_ms": 120000,
            "total_api_duration_ms": 45000,
            "total_lines_added": 5,
            "total_lines_removed": 2
          },
          "context_window": {
            "total_input_tokens": 10000,
            "total_output_tokens": 2000,
            "context_window_size": 200000,
            "current_usage": {
              "input_tokens": 7000,
              "output_tokens": 1000,
              "cache_creation_input_tokens": 2000,
              "cache_read_input_tokens": 1000
            },
            "used_percentage": 5,
            "remaining_percentage": 95
          },
          "exceeds_200k_tokens": false,
          "fast_mode": true,
          "effort": { "level": "xhigh" },
          "thinking": { "enabled": false },
          "rate_limits": {
            "five_hour": {
              "used_percentage": 12.5,
              "resets_at": 1770000000
            },
            "seven_day": {
              "used_percentage": 25.0,
              "resets_at": 1770600000
            }
          }
        }"#,
    )
    .expect("2.1.128 hook should parse");

    assert_eq!(hook.fast_mode, Some(true));
    assert_eq!(
        hook.effort.as_ref().map(|effort| effort.level.as_str()),
        Some("xhigh")
    );
    assert_eq!(
        hook.thinking.as_ref().map(|thinking| thinking.enabled),
        Some(false)
    );
    assert_eq!(
        hook.context_window
            .as_ref()
            .and_then(|context| context.current_usage.as_ref())
            .and_then(|usage| usage.cache_creation_input_tokens),
        Some(2000)
    );
    assert_eq!(
        hook.rate_limits
            .as_ref()
            .and_then(|limits| limits.five_hour.as_ref())
            .and_then(|limit| limit.used_percentage),
        Some(12.5)
    );
}
