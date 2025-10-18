#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum TimeFormatArg {
    Auto,
    H12,
    H24,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum LabelsArg {
    Short,
    Long,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
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

#[derive(clap::Parser, Debug)]
pub struct Args {
    /// Force Claude data path(s), comma-separated. Defaults to ~/.config/claude and ~/.claude
    #[arg(long, env = "CLAUDE_CONFIG_DIR")]
    pub claude_config_dir: Option<String>,

    /// Emit JSON instead of colored text
    #[arg(long)]
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

    /// Show token breakdown segment in text output
    #[arg(long)]
    pub show_breakdown: bool,

    /// Enable truecolor accents (or set CLAUDE_TRUECOLOR=1)
    #[arg(long)]
    pub truecolor: bool,

    /// Show extra status hints (approaching limit, compact countdown)
    /// Can also be toggled via CLAUDE_STATUS_HINTS=1
    #[arg(long, env = "CLAUDE_STATUS_HINTS")]
    pub hints: bool,

    /// Burn scope: session|global (default: session)
    #[arg(long, value_enum, default_value_t = BurnScopeArg::Session)]
    pub burn_scope: BurnScopeArg,

    /// Window scope: global|project (default: global)
    #[arg(long, value_enum, default_value_t = WindowScopeArg::Global)]
    pub window_scope: WindowScopeArg,

    /// Debug mode: show detailed calculation information
    #[arg(long, env = "CLAUDE_DEBUG")]
    pub debug: bool,

    /// Window anchor: provider|log (default: log)
    /// provider uses a persisted/provider reset anchor if available;
    /// log uses heuristic log-derived 5-hour blocks (monitor-style)
    #[arg(long, value_enum, default_value_t = WindowAnchorArg::Log)]
    pub window_anchor: WindowAnchorArg,
    // OAuth/API options removed for offline-only mode
}

impl Args {
    pub fn parse() -> Self {
        <Args as clap::Parser>::parse()
    }
}
