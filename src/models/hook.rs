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
}
