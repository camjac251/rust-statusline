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
    Args, BurnScopeArg, GitArg, LabelsArg, PresetArg, TimeFormatArg, WindowAnchorArg,
    WindowScopeArg,
};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct FileConfig {
    pub json: Option<bool>,
    pub labels: Option<LabelsArg>,
    pub git: Option<GitArg>,
    pub time_fmt: Option<TimeFormatArg>,
    pub truecolor: Option<bool>,
    pub prompt_cache_ttl_seconds: Option<u64>,
    pub burn_scope: Option<BurnScopeArg>,
    pub window_scope: Option<WindowScopeArg>,
    pub window_anchor: Option<WindowAnchorArg>,
    pub preset: Option<PresetArg>,
    pub subsystems: SubsystemFileConfig,
    pub display: DisplayFileConfig,
    pub json_settings: JsonFileConfig,
}

/// JSON-only opt-out toggles. Positive semantics in TOML: `true` keeps the
/// field, `false` omits it. Args use negative semantics (`no_json_*`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct JsonFileConfig {
    pub subagents: Option<bool>,
    pub tokens_breakdown: Option<bool>,
    pub duration: Option<bool>,
    pub rate_limit: Option<bool>,
    pub usage_limits: Option<bool>,
}

/// Display.* atomic toggles. All values use positive semantics:
/// `Some(true)` means the token is shown, `Some(false)` means it is hidden.
/// Args use negative semantics (`no_<section>_<element>`), so apply_config
/// inverts to set the args bool.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DisplayFileConfig {
    // cost.*
    pub cost_session: Option<bool>,
    pub cost_today: Option<bool>,
    pub cost_window: Option<bool>,
    pub cost_breakdown: Option<bool>,
    pub cost_provenance: Option<bool>,
    pub cost_lines_delta: Option<bool>,
    // usage.*
    pub usage_five_hour: Option<bool>,
    pub usage_weekly: Option<bool>,
    pub usage_opus: Option<bool>,
    pub usage_sonnet: Option<bool>,
    pub usage_extra: Option<bool>,
    // context.*
    pub context_tokens: Option<bool>,
    pub context_percent: Option<bool>,
    pub context_compact_hint: Option<bool>,
    // git.*
    pub git_branch: Option<bool>,
    pub git_dirty: Option<bool>,
    pub git_ahead_behind: Option<bool>,
    pub git_worktree: Option<bool>,
    // workspace.*
    pub workspace_cwd: Option<bool>,
    pub workspace_added_dirs: Option<bool>,
    pub workspace_model: Option<bool>,
    pub workspace_fast_mode_indicator: Option<bool>,
    pub workspace_agent: Option<bool>,
    pub workspace_output_style: Option<bool>,
    pub workspace_effort: Option<bool>,
    // integrations.*
    pub integrations_beads: Option<bool>,
    pub integrations_beads_alerts: Option<bool>,
    pub integrations_gastown: Option<bool>,
    pub integrations_prompt_cache: Option<bool>,
    // provider.*
    pub provider_key_source: Option<bool>,
    pub provider_name: Option<bool>,
}

/// Subsystem on/off toggles. `true` keeps the subsystem enabled (default).
/// `false` short-circuits the work for that subsystem entirely.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SubsystemFileConfig {
    pub git: Option<bool>,
    pub beads: Option<bool>,
    pub gastown: Option<bool>,
    pub db_cache: Option<bool>,
    pub usage_api: Option<bool>,
}

pub fn parse_effective_args<I, T>(itr: I) -> Args
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = Args::command().get_matches_from(itr);
    let mut args = Args::from_arg_matches(&matches).unwrap_or_else(|err| err.exit());

    let loaded_config = if args.no_config {
        None
    } else {
        match load_config(args.config.as_deref()) {
            Ok(Some((path, config))) => {
                args.config_loaded = Some(path);
                Some(config)
            }
            Ok(None) => None,
            Err(err) => {
                args.config_error = Some(err.to_string());
                None
            }
        }
    };

    // Resolve effective preset: CLI/env > config > none. Apply it first so that
    // TOML / env / CLI atomic toggles still override the preset values.
    let effective_preset = args
        .preset
        .or_else(|| loaded_config.as_ref().and_then(|c| c.preset));
    if let Some(preset) = effective_preset {
        if args.preset.is_none() {
            args.preset = Some(preset);
        }
        apply_preset(&mut args, &matches, preset);
    }

    if let Some(config) = loaded_config {
        apply_config(&mut args, &matches, &config);
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
    if !arg_was_user_set(matches, "truecolor") && std::env::var("CLAUDE_TRUECOLOR").is_err() {
        if let Some(value) = config.truecolor {
            args.truecolor = value;
        }
    }
    if !arg_was_user_set(matches, "prompt_cache_ttl_seconds") {
        if let Some(value) = config.prompt_cache_ttl_seconds {
            args.prompt_cache_ttl_seconds = Some(value);
        }
    }

    // display.* atomic toggles. TOML positive (true = visible),
    // Args negative (no_<section>_<element>: true = hidden).
    apply_display_toggle(
        matches,
        "no_cost_session",
        config.display.cost_session,
        &mut args.no_cost_session,
    );
    apply_display_toggle(
        matches,
        "no_cost_today",
        config.display.cost_today,
        &mut args.no_cost_today,
    );
    apply_display_toggle(
        matches,
        "no_cost_window",
        config.display.cost_window,
        &mut args.no_cost_window,
    );
    apply_display_toggle(
        matches,
        "no_cost_lines_delta",
        config.display.cost_lines_delta,
        &mut args.no_cost_lines_delta,
    );
    apply_display_opt_in(
        matches,
        "cost_breakdown",
        config.display.cost_breakdown,
        &mut args.cost_breakdown,
    );
    apply_display_opt_in(
        matches,
        "cost_provenance",
        config.display.cost_provenance,
        &mut args.cost_provenance,
    );

    apply_display_toggle(
        matches,
        "no_usage_five_hour",
        config.display.usage_five_hour,
        &mut args.no_usage_five_hour,
    );
    apply_display_toggle(
        matches,
        "no_usage_weekly",
        config.display.usage_weekly,
        &mut args.no_usage_weekly,
    );
    apply_display_toggle(
        matches,
        "no_usage_opus",
        config.display.usage_opus,
        &mut args.no_usage_opus,
    );
    apply_display_toggle(
        matches,
        "no_usage_sonnet",
        config.display.usage_sonnet,
        &mut args.no_usage_sonnet,
    );
    apply_display_toggle(
        matches,
        "no_usage_extra",
        config.display.usage_extra,
        &mut args.no_usage_extra,
    );

    apply_display_toggle(
        matches,
        "no_context_tokens",
        config.display.context_tokens,
        &mut args.no_context_tokens,
    );
    apply_display_toggle(
        matches,
        "no_context_percent",
        config.display.context_percent,
        &mut args.no_context_percent,
    );
    apply_display_toggle(
        matches,
        "no_context_compact_hint",
        config.display.context_compact_hint,
        &mut args.no_context_compact_hint,
    );

    apply_display_toggle(
        matches,
        "no_git_branch",
        config.display.git_branch,
        &mut args.no_git_branch,
    );
    apply_display_toggle(
        matches,
        "no_git_dirty",
        config.display.git_dirty,
        &mut args.no_git_dirty,
    );
    apply_display_toggle(
        matches,
        "no_git_ahead_behind",
        config.display.git_ahead_behind,
        &mut args.no_git_ahead_behind,
    );
    apply_display_toggle(
        matches,
        "no_git_worktree",
        config.display.git_worktree,
        &mut args.no_git_worktree,
    );

    apply_display_toggle(
        matches,
        "no_workspace_cwd",
        config.display.workspace_cwd,
        &mut args.no_workspace_cwd,
    );
    apply_display_toggle(
        matches,
        "no_workspace_added_dirs",
        config.display.workspace_added_dirs,
        &mut args.no_workspace_added_dirs,
    );
    apply_display_toggle(
        matches,
        "no_workspace_model",
        config.display.workspace_model,
        &mut args.no_workspace_model,
    );
    apply_display_toggle(
        matches,
        "no_workspace_fast_mode_indicator",
        config.display.workspace_fast_mode_indicator,
        &mut args.no_workspace_fast_mode_indicator,
    );
    apply_display_toggle(
        matches,
        "no_workspace_agent",
        config.display.workspace_agent,
        &mut args.no_workspace_agent,
    );
    apply_display_toggle(
        matches,
        "no_workspace_output_style",
        config.display.workspace_output_style,
        &mut args.no_workspace_output_style,
    );
    apply_display_toggle(
        matches,
        "no_workspace_effort",
        config.display.workspace_effort,
        &mut args.no_workspace_effort,
    );

    apply_display_toggle(
        matches,
        "no_integrations_beads",
        config.display.integrations_beads,
        &mut args.no_integrations_beads,
    );
    apply_display_toggle(
        matches,
        "no_integrations_beads_alerts",
        config.display.integrations_beads_alerts,
        &mut args.no_integrations_beads_alerts,
    );
    apply_display_toggle(
        matches,
        "no_integrations_gastown",
        config.display.integrations_gastown,
        &mut args.no_integrations_gastown,
    );
    apply_display_toggle(
        matches,
        "no_integrations_prompt_cache",
        config.display.integrations_prompt_cache,
        &mut args.no_integrations_prompt_cache,
    );

    apply_display_opt_in(
        matches,
        "provider_key_source",
        config.display.provider_key_source,
        &mut args.provider_key_source,
    );
    apply_display_opt_in(
        matches,
        "provider_name",
        config.display.provider_name,
        &mut args.provider_name,
    );

    // json.* opt-outs (TOML positive, args negative)
    apply_display_toggle(
        matches,
        "no_json_subagents",
        config.json_settings.subagents,
        &mut args.no_json_subagents,
    );
    apply_display_toggle(
        matches,
        "no_json_tokens_breakdown",
        config.json_settings.tokens_breakdown,
        &mut args.no_json_tokens_breakdown,
    );
    apply_display_toggle(
        matches,
        "no_json_duration",
        config.json_settings.duration,
        &mut args.no_json_duration,
    );
    apply_display_toggle(
        matches,
        "no_json_rate_limit",
        config.json_settings.rate_limit,
        &mut args.no_json_rate_limit,
    );
    apply_display_toggle(
        matches,
        "no_json_usage_limits",
        config.json_settings.usage_limits,
        &mut args.no_json_usage_limits,
    );
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
    // Subsystem toggles. TOML positive semantics (git = true means enabled);
    // Args negative semantics (no_subsystem_git = true means disabled).
    if !arg_was_user_set(matches, "no_subsystem_git") {
        if let Some(enabled) = config.subsystems.git {
            args.no_subsystem_git = !enabled;
        }
    }
    if !arg_was_user_set(matches, "no_subsystem_beads") {
        if let Some(enabled) = config.subsystems.beads {
            args.no_subsystem_beads = !enabled;
        }
    }
    if !arg_was_user_set(matches, "no_subsystem_gastown") {
        if let Some(enabled) = config.subsystems.gastown {
            args.no_subsystem_gastown = !enabled;
        }
    }
    if !arg_was_user_set(matches, "no_subsystem_db_cache") {
        if let Some(enabled) = config.subsystems.db_cache {
            args.no_subsystem_db_cache = !enabled;
        }
    }
    if !arg_was_user_set(matches, "no_subsystem_usage_api") {
        if let Some(enabled) = config.subsystems.usage_api {
            args.no_subsystem_usage_api = !enabled;
        }
    }
}

/// Apply a preset's display.* + subsystems.* defaults, respecting CLI/env wins.
fn apply_preset(args: &mut Args, matches: &clap::ArgMatches, preset: PresetArg) {
    match preset {
        PresetArg::Default => {}
        PresetArg::Minimal => apply_preset_minimal(args, matches),
        PresetArg::Full => apply_preset_full(args, matches),
    }
}

fn set_if_unset_neg(matches: &clap::ArgMatches, id: &str, target: &mut bool, hide: bool) {
    if !arg_was_user_set(matches, id) {
        *target = hide;
    }
}

fn set_if_unset_pos(matches: &clap::ArgMatches, id: &str, target: &mut bool, show: bool) {
    if !arg_was_user_set(matches, id) {
        *target = show;
    }
}

fn apply_preset_minimal(args: &mut Args, matches: &clap::ArgMatches) {
    // Cost: keep session, hide the rest
    set_if_unset_neg(matches, "no_cost_today", &mut args.no_cost_today, true);
    set_if_unset_neg(matches, "no_cost_window", &mut args.no_cost_window, true);
    set_if_unset_neg(
        matches,
        "no_cost_lines_delta",
        &mut args.no_cost_lines_delta,
        true,
    );
    // Usage: keep five_hour, hide the rest
    set_if_unset_neg(matches, "no_usage_weekly", &mut args.no_usage_weekly, true);
    set_if_unset_neg(matches, "no_usage_opus", &mut args.no_usage_opus, true);
    set_if_unset_neg(matches, "no_usage_sonnet", &mut args.no_usage_sonnet, true);
    set_if_unset_neg(matches, "no_usage_extra", &mut args.no_usage_extra, true);
    // Context: keep percent, hide tokens + compact hint
    set_if_unset_neg(
        matches,
        "no_context_tokens",
        &mut args.no_context_tokens,
        true,
    );
    set_if_unset_neg(
        matches,
        "no_context_compact_hint",
        &mut args.no_context_compact_hint,
        true,
    );
    // Git: keep branch + dirty, hide rest
    set_if_unset_neg(
        matches,
        "no_git_ahead_behind",
        &mut args.no_git_ahead_behind,
        true,
    );
    set_if_unset_neg(matches, "no_git_worktree", &mut args.no_git_worktree, true);
    // Workspace: keep cwd + model + fast_mode_indicator, hide rest
    set_if_unset_neg(
        matches,
        "no_workspace_added_dirs",
        &mut args.no_workspace_added_dirs,
        true,
    );
    set_if_unset_neg(
        matches,
        "no_workspace_agent",
        &mut args.no_workspace_agent,
        true,
    );
    set_if_unset_neg(
        matches,
        "no_workspace_output_style",
        &mut args.no_workspace_output_style,
        true,
    );
    set_if_unset_neg(
        matches,
        "no_workspace_effort",
        &mut args.no_workspace_effort,
        true,
    );
    // Integrations: hide all
    set_if_unset_neg(
        matches,
        "no_integrations_beads",
        &mut args.no_integrations_beads,
        true,
    );
    set_if_unset_neg(
        matches,
        "no_integrations_beads_alerts",
        &mut args.no_integrations_beads_alerts,
        true,
    );
    set_if_unset_neg(
        matches,
        "no_integrations_gastown",
        &mut args.no_integrations_gastown,
        true,
    );
    set_if_unset_neg(
        matches,
        "no_integrations_prompt_cache",
        &mut args.no_integrations_prompt_cache,
        true,
    );
    // Subsystems: skip the expensive ones not needed for minimal
    set_if_unset_neg(
        matches,
        "no_subsystem_beads",
        &mut args.no_subsystem_beads,
        true,
    );
    set_if_unset_neg(
        matches,
        "no_subsystem_gastown",
        &mut args.no_subsystem_gastown,
        true,
    );
    set_if_unset_neg(
        matches,
        "no_subsystem_usage_api",
        &mut args.no_subsystem_usage_api,
        true,
    );
}

fn apply_preset_full(args: &mut Args, matches: &clap::ArgMatches) {
    set_if_unset_pos(matches, "cost_breakdown", &mut args.cost_breakdown, true);
    set_if_unset_pos(matches, "cost_provenance", &mut args.cost_provenance, true);
    set_if_unset_pos(
        matches,
        "provider_key_source",
        &mut args.provider_key_source,
        true,
    );
    set_if_unset_pos(matches, "provider_name", &mut args.provider_name, true);
}

/// For default-on toggles (`no_<section>_<element>`): TOML true keeps it visible (args.no_* = false).
fn apply_display_toggle(
    matches: &clap::ArgMatches,
    arg_id: &str,
    enabled: Option<bool>,
    target: &mut bool,
) {
    if !arg_was_user_set(matches, arg_id) {
        if let Some(visible) = enabled {
            *target = !visible;
        }
    }
}

/// For default-off opt-in toggles (positive name, e.g. `cost_breakdown`): TOML true shows it.
fn apply_display_opt_in(
    matches: &clap::ArgMatches,
    arg_id: &str,
    enabled: Option<bool>,
    target: &mut bool,
) {
    if !arg_was_user_set(matches, arg_id) {
        if let Some(visible) = enabled {
            *target = visible;
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
            "preset" => config.preset = Some(parse_preset(value)?),
            "labels" => config.labels = Some(parse_labels(value)?),
            "git" => config.git = Some(parse_git(value)?),
            "git.verbosity" => config.git = Some(parse_git(value)?),
            "time" | "time_fmt" => config.time_fmt = Some(parse_time(value)?),
            "truecolor" => config.truecolor = Some(parse_bool(value)?),
            "prompt_cache_ttl_seconds" => config.prompt_cache_ttl_seconds = Some(parse_u64(value)?),
            "burn_scope" => config.burn_scope = Some(parse_burn_scope(value)?),
            "window_scope" => config.window_scope = Some(parse_window_scope(value)?),
            "window_anchor" => config.window_anchor = Some(parse_window_anchor(value)?),
            "subsystems.git" => config.subsystems.git = Some(parse_bool(value)?),
            "subsystems.beads" => config.subsystems.beads = Some(parse_bool(value)?),
            "subsystems.gastown" => config.subsystems.gastown = Some(parse_bool(value)?),
            "subsystems.db_cache" => config.subsystems.db_cache = Some(parse_bool(value)?),
            "subsystems.usage_api" => config.subsystems.usage_api = Some(parse_bool(value)?),
            // display.cost.*
            "cost.session" => config.display.cost_session = Some(parse_bool(value)?),
            "cost.today" => config.display.cost_today = Some(parse_bool(value)?),
            "cost.window" => config.display.cost_window = Some(parse_bool(value)?),
            "cost.breakdown" => config.display.cost_breakdown = Some(parse_bool(value)?),
            "cost.provenance" => config.display.cost_provenance = Some(parse_bool(value)?),
            "cost.lines_delta" => config.display.cost_lines_delta = Some(parse_bool(value)?),
            // display.usage.*
            "usage.five_hour" => config.display.usage_five_hour = Some(parse_bool(value)?),
            "usage.weekly" => config.display.usage_weekly = Some(parse_bool(value)?),
            "usage.opus" => config.display.usage_opus = Some(parse_bool(value)?),
            "usage.sonnet" => config.display.usage_sonnet = Some(parse_bool(value)?),
            "usage.extra" => config.display.usage_extra = Some(parse_bool(value)?),
            // display.context.*
            "context.tokens" => config.display.context_tokens = Some(parse_bool(value)?),
            "context.percent" => config.display.context_percent = Some(parse_bool(value)?),
            "context.compact_hint" => {
                config.display.context_compact_hint = Some(parse_bool(value)?)
            }
            // display.git.* (git.verbosity handled above as a mode selector)
            "git.branch" => config.display.git_branch = Some(parse_bool(value)?),
            "git.dirty" => config.display.git_dirty = Some(parse_bool(value)?),
            "git.ahead_behind" => config.display.git_ahead_behind = Some(parse_bool(value)?),
            "git.worktree" => config.display.git_worktree = Some(parse_bool(value)?),
            // display.workspace.*
            "workspace.cwd" => config.display.workspace_cwd = Some(parse_bool(value)?),
            "workspace.added_dirs" => {
                config.display.workspace_added_dirs = Some(parse_bool(value)?)
            }
            "workspace.model" => config.display.workspace_model = Some(parse_bool(value)?),
            "workspace.fast_mode_indicator" => {
                config.display.workspace_fast_mode_indicator = Some(parse_bool(value)?)
            }
            "workspace.agent" => config.display.workspace_agent = Some(parse_bool(value)?),
            "workspace.output_style" => {
                config.display.workspace_output_style = Some(parse_bool(value)?)
            }
            "workspace.effort" => config.display.workspace_effort = Some(parse_bool(value)?),
            // display.integrations.*
            "integrations.beads" => config.display.integrations_beads = Some(parse_bool(value)?),
            "integrations.beads_alerts" => {
                config.display.integrations_beads_alerts = Some(parse_bool(value)?)
            }
            "integrations.gastown" => {
                config.display.integrations_gastown = Some(parse_bool(value)?)
            }
            "integrations.prompt_cache" => {
                config.display.integrations_prompt_cache = Some(parse_bool(value)?)
            }
            // display.provider.*
            "provider.key_source" => config.display.provider_key_source = Some(parse_bool(value)?),
            "provider.name" => config.display.provider_name = Some(parse_bool(value)?),
            // json.*
            "json.subagents" => config.json_settings.subagents = Some(parse_bool(value)?),
            "json.tokens_breakdown" => {
                config.json_settings.tokens_breakdown = Some(parse_bool(value)?)
            }
            "json.duration" => config.json_settings.duration = Some(parse_bool(value)?),
            "json.rate_limit" => config.json_settings.rate_limit = Some(parse_bool(value)?),
            "json.usage_limits" => config.json_settings.usage_limits = Some(parse_bool(value)?),
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
        // Strip [display] so flat display keys and documented nested display
        // sections share the same match arms. Sections that carry meaning
        // (e.g. [subsystems]) keep their dotted prefix.
        with_section
            .strip_prefix("display.")
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

fn parse_preset(value: &str) -> Result<PresetArg> {
    match parse_string(value)?.trim().to_ascii_lowercase().as_str() {
        "minimal" => Ok(PresetArg::Minimal),
        "default" => Ok(PresetArg::Default),
        "full" => Ok(PresetArg::Full),
        other => Err(anyhow!("invalid preset value: {other}")),
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
            prompt_cache_ttl_seconds = 3600

            [display.cost]
            provenance = true
            today = false

            [display.integrations]
            prompt_cache = false
            "#,
        )
        .expect("config should parse");

        assert_eq!(config.labels, Some(LabelsArg::Long));
        assert_eq!(config.git, Some(GitArg::Verbose));
        assert_eq!(config.prompt_cache_ttl_seconds, Some(3600));
        assert_eq!(config.display.cost_provenance, Some(true));
        assert_eq!(config.display.cost_today, Some(false));
        assert_eq!(config.display.integrations_prompt_cache, Some(false));
    }
}
