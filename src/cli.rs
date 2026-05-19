use std::path::PathBuf;

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormatArg {
    Auto,
    H12,
    H24,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelsArg {
    Short,
    Long,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitArg {
    Minimal,
    Verbose,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnScopeArg {
    /// Per-minute burn for this session only (input+output tokens)
    Session,
    /// Per-minute burn across all projects in window (input+output tokens)
    Global,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowScopeArg {
    /// Aggregate window across all projects
    Global,
    /// Restrict window to current project only
    Project,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAnchorArg {
    /// Align windows to provider reset anchor if known
    Provider,
    /// Use log/heuristic 5-hour blocks (floored hour + 5h)
    Log,
}

/// Built-in presets that pre-configure display.* atomic toggles.
/// CLI / env / TOML atomic flags still win over the preset.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetArg {
    /// Bare-essentials statusline: cwd + model + session cost + 5-hour usage + context %
    Minimal,
    /// README baseline (all default-on tokens, no opt-ins)
    Default,
    /// Everything on including breakdown, provenance, and provider hints
    Full,
}

#[derive(clap::Subcommand, Debug, Clone)]
pub enum Command {
    /// Inspect local Claude/statusline configuration without reading hook stdin
    Doctor,
    /// Install or update Claude Code statusLine settings
    Init(InitArgs),
}

#[derive(clap::Args, Debug, Clone)]
pub struct InitArgs {
    /// Print the settings change without writing it
    #[arg(long)]
    pub dry_run: bool,

    /// Replace a non-object statusLine value if one exists
    #[arg(long)]
    pub force: bool,

    /// Command stored in Claude Code settings.json
    #[arg(long)]
    pub command: Option<String>,

    /// Claude Code statusLine refresh interval in seconds
    #[arg(long, default_value_t = 5)]
    pub refresh_interval: u64,
}

#[derive(clap::Parser, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Load options from a config file
    #[arg(long, global = true, env = "CLAUDE_STATUSLINE_CONFIG")]
    pub config: Option<PathBuf>,

    /// Disable config file loading
    #[arg(long, global = true)]
    pub no_config: bool,

    /// Force Claude data path(s), comma-separated. Defaults to ~/.config/claude and ~/.claude
    #[arg(long, global = true, env = "CLAUDE_CONFIG_DIR")]
    pub claude_config_dir: Option<String>,

    /// Emit JSON instead of colored text
    #[arg(long, global = true)]
    pub json: bool,

    /// Label verbosity for text output: short|long
    #[arg(long, value_enum, default_value_t = LabelsArg::Short)]
    pub labels: LabelsArg,

    /// Git segment style: minimal|verbose
    #[arg(long, value_enum, default_value_t = GitArg::Minimal)]
    pub git: GitArg,

    /// Time display: auto|12h|24h
    #[arg(long = "time", value_enum, default_value_t = TimeFormatArg::Auto)]
    pub time_fmt: TimeFormatArg,

    /// Enable truecolor accents (or set CLAUDE_TRUECOLOR=1)
    #[arg(long)]
    pub truecolor: bool,

    /// Prompt cache TTL in seconds
    #[arg(long, env = "CLAUDE_PROMPT_CACHE_TTL_SECONDS")]
    pub prompt_cache_ttl_seconds: Option<u64>,

    /// Apply a built-in preset (minimal|default|full). Atomic --no-* / --* CLI
    /// flags override the preset.
    #[arg(long, value_enum, global = true, env = "CLAUDE_STATUSLINE_PRESET")]
    pub preset: Option<PresetArg>,

    // ---- display.cost.* ----
    /// Hide the session cost token (`session:$X`)
    #[arg(
        long = "no-cost-session",
        global = true,
        env = "CLAUDE_STATUSLINE_COST_NO_SESSION"
    )]
    pub no_cost_session: bool,
    /// Hide the today cost token (`today:$X`)
    #[arg(
        long = "no-cost-today",
        global = true,
        env = "CLAUDE_STATUSLINE_COST_NO_TODAY"
    )]
    pub no_cost_today: bool,
    /// Hide the window cost token (`window:$X`)
    #[arg(
        long = "no-cost-window",
        global = true,
        env = "CLAUDE_STATUSLINE_COST_NO_WINDOW"
    )]
    pub no_cost_window: bool,
    /// Show the per-token-kind breakdown segment (opt-in)
    #[arg(
        long = "cost-breakdown",
        global = true,
        env = "CLAUDE_STATUSLINE_COST_BREAKDOWN"
    )]
    pub cost_breakdown: bool,
    /// Show the cost/pricing source provenance suffix (opt-in)
    #[arg(
        long = "cost-provenance",
        global = true,
        env = "CLAUDE_STATUSLINE_COST_PROVENANCE"
    )]
    pub cost_provenance: bool,
    /// Hide the lines-delta segment in the header
    #[arg(
        long = "no-cost-lines-delta",
        global = true,
        env = "CLAUDE_STATUSLINE_COST_NO_LINES_DELTA"
    )]
    pub no_cost_lines_delta: bool,

    // ---- display.usage.* ----
    /// Hide the 5-hour usage percent + inline reset token
    #[arg(
        long = "no-usage-five-hour",
        global = true,
        env = "CLAUDE_STATUSLINE_USAGE_NO_FIVE_HOUR"
    )]
    pub no_usage_five_hour: bool,
    /// Hide the weekly usage percent token
    #[arg(
        long = "no-usage-weekly",
        global = true,
        env = "CLAUDE_STATUSLINE_USAGE_NO_WEEKLY"
    )]
    pub no_usage_weekly: bool,
    /// Hide the per-model Opus usage percent token
    #[arg(
        long = "no-usage-opus",
        global = true,
        env = "CLAUDE_STATUSLINE_USAGE_NO_OPUS"
    )]
    pub no_usage_opus: bool,
    /// Hide the per-model Sonnet usage percent token
    #[arg(
        long = "no-usage-sonnet",
        global = true,
        env = "CLAUDE_STATUSLINE_USAGE_NO_SONNET"
    )]
    pub no_usage_sonnet: bool,
    /// Hide the paid extra-usage overage token
    #[arg(
        long = "no-usage-extra",
        global = true,
        env = "CLAUDE_STATUSLINE_USAGE_NO_EXTRA"
    )]
    pub no_usage_extra: bool,

    // ---- display.context.* ----
    /// Hide context token-count side of `ctx:N/L`
    #[arg(
        long = "no-context-tokens",
        global = true,
        env = "CLAUDE_STATUSLINE_CONTEXT_NO_TOKENS"
    )]
    pub no_context_tokens: bool,
    /// Hide context percent side of `ctx:N/L X%`
    #[arg(
        long = "no-context-percent",
        global = true,
        env = "CLAUDE_STATUSLINE_CONTEXT_NO_PERCENT"
    )]
    pub no_context_percent: bool,
    /// Hide the auto-compact `compact:@NK ~Nm` countdown chip
    #[arg(
        long = "no-context-compact-hint",
        global = true,
        env = "CLAUDE_STATUSLINE_CONTEXT_NO_COMPACT_HINT"
    )]
    pub no_context_compact_hint: bool,

    // ---- display.git.* ----
    /// Hide the branch name inside the git header segment
    #[arg(
        long = "no-git-branch",
        global = true,
        env = "CLAUDE_STATUSLINE_GIT_NO_BRANCH"
    )]
    pub no_git_branch: bool,
    /// Hide the dirty / clean indicator inside the git header segment
    #[arg(
        long = "no-git-dirty",
        global = true,
        env = "CLAUDE_STATUSLINE_GIT_NO_DIRTY"
    )]
    pub no_git_dirty: bool,
    /// Hide ahead / behind counts inside the git header segment
    #[arg(
        long = "no-git-ahead-behind",
        global = true,
        env = "CLAUDE_STATUSLINE_GIT_NO_AHEAD_BEHIND"
    )]
    pub no_git_ahead_behind: bool,
    /// Hide worktree segment (Claude internal worktrees + hook-provided linked worktree)
    #[arg(
        long = "no-git-worktree",
        global = true,
        env = "CLAUDE_STATUSLINE_GIT_NO_WORKTREE"
    )]
    pub no_git_worktree: bool,

    // ---- display.workspace.* ----
    /// Hide the cwd / directory header segment
    #[arg(
        long = "no-workspace-cwd",
        global = true,
        env = "CLAUDE_STATUSLINE_WORKSPACE_NO_CWD"
    )]
    pub no_workspace_cwd: bool,
    /// Hide the added-dirs header segment
    #[arg(
        long = "no-workspace-added-dirs",
        global = true,
        env = "CLAUDE_STATUSLINE_WORKSPACE_NO_ADDED_DIRS"
    )]
    pub no_workspace_added_dirs: bool,
    /// Hide the model header segment
    #[arg(
        long = "no-workspace-model",
        global = true,
        env = "CLAUDE_STATUSLINE_WORKSPACE_NO_MODEL"
    )]
    pub no_workspace_model: bool,
    /// Hide the fast-mode badge on the model segment
    #[arg(
        long = "no-workspace-fast-mode-indicator",
        global = true,
        env = "CLAUDE_STATUSLINE_WORKSPACE_NO_FAST_MODE_INDICATOR"
    )]
    pub no_workspace_fast_mode_indicator: bool,
    /// Hide the subagent name header segment
    #[arg(
        long = "no-workspace-agent",
        global = true,
        env = "CLAUDE_STATUSLINE_WORKSPACE_NO_AGENT"
    )]
    pub no_workspace_agent: bool,
    /// Hide the output-style header segment
    #[arg(
        long = "no-workspace-output-style",
        global = true,
        env = "CLAUDE_STATUSLINE_WORKSPACE_NO_OUTPUT_STYLE"
    )]
    pub no_workspace_output_style: bool,
    /// Hide the effort-level header segment
    #[arg(
        long = "no-workspace-effort",
        global = true,
        env = "CLAUDE_STATUSLINE_WORKSPACE_NO_EFFORT"
    )]
    pub no_workspace_effort: bool,

    // ---- display.integrations.* ----
    /// Hide the beads current-work / open-count header segment (does NOT skip the work; use --no-subsystem-beads for that)
    #[arg(
        long = "no-integrations-beads",
        global = true,
        env = "CLAUDE_STATUSLINE_INTEGRATIONS_NO_BEADS"
    )]
    pub no_integrations_beads: bool,
    /// Hide the beads P0 + blocked alert header segment
    #[arg(
        long = "no-integrations-beads-alerts",
        global = true,
        env = "CLAUDE_STATUSLINE_INTEGRATIONS_NO_BEADS_ALERTS"
    )]
    pub no_integrations_beads_alerts: bool,
    /// Hide the gastown header segment (does NOT skip the work; use --no-subsystem-gastown for that)
    #[arg(
        long = "no-integrations-gastown",
        global = true,
        env = "CLAUDE_STATUSLINE_INTEGRATIONS_NO_GASTOWN"
    )]
    pub no_integrations_gastown: bool,
    /// Hide the prompt-cache countdown token in the status line
    #[arg(
        long = "no-integrations-prompt-cache",
        global = true,
        env = "CLAUDE_STATUSLINE_INTEGRATIONS_NO_PROMPT_CACHE"
    )]
    pub no_integrations_prompt_cache: bool,

    // ---- display.provider.* ----
    /// Show the API key source hint in the provider header segment
    #[arg(
        long = "provider-key-source",
        global = true,
        env = "CLAUDE_STATUSLINE_PROVIDER_KEY_SOURCE"
    )]
    pub provider_key_source: bool,
    /// Show the provider name hint in the provider header segment
    #[arg(
        long = "provider-name",
        global = true,
        env = "CLAUDE_STATUSLINE_PROVIDER_NAME"
    )]
    pub provider_name: bool,

    // ---- json.* (JSON-only opt-outs; affects --json output only) ----
    /// Omit session.subagents from JSON output
    #[arg(
        long = "no-json-subagents",
        global = true,
        env = "CLAUDE_STATUSLINE_JSON_NO_SUBAGENTS"
    )]
    pub no_json_subagents: bool,
    /// Omit per-token-kind breakdown fields from session.tokens and window.* in JSON
    #[arg(
        long = "no-json-tokens-breakdown",
        global = true,
        env = "CLAUDE_STATUSLINE_JSON_NO_TOKENS_BREAKDOWN"
    )]
    pub no_json_tokens_breakdown: bool,
    /// Omit session timing fields (duration_ms, api_duration_ms, cost_per_hour, lines_added/removed)
    #[arg(
        long = "no-json-duration",
        global = true,
        env = "CLAUDE_STATUSLINE_JSON_NO_DURATION"
    )]
    pub no_json_duration: bool,
    /// Omit the top-level rate_limit object
    #[arg(
        long = "no-json-rate-limit",
        global = true,
        env = "CLAUDE_STATUSLINE_JSON_NO_RATE_LIMIT"
    )]
    pub no_json_rate_limit: bool,
    /// Omit the top-level usage_limits object (also gated by subsystems.usage_api)
    #[arg(
        long = "no-json-usage-limits",
        global = true,
        env = "CLAUDE_STATUSLINE_JSON_NO_USAGE_LIMITS"
    )]
    pub no_json_usage_limits: bool,
    /// Omit compatibility aliases: top-level cwd, project_dir, fast_mode, and `block` (duplicate of `window`)
    #[arg(
        long = "no-json-compat-aliases",
        global = true,
        env = "CLAUDE_STATUSLINE_JSON_NO_COMPAT_ALIASES"
    )]
    pub no_json_compat_aliases: bool,

    /// Burn scope: session|global (default: session)
    #[arg(long, value_enum, default_value_t = BurnScopeArg::Session)]
    pub burn_scope: BurnScopeArg,

    /// Window scope: global|project (default: global)
    #[arg(long, value_enum, default_value_t = WindowScopeArg::Global)]
    pub window_scope: WindowScopeArg,

    /// Debug mode: show detailed calculation information
    #[arg(long, env = "CLAUDE_DEBUG")]
    pub debug: bool,

    /// Window anchor: provider|log (default: provider)
    /// provider uses the actual reset time from API headers;
    /// log uses heuristic log-derived 5-hour blocks (monitor-style)
    #[arg(long, value_enum, default_value_t = WindowAnchorArg::Provider)]
    pub window_anchor: WindowAnchorArg,

    /// Disable git subsystem (skips gix repository inspection entirely)
    #[arg(
        long = "no-subsystem-git",
        global = true,
        env = "CLAUDE_STATUSLINE_SUBSYSTEM_NO_GIT"
    )]
    pub no_subsystem_git: bool,

    /// Disable beads subsystem (skips .beads directory + bd CLI calls)
    #[arg(
        long = "no-subsystem-beads",
        global = true,
        env = "CLAUDE_STATUSLINE_SUBSYSTEM_NO_BEADS"
    )]
    pub no_subsystem_beads: bool,

    /// Disable Gas Town subsystem (skips town.json + GT_* env reads)
    #[arg(
        long = "no-subsystem-gastown",
        global = true,
        env = "CLAUDE_STATUSLINE_SUBSYSTEM_NO_GASTOWN"
    )]
    pub no_subsystem_gastown: bool,

    /// Disable SQLite db cache for global usage tracking
    /// Falls back to per-session today_cost calculation
    #[arg(
        long = "no-subsystem-db-cache",
        global = true,
        env = "CLAUDE_STATUSLINE_SUBSYSTEM_NO_DB_CACHE"
    )]
    pub no_subsystem_db_cache: bool,

    /// Disable OAuth usage API subsystem (skips both get_usage_summary calls)
    #[arg(
        long = "no-subsystem-usage-api",
        global = true,
        env = "CLAUDE_STATUSLINE_SUBSYSTEM_NO_USAGE_API"
    )]
    pub no_subsystem_usage_api: bool,

    #[arg(skip)]
    pub config_loaded: Option<PathBuf>,

    #[arg(skip)]
    pub config_error: Option<String>,
}

impl Args {
    pub fn parse() -> Self {
        Self::parse_effective_from(std::env::args_os())
    }

    pub fn parse_effective_from<I, T>(itr: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        crate::config::parse_effective_args(itr)
    }
}
