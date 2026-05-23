use claude_statusline::models::hook::HookJson;

/// A minimum-shape hook payload that matches what Claude Code 2.1.148+ ships
/// on every statusline invocation. Tests pull this in and extend it with the
/// optional fields they care about.
const MINIMUM_HOOK: &str = r#"{
  "session_id": "sess-min",
  "transcript_path": "/tmp/transcript.jsonl",
  "model": { "id": "claude-sonnet-4-5", "display_name": "Sonnet 4.5" },
  "workspace": {
    "current_dir": "/tmp/project",
    "project_dir": "/tmp/project",
    "added_dirs": []
  },
  "version": "2.1.148",
  "output_style": { "name": "default" },
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
  "thinking": { "enabled": false }
}"#;

#[test]
fn parses_minimum_2_1_148_hook_payload() {
    let hook: HookJson = serde_json::from_str(MINIMUM_HOOK).expect("minimum hook should parse");
    assert_eq!(hook.session_id, "sess-min");
    assert_eq!(hook.workspace.project_dir, "/tmp/project");
    assert_eq!(hook.version, "2.1.148");
    assert_eq!(hook.output_style.name, "default");
    assert_eq!(hook.cost.total_cost_usd, 0.0);
    assert_eq!(hook.context_window.context_window_size, 200000);
    assert!(!hook.fast_mode);
    assert!(!hook.thinking.enabled);
    assert!(hook.effort.is_none());
    assert!(hook.rate_limits.is_none());
    assert!(hook.remote.is_none());
}

#[test]
fn parses_claude_code_2113_extras() {
    let hook: HookJson = serde_json::from_str(
        r#"{
          "session_id": "sess-1",
          "transcript_path": "/tmp/transcript.jsonl",
          "model": { "id": "claude-sonnet-4-5", "display_name": "Sonnet 4.5" },
          "workspace": {
            "current_dir": "/tmp/project",
            "project_dir": "/tmp/project",
            "added_dirs": ["/tmp/project/docs", "/tmp/project/scripts"],
            "git_worktree": "feature/footer"
          },
          "version": "2.1.13",
          "output_style": { "name": "default" },
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
          "thinking": { "enabled": false },
          "remote": { "session_id": "remote-abc" }
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
fn parses_claude_code_2_1_128_reporting_fields() {
    let hook: HookJson = serde_json::from_str(
        r#"{
          "session_id": "sess-2128",
          "transcript_path": "/tmp/transcript.jsonl",
          "model": { "id": "claude-opus-4-6", "display_name": "Opus 4.6" },
          "workspace": {
            "current_dir": "/tmp/project",
            "project_dir": "/tmp/project",
            "added_dirs": []
          },
          "version": "2.1.128",
          "output_style": { "name": "default" },
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
            "five_hour": { "used_percentage": 12.5, "resets_at": 1770000000 },
            "seven_day": { "used_percentage": 25.0, "resets_at": 1770600000 }
          }
        }"#,
    )
    .expect("2.1.128 hook should parse");

    assert!(hook.fast_mode);
    assert_eq!(hook.cost.total_cost_usd, 1.23);
    assert_eq!(hook.cost.total_lines_added, 5);
    assert_eq!(
        hook.effort.as_ref().map(|effort| effort.level.as_str()),
        Some("xhigh")
    );
    assert!(!hook.thinking.enabled);
    let usage = hook.context_window.current_usage.as_ref().unwrap();
    assert_eq!(usage.cache_creation_input_tokens, 2000);
    assert_eq!(
        hook.rate_limits
            .as_ref()
            .and_then(|limits| limits.five_hour.as_ref())
            .and_then(|limit| limit.used_percentage),
        Some(12.5)
    );
}

#[test]
fn rejects_payloads_missing_required_fields() {
    let result: Result<HookJson, _> = serde_json::from_str(
        r#"{
          "session_id": "sess-legacy",
          "transcript_path": "/tmp/transcript.jsonl",
          "model": { "id": "x", "display_name": "x" },
          "workspace": { "current_dir": "/tmp/p", "project_dir": "/tmp/p" }
        }"#,
    );
    assert!(
        result.is_err(),
        "payload missing 2.1.148-required fields must fail to parse"
    );
}
