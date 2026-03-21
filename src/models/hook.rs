use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct HookModel {
    pub id: String,
    pub display_name: String,
}

#[derive(Deserialize, Debug)]
pub struct HookWorkspace {
    pub current_dir: String,
    pub project_dir: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct OutputStyle {
    pub name: String,
}

/// Optional cost summary provided by Claude Code's statusLine input
#[derive(Deserialize, Debug)]
pub struct HookCost {
    pub total_cost_usd: Option<f64>,
    pub total_duration_ms: Option<u64>,
    pub total_api_duration_ms: Option<u64>,
    pub total_lines_added: Option<i64>,
    pub total_lines_removed: Option<i64>,
}

/// Current usage breakdown from the last API call
#[derive(Deserialize, Debug, Clone)]
pub struct HookCurrentUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

/// Context window information provided by Claude Code
#[derive(Deserialize, Debug)]
pub struct HookContextWindow {
    /// Cumulative input tokens across the session
    pub total_input_tokens: Option<u64>,
    /// Cumulative output tokens across the session
    pub total_output_tokens: Option<u64>,
    /// Maximum context window size (respects API_MAX_INPUT_TOKENS if set)
    pub context_window_size: Option<u64>,
    /// Current context window usage from the last API call
    pub current_usage: Option<HookCurrentUsage>,
    /// Pre-calculated used percentage (input-only, from CLI)
    pub used_percentage: Option<u32>,
    /// Pre-calculated remaining percentage (from CLI)
    pub remaining_percentage: Option<u32>,
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

#[derive(Deserialize, Debug)]
pub struct HookJson {
    pub session_id: String,
    pub transcript_path: String,
    #[allow(dead_code)]
    pub cwd: Option<String>,
    pub model: HookModel,
    pub workspace: HookWorkspace,
    pub version: Option<String>,
    pub output_style: Option<OutputStyle>,
    /// Optional aggregate cost fields from Claude Code
    pub cost: Option<HookCost>,
    /// Context window information (added in Claude Code 2.0.69+)
    pub context_window: Option<HookContextWindow>,
    /// Whether tokens exceed 200k (long-context pricing threshold for Sonnet 4/4.5)
    #[serde(default)]
    pub exceeds_200k_tokens: Option<bool>,
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
}
