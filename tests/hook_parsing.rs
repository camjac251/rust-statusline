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
