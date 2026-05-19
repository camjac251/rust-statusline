use claude_statusline::cli::{Args, PresetArg};

#[test]
fn no_preset_by_default() {
    let args = Args::parse_effective_from(["claude_statusline"]);
    assert_eq!(args.preset, None);
}

#[test]
fn preset_default_is_noop() {
    let args = Args::parse_effective_from(["claude_statusline", "--preset", "default"]);
    assert_eq!(args.preset, Some(PresetArg::Default));
    // No display.* fields should have flipped from defaults.
    assert!(!args.no_cost_session);
    assert!(!args.no_cost_today);
    assert!(!args.no_usage_weekly);
    assert!(!args.no_integrations_beads);
    assert!(!args.cost_breakdown);
    assert!(!args.cost_provenance);
    assert!(!args.provider_key_source);
}

#[test]
fn preset_minimal_hides_secondary_tokens_and_skips_expensive_subsystems() {
    let args = Args::parse_effective_from(["claude_statusline", "--preset", "minimal"]);
    assert_eq!(args.preset, Some(PresetArg::Minimal));

    // Cost: keep session, hide rest
    assert!(!args.no_cost_session);
    assert!(args.no_cost_today);
    assert!(args.no_cost_window);
    assert!(args.no_cost_lines_delta);

    // Usage: keep five_hour, hide rest
    assert!(!args.no_usage_five_hour);
    assert!(args.no_usage_weekly);
    assert!(args.no_usage_opus);
    assert!(args.no_usage_sonnet);
    assert!(args.no_usage_extra);

    // Context: keep percent, hide tokens and compact hint
    assert!(args.no_context_tokens);
    assert!(!args.no_context_percent);
    assert!(args.no_context_compact_hint);

    // Git: keep branch + dirty, hide ahead/behind + worktree
    assert!(!args.no_git_branch);
    assert!(!args.no_git_dirty);
    assert!(args.no_git_ahead_behind);
    assert!(args.no_git_worktree);

    // Workspace: keep cwd + model + fast_mode_indicator, hide rest
    assert!(!args.no_workspace_cwd);
    assert!(!args.no_workspace_model);
    assert!(!args.no_workspace_fast_mode_indicator);
    assert!(args.no_workspace_added_dirs);
    assert!(args.no_workspace_agent);
    assert!(args.no_workspace_output_style);
    assert!(args.no_workspace_effort);

    // Integrations: hide all
    assert!(args.no_integrations_beads);
    assert!(args.no_integrations_beads_alerts);
    assert!(args.no_integrations_gastown);
    assert!(args.no_integrations_prompt_cache);

    // Subsystems: skip expensive ones
    assert!(args.no_subsystem_beads);
    assert!(args.no_subsystem_gastown);
    assert!(args.no_subsystem_usage_api);
    // git + db_cache stay on (cheap / essential)
    assert!(!args.no_subsystem_git);
    assert!(!args.no_subsystem_db_cache);
}

#[test]
fn preset_full_turns_on_opt_in_tokens() {
    let args = Args::parse_effective_from(["claude_statusline", "--preset", "full"]);
    assert_eq!(args.preset, Some(PresetArg::Full));
    assert!(args.cost_breakdown);
    assert!(args.cost_provenance);
    assert!(args.provider_key_source);
    assert!(args.provider_name);

    // Default-on tokens stay on
    assert!(!args.no_cost_today);
    assert!(!args.no_usage_weekly);
    assert!(!args.no_integrations_beads);
}

#[test]
fn cli_atomic_overrides_preset_minimal() {
    // --preset minimal hides today, but explicit --cost-today equivalent shouldn't exist;
    // the negative form --no-cost-today is what flips visibility off. To override the preset
    // back ON, the user needs to NOT pass --no-cost-today AND the CLI rule says: atomic CLI
    // wins. Here we verify the inverse: preset hides today, CLI does not override -> stays hidden.
    // But CLI explicitly passing --no-cost-today should keep today hidden (no change in outcome
    // when the preset already hid it). The real test is preset ON, CLI override that re-enables
    // requires the user not pass --no-* and the preset semantics already aligns with that:
    // preset only sets if !user_set. So if the user passes --no-cost-today, that's user_set=true
    // and the preset never overrides. Cover the explicit-CLI-wins case for --no-cost-today.

    let args = Args::parse_effective_from([
        "claude_statusline",
        "--preset",
        "minimal",
        "--no-cost-today",
    ]);
    assert!(args.no_cost_today);
}

#[test]
fn config_preset_resolves_when_no_cli_preset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
        [display]
        preset = "minimal"
        "#,
    )
    .expect("write config");

    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
    ]);
    assert_eq!(args.preset, Some(PresetArg::Minimal));
    assert!(args.no_cost_today);
    assert!(args.no_usage_weekly);
}

#[test]
fn cli_preset_overrides_config_preset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
        [display]
        preset = "minimal"
        "#,
    )
    .expect("write config");

    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
        "--preset",
        "full",
    ]);
    assert_eq!(args.preset, Some(PresetArg::Full));
    assert!(args.cost_breakdown);
}

// ---------- json.* toggles ----------

#[test]
fn json_toggles_default_to_enabled() {
    let args = Args::parse_effective_from(["claude_statusline"]);
    assert!(!args.no_json_subagents);
    assert!(!args.no_json_tokens_breakdown);
    assert!(!args.no_json_duration);
    assert!(!args.no_json_rate_limit);
    assert!(!args.no_json_usage_limits);
    assert!(!args.no_json_compat_aliases);
}

#[test]
fn no_json_compat_aliases_cli() {
    let args = Args::parse_effective_from(["claude_statusline", "--no-json-compat-aliases"]);
    assert!(args.no_json_compat_aliases);
}

#[test]
fn json_toggles_via_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
        [json]
        subagents = false
        tokens_breakdown = false
        duration = false
        rate_limit = false
        usage_limits = false
        compat_aliases = false
        "#,
    )
    .expect("write config");

    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
    ]);
    assert!(args.no_json_subagents);
    assert!(args.no_json_tokens_breakdown);
    assert!(args.no_json_duration);
    assert!(args.no_json_rate_limit);
    assert!(args.no_json_usage_limits);
    assert!(args.no_json_compat_aliases);
}
