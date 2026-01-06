//! Beads issue tracker data structures.
//!
//! These structures represent issues from a local beads database (.beads/beads.db).
//! Beads is a distributed, git-backed issue tracker for AI agents.

use serde::{Deserialize, Serialize};

/// Status values for beads issues
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadStatus {
    Open,
    InProgress,
    Blocked,
    Deferred,
    Closed,
    Tombstone,
    Pinned,
    Hooked,
}

impl BeadStatus {
    /// Parse a status string into a BeadStatus enum value
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "in_progress" => Some(Self::InProgress),
            "blocked" => Some(Self::Blocked),
            "deferred" => Some(Self::Deferred),
            "closed" => Some(Self::Closed),
            "tombstone" => Some(Self::Tombstone),
            "pinned" => Some(Self::Pinned),
            "hooked" => Some(Self::Hooked),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Deferred => "deferred",
            Self::Closed => "closed",
            Self::Tombstone => "tombstone",
            Self::Pinned => "pinned",
            Self::Hooked => "hooked",
        }
    }
}

/// A single bead (issue) from the tracker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bead {
    /// Issue ID (e.g., "bd-a1b2" or hierarchical "bd-a3f8.1.1")
    pub id: String,
    /// Issue title
    pub title: String,
    /// Current status
    pub status: BeadStatus,
    /// Priority (0=critical/P0 to 4=low/P4)
    pub priority: i32,
    /// Issue type (task, bug, feature, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    /// Who this is assigned to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Estimated minutes to complete
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_minutes: Option<i32>,
    /// Labels attached to this issue
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub labels: Vec<String>,
}

/// Summary of beads status counts
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BeadsCounts {
    pub open: usize,
    pub in_progress: usize,
    pub blocked: usize,
    pub hooked: usize,
    pub deferred: usize,
    pub pinned: usize,
}

/// Priority breakdown for urgency indication
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PriorityCounts {
    /// P0 - Critical/urgent issues
    pub p0_critical: usize,
    /// P1 - High priority issues
    pub p1_high: usize,
    /// P2 - Medium priority (default)
    pub p2_medium: usize,
    /// P3/P4 - Low priority issues
    pub p3_p4_low: usize,
}

/// Issue type breakdown
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TypeCounts {
    pub task: usize,
    pub bug: usize,
    pub feature: usize,
    pub epic: usize,
    pub other: usize,
}

/// Information about beads issues in the current project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeadsInfo {
    /// Path to the .beads directory
    pub beads_dir: String,
    /// Current work item (hooked takes priority, then in_progress)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_work: Option<Bead>,
    /// Count of issues by status
    pub counts: BeadsCounts,
    /// Count of issues by priority (for urgency alerts)
    pub priorities: PriorityCounts,
    /// Count of issues by type
    pub types: TypeCounts,
    /// Total number of open issues (open + in_progress + blocked + hooked)
    pub total_open: usize,
    /// Number of epics (parent issues with children)
    pub epic_count: usize,
    /// Top labels with counts (most common first)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub top_labels: Vec<(String, usize)>,
}
