use claude_statusline::cli::Args;

#[test]
fn display_default_toggles_are_visible() {
    let args = Args::parse_effective_from(["claude_statusline"]);

    // Default-on toggles (no_* fields should be false)
    assert!(!args.no_cost_session);
    assert!(!args.no_cost_today);
    assert!(!args.no_cost_window);
    assert!(!args.no_usage_five_hour);
    assert!(!args.no_usage_weekly);
    assert!(!args.no_context_tokens);
    assert!(!args.no_context_percent);
    assert!(!args.no_context_compact_hint);
    assert!(!args.no_workspace_cwd);
    assert!(!args.no_workspace_model);
    assert!(!args.no_integrations_beads);
    assert!(!args.no_integrations_prompt_cache);
    assert!(!args.no_git_branch);

    // Default-off opt-ins
    assert!(!args.cost_breakdown);
    assert!(!args.cost_provenance);
    assert!(!args.provider_key_source);
    assert!(!args.provider_name);
}

#[test]
fn no_cost_session_cli_disables() {
    let args = Args::parse_effective_from(["claude_statusline", "--no-cost-session"]);
    assert!(args.no_cost_session);
    assert!(!args.no_cost_today); // siblings unaffected
}

#[test]
fn cost_breakdown_opt_in_enables() {
    let args = Args::parse_effective_from(["claude_statusline", "--cost-breakdown"]);
    assert!(args.cost_breakdown);
    assert!(!args.cost_provenance);
}

#[test]
fn config_disables_individual_cost_tokens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
        [display.cost]
        session = false
        today = false
        window = true
        breakdown = true
        provenance = true

        [display.context]
        compact_hint = false

        [display.provider]
        key_source = true
        name = true
        "#,
    )
    .expect("write config");

    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
    ]);

    assert!(args.no_cost_session);
    assert!(args.no_cost_today);
    assert!(!args.no_cost_window);
    assert!(args.cost_breakdown);
    assert!(args.cost_provenance);
    assert!(args.no_context_compact_hint);
    assert!(args.provider_key_source);
    assert!(args.provider_name);
}

#[test]
fn cli_atomic_override_beats_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
        [display.cost]
        breakdown = true
        "#,
    )
    .expect("write config");

    // Config enables breakdown; CLI does not override -> stays on
    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
    ]);
    assert!(args.cost_breakdown);

    // Config enables breakdown; CLI passes --cost-breakdown which keeps it on
    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
        "--cost-breakdown",
    ]);
    assert!(args.cost_breakdown);
}

#[test]
fn workspace_atomic_toggles() {
    let args = Args::parse_effective_from([
        "claude_statusline",
        "--no-workspace-cwd",
        "--no-workspace-model",
        "--no-workspace-agent",
    ]);
    assert!(args.no_workspace_cwd);
    assert!(args.no_workspace_model);
    assert!(args.no_workspace_agent);
    assert!(!args.no_workspace_added_dirs);
}
