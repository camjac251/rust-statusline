//! Lightweight file configuration for CLI defaults.
//!
//! The statusline path is hot, so this intentionally supports a small TOML-like
//! subset for top-level scalar options rather than pulling a full parser into
//! the release binary. Unknown keys are ignored so newer configs remain
//! forwards-compatible with older binaries.

use anyhow::{Context, Result, anyhow};
use clap::{CommandFactory, FromArgMatches};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{
    Args, BurnScopeArg, GitArg, LabelsArg, TimeFormatArg, WindowAnchorArg, WindowScopeArg,
};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct FileConfig {
    pub json: Option<bool>,
    pub labels: Option<LabelsArg>,
    pub git: Option<GitArg>,
    pub time_fmt: Option<TimeFormatArg>,
    pub show_provider: Option<bool>,
    pub show_provenance: Option<bool>,
    pub show_breakdown: Option<bool>,
    pub truecolor: Option<bool>,
    pub hints: Option<bool>,
    pub prompt_cache: Option<bool>,
    pub prompt_cache_ttl_seconds: Option<u64>,
    pub burn_scope: Option<BurnScopeArg>,
    pub window_scope: Option<WindowScopeArg>,
    pub window_anchor: Option<WindowAnchorArg>,
    pub no_db_cache: Option<bool>,
    pub no_beads: Option<bool>,
    pub no_gastown: Option<bool>,
}

pub fn parse_effective_args<I, T>(itr: I) -> Args
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = Args::command().get_matches_from(itr);
    let mut args = Args::from_arg_matches(&matches).unwrap_or_else(|err| err.exit());

    if args.no_config {
        return args;
    }

    match load_config(args.config.as_deref()) {
        Ok(Some((path, config))) => {
            apply_config(&mut args, &matches, &config);
            args.config_loaded = Some(path);
        }
        Ok(None) => {}
        Err(err) => {
            args.config_error = Some(err.to_string());
        }
    }

    args
}

pub fn load_config(explicit: Option<&Path>) -> Result<Option<(PathBuf, FileConfig)>> {
    let path = if let Some(path) = explicit {
        if !path.exists() {
            return Err(anyhow!("config file does not exist: {}", path.display()));
        }
        Some(path.to_path_buf())
    } else {
        discover_config_path()
    };

    let Some(path) = path else {
        return Ok(None);
    };

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let config = parse_config_str(&raw)
        .with_context(|| format!("failed to parse config file {}", path.display()))?;
    Ok(Some((path, config)))
}

fn discover_config_path() -> Option<PathBuf> {
    if let Ok(cwd) = std::env::current_dir() {
        let project = cwd.join(".claude-statusline.toml");
        if project.is_file() {
            return Some(project);
        }
    }

    let dirs = directories::BaseDirs::new()?;
    let user = dirs
        .config_dir()
        .join("claude-statusline")
        .join("config.toml");
    user.is_file().then_some(user)
}

fn apply_config(args: &mut Args, matches: &clap::ArgMatches, config: &FileConfig) {
    if !arg_was_user_set(matches, "json") {
        if let Some(value) = config.json {
            args.json = value;
        }
    }
    if !arg_was_user_set(matches, "labels") {
        if let Some(value) = config.labels {
            args.labels = value;
        }
    }
    if !arg_was_user_set(matches, "git") {
        if let Some(value) = config.git {
            args.git = value;
        }
    }
    if !arg_was_user_set(matches, "time_fmt") {
        if let Some(value) = config.time_fmt {
            args.time_fmt = value;
        }
    }
    if !arg_was_user_set(matches, "show_provider") {
        if let Some(value) = config.show_provider {
            args.show_provider = value;
        }
    }
    if !arg_was_user_set(matches, "show_provenance") {
        if let Some(value) = config.show_provenance {
            args.show_provenance = value;
        }
    }
    if !arg_was_user_set(matches, "show_breakdown") {
        if let Some(value) = config.show_breakdown {
            args.show_breakdown = value;
        }
    }
    if !arg_was_user_set(matches, "truecolor") && std::env::var("CLAUDE_TRUECOLOR").is_err() {
        if let Some(value) = config.truecolor {
            args.truecolor = value;
        }
    }
    if !arg_was_user_set(matches, "hints")
        && !arg_was_user_set(matches, "no_hints")
        && std::env::var("CLAUDE_STATUS_HINTS").is_err()
    {
        if let Some(value) = config.hints {
            if value {
                args.hints = true;
                args.no_hints = false;
            } else {
                args.hints = false;
                args.no_hints = true;
            }
        }
    }
    if !arg_was_user_set(matches, "prompt_cache")
        && !arg_was_user_set(matches, "no_prompt_cache")
        && std::env::var("CLAUDE_PROMPT_CACHE").is_err()
    {
        if let Some(value) = config.prompt_cache {
            if value {
                args.prompt_cache = true;
                args.no_prompt_cache = false;
            } else {
                args.prompt_cache = false;
                args.no_prompt_cache = true;
            }
        }
    }
    if !arg_was_user_set(matches, "prompt_cache_ttl_seconds") {
        if let Some(value) = config.prompt_cache_ttl_seconds {
            args.prompt_cache_ttl_seconds = Some(value);
        }
    }
    if !arg_was_user_set(matches, "burn_scope") {
        if let Some(value) = config.burn_scope {
            args.burn_scope = value;
        }
    }
    if !arg_was_user_set(matches, "window_scope") {
        if let Some(value) = config.window_scope {
            args.window_scope = value;
        }
    }
    if !arg_was_user_set(matches, "window_anchor") {
        if let Some(value) = config.window_anchor {
            args.window_anchor = value;
        }
    }
    if !arg_was_user_set(matches, "no_db_cache") {
        if let Some(value) = config.no_db_cache {
            args.no_db_cache = value;
        }
    }
    if !arg_was_user_set(matches, "no_beads") {
        if let Some(value) = config.no_beads {
            args.no_beads = value;
        }
    }
    if !arg_was_user_set(matches, "no_gastown") {
        if let Some(value) = config.no_gastown {
            args.no_gastown = value;
        }
    }
}

fn arg_was_user_set(matches: &clap::ArgMatches, id: &str) -> bool {
    matches.value_source(id).is_some_and(|source| {
        matches!(
            source,
            clap::parser::ValueSource::CommandLine | clap::parser::ValueSource::EnvVariable
        )
    })
}

fn parse_config_str(input: &str) -> Result<FileConfig> {
    let mut config = FileConfig::default();
    let mut section = String::new();

    for (line_no, raw_line) in input.lines().enumerate() {
        let line = strip_comment(raw_line).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }

        let (raw_key, raw_value) = line
            .split_once('=')
            .ok_or_else(|| anyhow!("line {}: expected key = value", line_no + 1))?;
        let key = normalize_key(&section, raw_key.trim());
        let value = raw_value.trim();

        match key.as_str() {
            "json" => config.json = Some(parse_bool(value)?),
            "labels" => config.labels = Some(parse_labels(value)?),
            "git" => config.git = Some(parse_git(value)?),
            "time" | "time_fmt" => config.time_fmt = Some(parse_time(value)?),
            "show_provider" => config.show_provider = Some(parse_bool(value)?),
            "show_provenance" => config.show_provenance = Some(parse_bool(value)?),
            "show_breakdown" => config.show_breakdown = Some(parse_bool(value)?),
            "truecolor" => config.truecolor = Some(parse_bool(value)?),
            "hints" => config.hints = Some(parse_bool(value)?),
            "prompt_cache" => config.prompt_cache = Some(parse_bool(value)?),
            "prompt_cache_ttl_seconds" => config.prompt_cache_ttl_seconds = Some(parse_u64(value)?),
            "burn_scope" => config.burn_scope = Some(parse_burn_scope(value)?),
            "window_scope" => config.window_scope = Some(parse_window_scope(value)?),
            "window_anchor" => config.window_anchor = Some(parse_window_anchor(value)?),
            "no_db_cache" => config.no_db_cache = Some(parse_bool(value)?),
            "no_beads" => config.no_beads = Some(parse_bool(value)?),
            "no_gastown" => config.no_gastown = Some(parse_bool(value)?),
            _ => {}
        }
    }

    Ok(config)
}

fn normalize_key(section: &str, key: &str) -> String {
    let normalized = key.trim().replace('-', "_").to_ascii_lowercase();
    if section.is_empty() {
        normalized
    } else {
        let with_section = format!("{}.{}", section, normalized);
        with_section
            .strip_prefix("display.")
            .or_else(|| with_section.strip_prefix("statusline."))
            .unwrap_or(&with_section)
            .to_string()
    }
}

fn strip_comment(line: &str) -> String {
    let mut in_string = false;
    let mut escaped = false;
    let mut out = String::new();

    for ch in line.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            out.push(ch);
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            out.push(ch);
            continue;
        }
        if ch == '#' && !in_string {
            break;
        }
        out.push(ch);
    }

    out
}

fn parse_string(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        Ok(trimmed[1..trimmed.len() - 1].to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match parse_string(value)?.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(anyhow!("invalid boolean value: {other}")),
    }
}

fn parse_u64(value: &str) -> Result<u64> {
    parse_string(value)?
        .trim()
        .parse::<u64>()
        .context("invalid unsigned integer")
}

fn parse_labels(value: &str) -> Result<LabelsArg> {
    match parse_string(value)?.trim().to_ascii_lowercase().as_str() {
        "short" => Ok(LabelsArg::Short),
        "long" => Ok(LabelsArg::Long),
        other => Err(anyhow!("invalid labels value: {other}")),
    }
}

fn parse_git(value: &str) -> Result<GitArg> {
    match parse_string(value)?.trim().to_ascii_lowercase().as_str() {
        "minimal" => Ok(GitArg::Minimal),
        "verbose" => Ok(GitArg::Verbose),
        other => Err(anyhow!("invalid git value: {other}")),
    }
}

fn parse_time(value: &str) -> Result<TimeFormatArg> {
    match parse_string(value)?.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(TimeFormatArg::Auto),
        "12h" | "h12" => Ok(TimeFormatArg::H12),
        "24h" | "h24" => Ok(TimeFormatArg::H24),
        other => Err(anyhow!("invalid time value: {other}")),
    }
}

fn parse_burn_scope(value: &str) -> Result<BurnScopeArg> {
    match parse_string(value)?.trim().to_ascii_lowercase().as_str() {
        "session" => Ok(BurnScopeArg::Session),
        "global" => Ok(BurnScopeArg::Global),
        other => Err(anyhow!("invalid burn_scope value: {other}")),
    }
}

fn parse_window_scope(value: &str) -> Result<WindowScopeArg> {
    match parse_string(value)?.trim().to_ascii_lowercase().as_str() {
        "global" => Ok(WindowScopeArg::Global),
        "project" => Ok(WindowScopeArg::Project),
        other => Err(anyhow!("invalid window_scope value: {other}")),
    }
}

fn parse_window_anchor(value: &str) -> Result<WindowAnchorArg> {
    match parse_string(value)?.trim().to_ascii_lowercase().as_str() {
        "provider" => Ok(WindowAnchorArg::Provider),
        "log" => Ok(WindowAnchorArg::Log),
        other => Err(anyhow!("invalid window_anchor value: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_display_section_config() {
        let config = parse_config_str(
            r#"
            [display]
            labels = "long"
            git = "verbose"
            show_provenance = true
            prompt_cache = false
            prompt_cache_ttl_seconds = 3600
            "#,
        )
        .expect("config should parse");

        assert_eq!(config.labels, Some(LabelsArg::Long));
        assert_eq!(config.git, Some(GitArg::Verbose));
        assert_eq!(config.show_provenance, Some(true));
        assert_eq!(config.prompt_cache, Some(false));
        assert_eq!(config.prompt_cache_ttl_seconds, Some(3600));
    }
}
