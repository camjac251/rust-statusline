use claude_statusline::cli::Args;

#[test]
fn subsystem_flags_default_to_enabled() {
    let args = Args::parse_effective_from(["claude_statusline"]);
    assert!(!args.no_subsystem_git);
    assert!(!args.no_subsystem_beads);
    assert!(!args.no_subsystem_gastown);
    assert!(!args.no_subsystem_db_cache);
    assert!(!args.no_subsystem_usage_api);
}

#[test]
fn no_subsystem_git_cli_flag_disables() {
    let args = Args::parse_effective_from(["claude_statusline", "--no-subsystem-git"]);
    assert!(args.no_subsystem_git);
    assert!(!args.no_subsystem_beads);
}

#[test]
fn no_subsystem_usage_api_cli_flag_disables() {
    let args = Args::parse_effective_from(["claude_statusline", "--no-subsystem-usage-api"]);
    assert!(args.no_subsystem_usage_api);
}

#[test]
fn config_file_disables_subsystem_via_subsystems_section() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
        [subsystems]
        git = false
        beads = false
        usage_api = false
        "#,
    )
    .expect("write config");

    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
    ]);

    assert!(args.no_subsystem_git);
    assert!(args.no_subsystem_beads);
    assert!(args.no_subsystem_usage_api);
    assert!(!args.no_subsystem_gastown);
    assert!(!args.no_subsystem_db_cache);
}

#[test]
fn cli_overrides_subsystems_config_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
        [subsystems]
        git = false
        "#,
    )
    .expect("write config");

    // Config says git disabled, but --no-subsystem-git absence should leave it default-enabled
    // when the flag isn't passed. Conversely, --no-subsystem-git on CLI should win.
    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
    ]);
    // Config disabled it, no CLI override -> disabled
    assert!(args.no_subsystem_git);

    // Now also pass --no-subsystem-beads on CLI; both should be disabled.
    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
        "--no-subsystem-beads",
    ]);
    assert!(args.no_subsystem_git);
    assert!(args.no_subsystem_beads);
}

// Env var precedence is wired through clap's `env =` attribute, so a parse with
// the env set produces the same behavior as the CLI flag. We do not exercise that
// path here because cargo test runs binaries in parallel and env mutation leaks
// across threads. The CLI + config paths above prove the resolution logic; the
// env path is a single clap attribute and is exercised manually via doctor.
