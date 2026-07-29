use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::fs;
use std::path::PathBuf;

use crate::cli::{Args, Command, InitArgs};
use crate::provenance::PricingSource;

#[derive(Debug, Serialize)]
struct ClaudePathHealth {
    path: String,
    exists: bool,
    has_projects: bool,
}

#[derive(Debug, Serialize)]
struct SettingsHealth {
    path: String,
    exists: bool,
    status_line_present: bool,
    command: Option<String>,
    refresh_interval: Option<u64>,
    subagent_status_line_present: bool,
    subagent_command: Option<String>,
    ok: bool,
}

#[derive(Debug, Serialize)]
struct ConfigHealth {
    loaded_path: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct PricingHealth {
    probe_model: String,
    source: PricingSource,
    available: bool,
}

#[derive(Debug, Serialize)]
struct SubsystemHealth {
    git: bool,
    beads: bool,
    gastown: bool,
    db_cache: bool,
    usage_api: bool,
}

#[derive(Debug, Serialize)]
struct PresetHealth {
    selected: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct DisplayToggleHealth {
    cost_breakdown: bool,
    cost_provenance: bool,
    provider_key_source: bool,
    provider_name: bool,
    context_compact_hint_enabled: bool,
    integrations_prompt_cache_enabled: bool,
}

#[derive(Debug, Serialize)]
struct JsonToggleHealth {
    subagents: bool,
    tokens_breakdown: bool,
    duration: bool,
    rate_limit: bool,
    usage_limits: bool,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    ok: bool,
    warnings: Vec<String>,
    config: ConfigHealth,
    claude_paths: Vec<ClaudePathHealth>,
    settings: SettingsHealth,
    db: crate::db::DbHealth,
    usage_api: crate::usage_api::UsageApiHealth,
    pricing: PricingHealth,
    subsystems: SubsystemHealth,
    preset: PresetHealth,
    display_opt_in: DisplayToggleHealth,
    json_settings: JsonToggleHealth,
}

pub fn run_command(args: &Args, command: &Command) -> Result<()> {
    match command {
        Command::Doctor => run_doctor(args),
        Command::Init(init) => run_init(args, init),
    }
}

fn run_doctor(args: &Args) -> Result<()> {
    let report = build_report(args)?;
    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print_report(&report);
    }
    Ok(())
}

fn build_report(args: &Args) -> Result<DoctorReport> {
    let candidate_paths = candidate_claude_paths(args)?;
    let claude_paths: Vec<ClaudePathHealth> = candidate_paths
        .iter()
        .map(|path| ClaudePathHealth {
            path: path.display().to_string(),
            exists: path.exists(),
            has_projects: path.join("projects").is_dir(),
        })
        .collect();

    let active_paths = crate::utils::claude_paths(args.claude_config_dir.as_deref());
    let settings = inspect_settings(args)?;
    let db = crate::db::inspect_health();
    let usage_api = crate::usage_api::inspect_usage_api(&active_paths, Some("claude-sonnet-4-5"));
    let pricing_source = crate::pricing::pricing_source_for_model("claude-sonnet-4-5");
    let pricing = PricingHealth {
        probe_model: "claude-sonnet-4-5".to_string(),
        source: pricing_source,
        available: !matches!(pricing_source, PricingSource::Unavailable),
    };

    let mut warnings = Vec::new();
    if args.config_error.is_some() {
        warnings.push("config file could not be loaded".to_string());
    }
    if active_paths.is_empty() {
        warnings.push("no Claude projects directories were found".to_string());
    }
    if !settings.status_line_present {
        warnings.push(
            "Claude settings.json has no statusLine entry; run init to install it".to_string(),
        );
    } else if settings.refresh_interval.is_none() {
        warnings.push(
            "statusLine has no refreshInterval; Claude Code will not re-run the command on \
             terminal resize or refresh timed metrics between messages; run init to set it"
                .to_string(),
        );
    }
    if !db.ok {
        warnings.push("SQLite cache is not healthy".to_string());
    }
    if !pricing.available {
        warnings.push("pricing lookup failed for probe model".to_string());
    }

    let subsystems = SubsystemHealth {
        git: !args.no_subsystem_git,
        beads: !args.no_subsystem_beads,
        gastown: !args.no_subsystem_gastown,
        db_cache: !args.no_subsystem_db_cache,
        usage_api: !args.no_subsystem_usage_api,
    };

    let preset = PresetHealth {
        selected: args.preset.map(|p| match p {
            crate::cli::PresetArg::Minimal => "minimal",
            crate::cli::PresetArg::Default => "default",
            crate::cli::PresetArg::Full => "full",
        }),
    };

    let display_opt_in = DisplayToggleHealth {
        cost_breakdown: args.cost_breakdown,
        cost_provenance: args.cost_provenance,
        provider_key_source: args.provider_key_source,
        provider_name: args.provider_name,
        context_compact_hint_enabled: !args.no_context_compact_hint,
        integrations_prompt_cache_enabled: !args.no_integrations_prompt_cache,
    };

    let json_settings = JsonToggleHealth {
        subagents: !args.no_json_subagents,
        tokens_breakdown: !args.no_json_tokens_breakdown,
        duration: !args.no_json_duration,
        rate_limit: !args.no_json_rate_limit,
        usage_limits: !args.no_json_usage_limits,
    };

    Ok(DoctorReport {
        ok: warnings.is_empty(),
        warnings,
        config: ConfigHealth {
            loaded_path: args
                .config_loaded
                .as_ref()
                .map(|path| path.display().to_string()),
            error: args.config_error.clone(),
        },
        claude_paths,
        settings,
        db,
        usage_api,
        pricing,
        subsystems,
        preset,
        display_opt_in,
        json_settings,
    })
}

fn print_report(report: &DoctorReport) {
    println!("claude_statusline doctor");
    println!("ok: {}", report.ok);
    if !report.warnings.is_empty() {
        println!("warnings:");
        for warning in &report.warnings {
            println!("  - {}", warning);
        }
    }
    println!(
        "config: {}",
        report.config.loaded_path.as_deref().unwrap_or("not loaded")
    );
    if let Some(error) = &report.config.error {
        println!("config_error: {}", error);
    }
    println!("claude_paths:");
    for path in &report.claude_paths {
        println!(
            "  - {} exists={} projects={}",
            path.path, path.exists, path.has_projects
        );
    }
    println!(
        "settings: {} statusLine={} command={} refreshInterval={} subagentStatusLine={} subagentCommand={}",
        report.settings.path,
        report.settings.status_line_present,
        report.settings.command.as_deref().unwrap_or("n/a"),
        report
            .settings
            .refresh_interval
            .map(|v| v.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        report.settings.subagent_status_line_present,
        report.settings.subagent_command.as_deref().unwrap_or("n/a")
    );
    println!(
        "db: {} ok={} wal={} schema={} user_version={} cache_version={}",
        report.db.path,
        report.db.ok,
        report.db.journal_mode.as_deref().unwrap_or("unknown"),
        report.db.schema_version.as_deref().unwrap_or("unknown"),
        report
            .db
            .user_version
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        report
            .db
            .usage_cache_version
            .as_deref()
            .unwrap_or("unknown")
    );
    println!(
        "usage_api: direct={} token={} cache={} stale_cache={} negative_cache={} ttl={}s",
        report.usage_api.direct_claude_api,
        report.usage_api.oauth_token_present,
        report.usage_api.fresh_cache_present,
        report.usage_api.stale_cache_present,
        report.usage_api.negative_cache_active,
        report.usage_api.cache_ttl_seconds
    );
    if !report.usage_api.unknown_response_fields.is_empty() {
        println!(
            "usage_api: unmapped response fields: {}",
            report.usage_api.unknown_response_fields.join(", ")
        );
    }
    println!(
        "usage_api egress: {}{}",
        report.usage_api.egress.route,
        match &report.usage_api.egress.extra_ca {
            Some(path) => format!(" (extra CA: {})", path),
            None => String::new(),
        }
    );
    println!(
        "pricing: model={} source={}",
        report.pricing.probe_model,
        report.pricing.source.as_str()
    );
    println!(
        "subsystems: git={} beads={} gastown={} db_cache={} usage_api={}",
        report.subsystems.git,
        report.subsystems.beads,
        report.subsystems.gastown,
        report.subsystems.db_cache,
        report.subsystems.usage_api
    );
    println!("preset: {}", report.preset.selected.unwrap_or("(none)"));
    println!(
        "display opt-ins: breakdown={} provenance={} provider_key={} provider_name={} compact_hint={} prompt_cache={}",
        report.display_opt_in.cost_breakdown,
        report.display_opt_in.cost_provenance,
        report.display_opt_in.provider_key_source,
        report.display_opt_in.provider_name,
        report.display_opt_in.context_compact_hint_enabled,
        report.display_opt_in.integrations_prompt_cache_enabled
    );
    println!(
        "json: subagents={} tokens_breakdown={} duration={} rate_limit={} usage_limits={}",
        report.json_settings.subagents,
        report.json_settings.tokens_breakdown,
        report.json_settings.duration,
        report.json_settings.rate_limit,
        report.json_settings.usage_limits
    );
}

fn run_init(args: &Args, init: &InitArgs) -> Result<()> {
    let settings_path = settings_path(args)?;
    let command = init
        .command
        .clone()
        .unwrap_or_else(|| "claude_statusline".to_string());
    let updated =
        build_updated_settings(&settings_path, &command, init.refresh_interval, init.force)?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "settings_path": settings_path,
                "dry_run": init.dry_run,
                "statusLine": updated.get("statusLine"),
                "subagentStatusLine": updated.get("subagentStatusLine"),
            }))?
        );
    } else if init.dry_run {
        println!("would update {}", settings_path.display());
        println!("{}", serde_json::to_string_pretty(&updated)?);
    }

    if !init.dry_run {
        if let Some(parent) = settings_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(
            &settings_path,
            format!("{}\n", serde_json::to_string_pretty(&updated)?),
        )
        .with_context(|| format!("failed to write {}", settings_path.display()))?;
        if !args.json {
            println!("updated {}", settings_path.display());
            println!("statusLine.command = {}", command);
            println!("statusLine.refreshInterval = {}", init.refresh_interval);
            println!("subagentStatusLine.command = {}", command);
        }
    }

    Ok(())
}

fn build_updated_settings(
    settings_path: &PathBuf,
    command: &str,
    refresh_interval: u64,
    force: bool,
) -> Result<Value> {
    let mut root = if settings_path.is_file() {
        let raw = fs::read_to_string(settings_path)
            .with_context(|| format!("failed to read {}", settings_path.display()))?;
        serde_json::from_str::<Value>(&raw)
            .with_context(|| format!("failed to parse {}", settings_path.display()))?
    } else {
        Value::Object(Map::new())
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings root must be a JSON object"))?;

    if obj
        .get("statusLine")
        .is_some_and(|value| !value.is_object())
        && !force
    {
        return Err(anyhow!(
            "settings.statusLine exists but is not an object; rerun init with --force to replace it"
        ));
    }

    if obj
        .get("subagentStatusLine")
        .is_some_and(|value| !value.is_object())
        && !force
    {
        return Err(anyhow!(
            "settings.subagentStatusLine exists but is not an object; rerun init with --force to replace it"
        ));
    }

    let mut status_line = obj
        .get("statusLine")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    status_line.insert("type".to_string(), Value::String("command".to_string()));
    status_line.insert("command".to_string(), Value::String(command.to_string()));
    status_line.insert("padding".to_string(), Value::Number(0.into()));
    status_line.insert(
        "refreshInterval".to_string(),
        Value::Number(refresh_interval.into()),
    );
    obj.insert("statusLine".to_string(), Value::Object(status_line));

    // Claude Code's subagentStatusLine schema admits only type and command
    // (no padding or refreshInterval keys).
    let mut subagent_status_line = obj
        .get("subagentStatusLine")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    subagent_status_line.insert("type".to_string(), Value::String("command".to_string()));
    subagent_status_line.insert("command".to_string(), Value::String(command.to_string()));
    obj.insert(
        "subagentStatusLine".to_string(),
        Value::Object(subagent_status_line),
    );

    Ok(root)
}

fn inspect_settings(args: &Args) -> Result<SettingsHealth> {
    let path = settings_path(args)?;
    let exists = path.is_file();
    let value = if exists {
        fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
    } else {
        None
    };
    let status = value
        .as_ref()
        .and_then(|root| root.get("statusLine"))
        .and_then(|status| status.as_object());
    let command = status
        .and_then(|status| status.get("command"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let refresh_interval = status
        .and_then(|status| status.get("refreshInterval"))
        .and_then(|value| value.as_u64());
    let subagent = value
        .as_ref()
        .and_then(|root| root.get("subagentStatusLine"))
        .and_then(|status| status.as_object());
    let subagent_command = subagent
        .and_then(|status| status.get("command"))
        .and_then(|value| value.as_str())
        .map(str::to_string);

    // subagentStatusLine is optional, so its absence never degrades ok.
    Ok(SettingsHealth {
        path: path.display().to_string(),
        exists,
        status_line_present: status.is_some(),
        command,
        refresh_interval,
        subagent_status_line_present: subagent.is_some(),
        subagent_command,
        ok: status.is_some(),
    })
}

fn settings_path(args: &Args) -> Result<PathBuf> {
    if let Some(first) = args.claude_config_dir.as_deref().and_then(|paths| {
        paths
            .split(',')
            .map(str::trim)
            .find(|path| !path.is_empty())
    }) {
        return Ok(PathBuf::from(first).join("settings.json"));
    }

    let dirs = directories::BaseDirs::new().context("failed to locate home directory")?;
    Ok(dirs.home_dir().join(".claude").join("settings.json"))
}

fn candidate_claude_paths(args: &Args) -> Result<Vec<PathBuf>> {
    if let Some(paths) = args.claude_config_dir.as_deref() {
        let explicit: Vec<PathBuf> = paths
            .split(',')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .collect();
        if !explicit.is_empty() {
            return Ok(explicit);
        }
    }

    let dirs = directories::BaseDirs::new().context("failed to locate home directory")?;
    Ok(vec![
        dirs.home_dir().join(".claude"),
        dirs.config_dir().join("claude"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_updates_status_line_object() {
        let dir = tempdir().expect("tempdir");
        let settings_path = dir.path().join("settings.json");
        fs::write(&settings_path, r#"{"theme":"dark"}"#).expect("write settings");

        let updated = build_updated_settings(&settings_path, "claude_statusline --json", 7, false)
            .expect("settings update");

        assert_eq!(updated["theme"], "dark");
        assert_eq!(updated["statusLine"]["type"], "command");
        assert_eq!(updated["statusLine"]["command"], "claude_statusline --json");
        assert_eq!(updated["statusLine"]["padding"], 0);
        assert_eq!(updated["statusLine"]["refreshInterval"], 7);
    }

    #[test]
    fn init_installs_subagent_status_line_with_only_type_and_command() {
        let dir = tempdir().expect("tempdir");
        let settings_path = dir.path().join("settings.json");

        let updated = build_updated_settings(&settings_path, "claude_statusline", 5, false)
            .expect("settings update");

        let subagent = updated["subagentStatusLine"]
            .as_object()
            .expect("subagentStatusLine object");
        assert_eq!(subagent["type"], "command");
        assert_eq!(subagent["command"], "claude_statusline");
        assert_eq!(subagent.len(), 2, "only type and command are written");
    }

    #[test]
    fn init_preserves_extra_keys_on_existing_subagent_status_line() {
        let dir = tempdir().expect("tempdir");
        let settings_path = dir.path().join("settings.json");
        fs::write(
            &settings_path,
            r#"{"subagentStatusLine":{"command":"old-command","env":{"FOO":"1"}}}"#,
        )
        .expect("write settings");

        let updated = build_updated_settings(&settings_path, "claude_statusline", 5, false)
            .expect("settings update");

        assert_eq!(updated["subagentStatusLine"]["type"], "command");
        assert_eq!(
            updated["subagentStatusLine"]["command"],
            "claude_statusline"
        );
        assert_eq!(updated["subagentStatusLine"]["env"]["FOO"], "1");
    }

    #[test]
    fn inspect_settings_reads_subagent_status_line_command() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("settings.json"),
            r#"{
                "statusLine": {"type": "command", "command": "claude_statusline", "refreshInterval": 5},
                "subagentStatusLine": {"type": "command", "command": "claude_statusline --truecolor"}
            }"#,
        )
        .expect("write settings");
        let args = Args::parse_effective_from([
            "claude_statusline",
            "--no-config",
            "--claude-config-dir",
            dir.path().to_str().expect("utf8 path"),
        ]);

        let health = inspect_settings(&args).expect("inspect settings");
        assert!(health.status_line_present);
        assert!(health.subagent_status_line_present);
        assert_eq!(
            health.subagent_command.as_deref(),
            Some("claude_statusline --truecolor")
        );
        assert!(health.ok);
    }

    #[test]
    fn report_warns_when_status_line_lacks_refresh_interval() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("settings.json"),
            r#"{"statusLine": {"type": "command", "command": "claude_statusline"}}"#,
        )
        .expect("write settings");
        let args = Args::parse_effective_from([
            "claude_statusline",
            "--no-config",
            "--claude-config-dir",
            dir.path().to_str().expect("utf8 path"),
        ]);

        let report = build_report(&args).expect("build report");
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("refreshInterval")),
            "expected a refreshInterval warning, got {:?}",
            report.warnings
        );
    }

    #[test]
    fn report_does_not_warn_about_refresh_interval_when_present() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("settings.json"),
            r#"{"statusLine": {"type": "command", "command": "claude_statusline", "refreshInterval": 5}}"#,
        )
        .expect("write settings");
        let args = Args::parse_effective_from([
            "claude_statusline",
            "--no-config",
            "--claude-config-dir",
            dir.path().to_str().expect("utf8 path"),
        ]);

        let report = build_report(&args).expect("build report");
        assert!(
            !report
                .warnings
                .iter()
                .any(|w| w.contains("refreshInterval")),
            "did not expect a refreshInterval warning, got {:?}",
            report.warnings
        );
    }

    #[test]
    fn inspect_settings_stays_ok_without_subagent_status_line() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("settings.json"),
            r#"{"statusLine": {"type": "command", "command": "claude_statusline"}}"#,
        )
        .expect("write settings");
        let args = Args::parse_effective_from([
            "claude_statusline",
            "--no-config",
            "--claude-config-dir",
            dir.path().to_str().expect("utf8 path"),
        ]);

        let health = inspect_settings(&args).expect("inspect settings");
        assert!(!health.subagent_status_line_present);
        assert!(health.subagent_command.is_none());
        assert!(health.ok);
    }

    #[test]
    fn init_refuses_non_object_subagent_status_line_without_force() {
        let dir = tempdir().expect("tempdir");
        let settings_path = dir.path().join("settings.json");
        fs::write(&settings_path, r#"{"subagentStatusLine":"legacy"}"#).expect("write settings");

        let err = build_updated_settings(&settings_path, "claude_statusline", 5, false)
            .expect_err("non-object subagentStatusLine must be rejected");
        assert!(err.to_string().contains("subagentStatusLine"));

        let updated = build_updated_settings(&settings_path, "claude_statusline", 5, true)
            .expect("force replaces non-object value");
        assert_eq!(updated["subagentStatusLine"]["type"], "command");
        assert_eq!(
            updated["subagentStatusLine"]["command"],
            "claude_statusline"
        );
    }
}
