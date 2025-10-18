//! cc-sessions integration models

use serde::{Deserialize, Serialize};

/// cc-sessions mode (DAIC = discussion, GO = implementation)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionsMode {
    /// Discussion mode (DAIC)
    #[serde(rename = "discussion")]
    Discussion,
    /// Implementation mode (GO)
    #[serde(rename = "implementation")]
    Implementation,
}

/// Task state from sessions-state.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskState {
    pub name: Option<String>,
    pub file: Option<String>,
    pub branch: Option<String>,
    pub status: Option<String>,
    pub created: Option<String>,
    pub started: Option<String>,
    pub updated: Option<String>,
    pub dependencies: Option<Vec<String>>,
    pub submodules: Option<Vec<String>>,
}

/// Full sessions state file structure (minimal subset for statusline)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsState {
    pub version: Option<String>,
    pub current_task: Option<TaskState>,
    pub mode: Option<SessionsMode>,
    pub model: Option<String>,
}

/// Sessions information for display
#[derive(Debug, Clone, Serialize)]
pub struct SessionsInfo {
    pub detected: bool,
    pub current_task: Option<String>,
    pub mode: Option<String>,
    pub open_tasks: u32,
    pub edited_files: u32,
    pub upstream: Option<UpstreamInfo>,
}

/// Git upstream tracking info (ahead/behind)
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamInfo {
    pub ahead: u32,
    pub behind: u32,
}
