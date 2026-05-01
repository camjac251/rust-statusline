use claude_statusline::cli::{Args, LabelsArg};

#[test]
fn config_file_fills_unset_cli_options() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
        [display]
        labels = "long"
        show_provenance = true
        prompt_cache = false
        prompt_cache_ttl_seconds = 3600
        "#,
    )
    .expect("write config");

    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
    ]);

    assert_eq!(args.labels, LabelsArg::Long);
    assert!(args.show_provenance);
    assert!(!args.prompt_cache);
    assert_eq!(args.prompt_cache_ttl_seconds, Some(3600));
    assert_eq!(args.config_loaded.as_deref(), Some(config_path.as_path()));
}

#[test]
fn command_line_overrides_config_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "labels = \"long\"\n").expect("write config");

    let args = Args::parse_effective_from([
        "claude_statusline",
        "--config",
        config_path.to_str().expect("utf8 path"),
        "--labels",
        "short",
    ]);

    assert_eq!(args.labels, LabelsArg::Short);
}
