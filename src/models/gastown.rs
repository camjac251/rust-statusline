//! Gas Town multi-agent orchestration data structures.
//!
//! Gas Town is a multi-agent orchestration system for Claude agents.
//! See: https://github.com/steveyegge/gastown

use serde::{Deserialize, Serialize};

/// Agent types in the Gas Town hierarchy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    /// Town-level coordinator (red, 🎩)
    Mayor,
    /// Town-level background worker (yellow, 🐺)
    Deacon,
    /// Rig-level supervisor (cyan, 🦉)
    Witness,
    /// Rig-level merge queue (blue, 🏭)
    Refinery,
    /// Established worker with persistent workspace (green, 👷)
    Crew,
    /// Transient worker with ephemeral worktree (white, 😺)
    Polecat,
}

impl AgentType {
    /// Parse agent type from role string
    pub fn parse(role: &str) -> Option<Self> {
        match role.to_lowercase().as_str() {
            "mayor" => Some(Self::Mayor),
            "deacon" => Some(Self::Deacon),
            "witness" => Some(Self::Witness),
            "refinery" => Some(Self::Refinery),
            "crew" => Some(Self::Crew),
            "polecat" => Some(Self::Polecat),
            _ => None,
        }
    }

    /// Get the display emoji for this agent type
    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Mayor => "🎩",
            Self::Deacon => "🐺",
            Self::Witness => "🦉",
            Self::Refinery => "🏭",
            Self::Crew => "👷",
            Self::Polecat => "😺",
        }
    }

    /// Get the role name
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mayor => "mayor",
            Self::Deacon => "deacon",
            Self::Witness => "witness",
            Self::Refinery => "refinery",
            Self::Crew => "crew",
            Self::Polecat => "polecat",
        }
    }
}

/// Rig status indicators for LED display
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RigStatus {
    /// Both witness and refinery running (🟢)
    Active,
    /// One of witness/refinery running (🟡)
    Partial,
    /// Neither running (⚫)
    Inactive,
}

impl RigStatus {
    /// Get the LED emoji for this status
    pub fn led(&self) -> &'static str {
        match self {
            Self::Active => "🟢",
            Self::Partial => "🟡",
            Self::Inactive => "⚫",
        }
    }
}

/// Information about a single rig
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigInfo {
    /// Rig name
    pub name: String,
    /// Status (active/partial/inactive)
    pub status: RigStatus,
    /// Number of polecats in this rig
    pub polecat_count: usize,
    /// Number of crew workers in this rig
    pub crew_count: usize,
    /// Whether witness is running
    pub has_witness: bool,
    /// Whether refinery is running
    pub has_refinery: bool,
}

/// Mail message summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailPreview {
    /// Number of unread messages
    pub unread_count: usize,
    /// Subject of the most recent unread message (truncated)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Current agent's identity in the Gas Town hierarchy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// Agent type (mayor, deacon, witness, refinery, crew, polecat)
    pub agent_type: AgentType,
    /// Rig name (if rig-level agent)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig: Option<String>,
    /// Agent name (for crew/polecat)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Full identity path (e.g., "gastown/crew/max" or "mayor")
    pub identity: String,
}

/// Complete Gas Town information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GasTownInfo {
    /// Path to the town root directory
    pub town_root: String,
    /// Town name from config
    #[serde(skip_serializing_if = "Option::is_none")]
    pub town_name: Option<String>,
    /// Current agent's identity
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentIdentity>,
    /// Mail inbox preview
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mail: Option<MailPreview>,
    /// Current hooked issue (from GT_ISSUE env var)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooked_issue: Option<String>,
    /// Rig status information (for mayor/deacon views)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub rigs: Vec<RigInfo>,
    /// Total polecat count across all rigs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_polecats: Option<usize>,
    /// Refinery merge queue info (for refinery view)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refinery_queue: Option<RefineryQueue>,
}

/// Refinery merge queue status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefineryQueue {
    /// Currently merging issue ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// Number of items pending in queue
    pub pending: usize,
}
