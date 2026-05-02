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
struct DoctorReport {
    ok: bool,
    warnings: Vec<String>,
    config: ConfigHealth,
    claude_paths: Vec<ClaudePathHealth>,
    settings: SettingsHealth,
    db: crate::db::DbHealth,
    usage_api: crate::usage_api::UsageApiHealth,
    pricing: PricingHealth,
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
    }
    if !db.ok {
        warnings.push("SQLite cache is not healthy".to_string());
    }
    if !pricing.available {
        warnings.push("pricing lookup failed for probe model".to_string());
    }

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
        "settings: {} statusLine={} command={} refreshInterval={}",
        report.settings.path,
        report.settings.status_line_present,
        report.settings.command.as_deref().unwrap_or("n/a"),
        report
            .settings
            .refresh_interval
            .map(|v| v.to_string())
            .unwrap_or_else(|| "n/a".to_string())
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
        "usage_api: fetch_enabled={} direct={} token={} cache={} stale_cache={} negative_cache={}",
        report.usage_api.fetch_enabled,
        report.usage_api.direct_claude_api,
        report.usage_api.oauth_token_present,
        report.usage_api.fresh_cache_present,
        report.usage_api.stale_cache_present,
        report.usage_api.negative_cache_active
    );
    println!(
        "pricing: model={} source={}",
        report.pricing.probe_model,
        report.pricing.source.as_str()
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

    Ok(SettingsHealth {
        path: path.display().to_string(),
        exists,
        status_line_present: status.is_some(),
        command,
        refresh_interval,
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
}
