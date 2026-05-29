use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct HookModel {
    pub id: String,
    pub display_name: String,
}

#[derive(Deserialize, Debug)]
pub struct HookWorkspace {
    pub current_dir: String,
    pub project_dir: String,
    #[serde(default)]
    pub added_dirs: Vec<String>,
    pub git_worktree: Option<String>,
    pub repo: Option<HookRepo>,
}

/// Repository identity from the origin remote.
#[derive(Deserialize, Debug, Clone)]
pub struct HookRepo {
    pub host: String,
    pub owner: String,
    pub name: String,
}

#[derive(Deserialize, Debug)]
pub struct OutputStyle {
    pub name: String,
}

/// Aggregate cost summary from the modern Claude Code statusline hook schema.
#[derive(Deserialize, Debug)]
pub struct HookCost {
    pub total_cost_usd: f64,
    pub total_duration_ms: u64,
    pub total_api_duration_ms: u64,
    pub total_lines_added: i64,
    pub total_lines_removed: i64,
}

/// Current usage breakdown from the last API call
#[derive(Deserialize, Debug, Clone)]
pub struct HookCurrentUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

fn null_u32_as_zero<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u32>::deserialize(deserializer)?.unwrap_or(0))
}

/// Context window information from the modern Claude Code statusline hook schema.
///
/// `current_usage` and its percentages are null between API calls.
#[derive(Deserialize, Debug)]
pub struct HookContextWindow {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub context_window_size: u64,
    pub current_usage: Option<HookCurrentUsage>,
    #[serde(default, deserialize_with = "null_u32_as_zero")]
    pub used_percentage: u32,
    #[serde(default, deserialize_with = "null_u32_as_zero")]
    pub remaining_percentage: u32,
}

/// Subscription rate limit for a time window (5-hour or 7-day)
#[derive(Deserialize, Debug, Clone)]
pub struct HookRateLimit {
    pub used_percentage: Option<f64>,
    pub resets_at: Option<f64>,
}

/// Rate limits provided directly by Claude Code for subscribers
#[derive(Deserialize, Debug, Clone)]
pub struct HookRateLimits {
    pub five_hour: Option<HookRateLimit>,
    pub seven_day: Option<HookRateLimit>,
}

/// Reasoning effort information. Only emitted when the active model exposes
/// the effort capability.
#[derive(Deserialize, Debug, Clone)]
pub struct HookEffort {
    pub level: String,
}

/// Extended thinking state from the modern Claude Code statusline hook schema.
#[derive(Deserialize, Debug, Clone)]
pub struct HookThinking {
    pub enabled: bool,
}

/// Vim mode information
#[derive(Deserialize, Debug, Clone)]
pub struct HookVim {
    pub mode: String,
}

/// Agent information (when running with --agent)
#[derive(Deserialize, Debug, Clone)]
pub struct HookAgent {
    pub name: String,
    #[serde(rename = "type")]
    pub agent_type: Option<String>,
}

/// Worktree information (during --worktree sessions)
#[derive(Deserialize, Debug, Clone)]
pub struct HookWorktree {
    pub name: String,
    pub path: String,
    pub branch: Option<String>,
    pub original_cwd: Option<String>,
    pub original_branch: Option<String>,
}

/// Remote session information when Claude Code is connected to a remote host
#[derive(Deserialize, Debug, Clone)]
pub struct HookRemote {
    pub session_id: String,
}

/// Open PR metadata for the current branch.
#[derive(Deserialize, Debug, Clone)]
pub struct HookPr {
    pub number: u64,
    pub url: String,
    pub review_state: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct HookJson {
    pub session_id: String,
    pub transcript_path: String,
    pub model: HookModel,
    pub workspace: HookWorkspace,
    pub version: String,
    pub output_style: OutputStyle,
    /// Aggregate session cost from the modern Claude Code statusline hook schema.
    pub cost: HookCost,
    /// Context window snapshot from the modern Claude Code statusline hook schema.
    pub context_window: HookContextWindow,
    /// True when cumulative input tokens crossed Sonnet's 200k long-context tier.
    pub exceeds_200k_tokens: bool,
    /// Whether Claude Code fast mode is currently enabled.
    pub fast_mode: bool,
    /// Extended-thinking state for this session.
    pub thinking: HookThinking,
    /// Live reasoning effort level when the current model exposes the capability.
    pub effort: Option<HookEffort>,
    /// Subscription rate limits (internal field, not in public docs)
    pub rate_limits: Option<HookRateLimits>,
    /// Human-readable session name from /rename
    pub session_name: Option<String>,
    /// Vim mode when vim mode is enabled
    pub vim: Option<HookVim>,
    /// Agent info when running with --agent flag
    pub agent: Option<HookAgent>,
    /// Worktree info during --worktree sessions
    pub worktree: Option<HookWorktree>,
    /// Remote session info when connected via claude remote/assistant
    pub remote: Option<HookRemote>,
    /// Open PR info for the current branch
    pub pr: Option<HookPr>,
}
