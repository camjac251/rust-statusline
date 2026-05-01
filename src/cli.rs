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

    /// Show provider hint in header (hidden by default)
    #[arg(long)]
    pub show_provider: bool,

    /// Show cost/context/pricing source information
    #[arg(long)]
    pub show_provenance: bool,

    /// Show token breakdown segment in text output
    #[arg(long)]
    pub show_breakdown: bool,

    /// Enable truecolor accents (or set CLAUDE_TRUECOLOR=1)
    #[arg(long)]
    pub truecolor: bool,

    /// Show extra status hints (approaching limit, compact countdown)
    /// Enabled by default; disable with --no-hints or CLAUDE_STATUS_HINTS=0
    #[arg(long)]
    pub hints: bool,

    /// Disable status hints (overrides --hints and CLAUDE_STATUS_HINTS)
    #[arg(long)]
    pub no_hints: bool,

    /// Show prompt-cache countdown based on the last assistant response.
    /// Enabled by default; disable with --no-prompt-cache or CLAUDE_PROMPT_CACHE=0
    #[arg(long)]
    pub prompt_cache: bool,

    /// Disable prompt-cache countdown
    #[arg(long)]
    pub no_prompt_cache: bool,

    /// Prompt cache TTL in seconds
    #[arg(long, env = "CLAUDE_PROMPT_CACHE_TTL_SECONDS")]
    pub prompt_cache_ttl_seconds: Option<u64>,

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

    /// Disable SQLite database cache for global usage tracking
    /// Falls back to per-session scan_usage calculation (no global aggregation)
    #[arg(long, env = "CLAUDE_DB_CACHE_DISABLE")]
    pub no_db_cache: bool,

    /// Disable beads issue tracker integration
    /// Skips looking for .beads directory and querying issue status
    #[arg(long, env = "CLAUDE_NO_BEADS")]
    pub no_beads: bool,

    /// Disable Gas Town multi-agent integration
    /// Skips looking for mayor/town.json and querying agent context
    #[arg(long, env = "CLAUDE_NO_GASTOWN")]
    pub no_gastown: bool,

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
        let mut args = crate::config::parse_effective_args(itr);
        args.normalize();
        args
    }

    fn normalize(&mut self) {
        // Hints are on by default; --no-hints or CLAUDE_STATUS_HINTS=0 disables them
        if self.no_hints {
            self.hints = false;
        } else if !self.hints {
            // Neither --hints nor --no-hints passed; check env, default to true
            self.hints = match std::env::var("CLAUDE_STATUS_HINTS") {
                Ok(v) => !matches!(v.trim(), "0" | "false" | "no" | "off"),
                Err(_) => true,
            };
        }

        if self.no_prompt_cache {
            self.prompt_cache = false;
        } else if !self.prompt_cache {
            self.prompt_cache = match std::env::var("CLAUDE_PROMPT_CACHE") {
                Ok(v) => !matches!(v.trim(), "0" | "false" | "no" | "off"),
                Err(_) => true,
            };
        }
    }
}
