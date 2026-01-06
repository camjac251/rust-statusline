//! Beads issue tracker integration.
//!
//! This module queries the local beads SQLite database (.beads/beads.db)
//! to provide issue tracking context in the statusline.
//!
//! Beads is a distributed, git-backed issue tracker for AI agents.
//! See: https://github.com/steveyegge/beads

use crate::models::{Bead, BeadStatus, BeadsCounts, BeadsInfo, PriorityCounts, TypeCounts};
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// Canonical database filename for beads
const BEADS_DB_NAME: &str = "beads.db";

/// Redirect filename that points to another .beads directory
const REDIRECT_FILE: &str = "redirect";

/// Find the .beads directory for a project
///
/// Walks up from the given path looking for a .beads directory.
/// Follows redirect files if present (single level only).
fn find_beads_dir(start_path: &Path) -> Option<PathBuf> {
    let mut current = start_path.to_path_buf();

    // Canonicalize if possible
    if let Ok(canonical) = current.canonicalize() {
        current = canonical;
    }

    loop {
        let beads_dir = current.join(".beads");
        if beads_dir.is_dir() {
            // Check for redirect file
            let redirect_path = beads_dir.join(REDIRECT_FILE);
            if redirect_path.is_file() {
                if let Ok(content) = std::fs::read_to_string(&redirect_path) {
                    // Parse redirect target (skip comments and empty lines)
                    for line in content.lines() {
                        let line = line.trim();
                        if !line.is_empty() && !line.starts_with('#') {
                            let target = if Path::new(line).is_absolute() {
                                PathBuf::from(line)
                            } else {
                                // Resolve relative to project root (parent of .beads)
                                current.join(line)
                            };
                            if target.is_dir() {
                                return Some(target);
                            }
                            break;
                        }
                    }
                }
            }
            return Some(beads_dir);
        }

        // Move up to parent
        if !current.pop() {
            break;
        }
    }

    None
}

/// Query beads information from the database
///
/// Returns None if:
/// - No .beads directory is found
/// - The database doesn't exist
/// - Any query errors occur
pub fn get_beads_info(project_dir: &Path) -> Option<BeadsInfo> {
    let beads_dir = find_beads_dir(project_dir)?;
    let db_path = beads_dir.join(BEADS_DB_NAME);

    if !db_path.is_file() {
        return None;
    }

    let conn = Connection::open(&db_path).ok()?;

    // Get status counts
    let counts = query_status_counts(&conn)?;

    // Get priority counts
    let priorities = query_priority_counts(&conn).unwrap_or_default();

    // Get type counts
    let types = query_type_counts(&conn).unwrap_or_default();

    // Get current work (hooked first, then in_progress)
    let current_work = query_current_work(&conn);

    // Get epic count (issues that have children)
    let epic_count = query_epic_count(&conn).unwrap_or(0);

    // Get top labels
    let top_labels = query_top_labels(&conn, 5).unwrap_or_default();

    let total_open = counts.open + counts.in_progress + counts.blocked + counts.hooked;

    Some(BeadsInfo {
        beads_dir: beads_dir.to_string_lossy().to_string(),
        current_work,
        counts,
        priorities,
        types,
        total_open,
        epic_count,
        top_labels,
    })
}

/// Query status counts for non-closed issues
fn query_status_counts(conn: &Connection) -> Option<BeadsCounts> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT status, COUNT(*) as count
            FROM issues
            WHERE status NOT IN ('closed', 'tombstone')
              AND (deleted_at IS NULL OR deleted_at = '')
            GROUP BY status
            "#,
        )
        .ok()?;

    let mut counts = BeadsCounts::default();

    let rows = stmt
        .query_map([], |row| {
            let status: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((status, count as usize))
        })
        .ok()?;

    for row in rows.flatten() {
        let (status, count) = row;
        match status.as_str() {
            "open" => counts.open = count,
            "in_progress" => counts.in_progress = count,
            "blocked" => counts.blocked = count,
            "hooked" => counts.hooked = count,
            "deferred" => counts.deferred = count,
            "pinned" => counts.pinned = count,
            _ => {}
        }
    }

    Some(counts)
}

/// Query the current work item (hooked or in_progress, highest priority first)
fn query_current_work(conn: &Connection) -> Option<Bead> {
    // Priority: hooked first (agent actively working), then in_progress
    // Within each status, sort by priority (lower = more critical)
    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, title, status, priority, issue_type, assignee, estimated_minutes
            FROM issues
            WHERE status IN ('hooked', 'in_progress')
              AND (deleted_at IS NULL OR deleted_at = '')
            ORDER BY
                CASE status WHEN 'hooked' THEN 0 ELSE 1 END,
                priority ASC,
                updated_at DESC
            LIMIT 1
            "#,
        )
        .ok()?;

    stmt.query_row([], |row| {
        let status_str: String = row.get(2)?;
        let status = BeadStatus::parse(&status_str).unwrap_or(BeadStatus::Open);

        Ok(Bead {
            id: row.get(0)?,
            title: row.get(1)?,
            status,
            priority: row.get(3)?,
            issue_type: row.get::<_, Option<String>>(4).ok().flatten(),
            assignee: row.get::<_, Option<String>>(5).ok().flatten(),
            estimated_minutes: row.get::<_, Option<i32>>(6).ok().flatten(),
            labels: Vec::new(), // Labels are in a separate table, skip for statusline
        })
    })
    .optional()
    .ok()
    .flatten()
}

/// Query priority counts for open issues (non-closed, non-tombstone)
fn query_priority_counts(conn: &Connection) -> Option<PriorityCounts> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT priority, COUNT(*) as count
            FROM issues
            WHERE status NOT IN ('closed', 'tombstone')
              AND (deleted_at IS NULL OR deleted_at = '')
            GROUP BY priority
            "#,
        )
        .ok()?;

    let mut counts = PriorityCounts::default();

    let rows = stmt
        .query_map([], |row| {
            let priority: i32 = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((priority, count as usize))
        })
        .ok()?;

    for row in rows.flatten() {
        let (priority, count) = row;
        match priority {
            0 => counts.p0_critical = count,
            1 => counts.p1_high = count,
            2 => counts.p2_medium = count,
            _ => counts.p3_p4_low += count,
        }
    }

    Some(counts)
}

/// Query issue type counts for open issues
fn query_type_counts(conn: &Connection) -> Option<TypeCounts> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT LOWER(COALESCE(issue_type, 'task')) as type, COUNT(*) as count
            FROM issues
            WHERE status NOT IN ('closed', 'tombstone')
              AND (deleted_at IS NULL OR deleted_at = '')
            GROUP BY type
            "#,
        )
        .ok()?;

    let mut counts = TypeCounts::default();

    let rows = stmt
        .query_map([], |row| {
            let issue_type: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((issue_type, count as usize))
        })
        .ok()?;

    for row in rows.flatten() {
        let (issue_type, count) = row;
        match issue_type.as_str() {
            "task" => counts.task = count,
            "bug" => counts.bug = count,
            "feature" => counts.feature = count,
            "epic" => counts.epic = count,
            _ => counts.other += count,
        }
    }

    Some(counts)
}

/// Count epics (issues with hierarchical IDs that have children)
/// Epics are detected by looking for issues whose IDs are prefixes of other issues
/// e.g., bd-abc is an epic if bd-abc.1 or bd-abc.2 exists
fn query_epic_count(conn: &Connection) -> Option<usize> {
    // Count issues that have children (their ID is a prefix of another issue's ID)
    let count: i64 = conn
        .query_row(
            r#"
            SELECT COUNT(DISTINCT parent.id)
            FROM issues parent
            JOIN issues child ON child.id LIKE parent.id || '.%'
            WHERE parent.status NOT IN ('closed', 'tombstone')
              AND (parent.deleted_at IS NULL OR parent.deleted_at = '')
              AND child.status NOT IN ('closed', 'tombstone')
              AND (child.deleted_at IS NULL OR child.deleted_at = '')
            "#,
            [],
            |row| row.get(0),
        )
        .ok()?;

    Some(count as usize)
}

/// Query top labels by usage count
fn query_top_labels(conn: &Connection, limit: usize) -> Option<Vec<(String, usize)>> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT l.label, COUNT(*) as count
            FROM labels l
            JOIN issues i ON l.issue_id = i.id
            WHERE i.status NOT IN ('closed', 'tombstone')
              AND (i.deleted_at IS NULL OR i.deleted_at = '')
            GROUP BY l.label
            ORDER BY count DESC
            LIMIT ?
            "#,
        )
        .ok()?;

    let rows = stmt
        .query_map([limit as i64], |row| {
            let label: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((label, count as usize))
        })
        .ok()?;

    Some(rows.flatten().collect())
}

/// Format a bead for display in the statusline
///
/// Returns something like "🪝 bd-a1b2: Fix the auth bug" for hooked,
/// or "bd-a1b2: Fix the auth bug" for in_progress.
pub fn format_bead_display(bead: &Bead, max_len: usize) -> String {
    let prefix = if bead.status == BeadStatus::Hooked {
        "🪝 "
    } else {
        ""
    };

    let display = format!("{}{}: {}", prefix, bead.id, bead.title);

    if display.chars().count() > max_len {
        // Truncate with ellipsis
        let truncated: String = display.chars().take(max_len - 1).collect();
        format!("{}…", truncated)
    } else {
        display
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bead_display() {
        let bead = Bead {
            id: "bd-a1b2".to_string(),
            title: "Fix authentication".to_string(),
            status: BeadStatus::InProgress,
            priority: 1,
            issue_type: None,
            assignee: None,
            estimated_minutes: None,
            labels: vec![],
        };

        let display = format_bead_display(&bead, 50);
        assert_eq!(display, "bd-a1b2: Fix authentication");
    }

    #[test]
    fn test_format_bead_display_hooked() {
        let bead = Bead {
            id: "bd-c3d4".to_string(),
            title: "Implement feature".to_string(),
            status: BeadStatus::Hooked,
            priority: 0,
            issue_type: None,
            assignee: None,
            estimated_minutes: None,
            labels: vec![],
        };

        let display = format_bead_display(&bead, 50);
        assert!(display.starts_with("🪝 "));
        assert!(display.contains("bd-c3d4"));
    }

    #[test]
    fn test_format_bead_display_truncation() {
        let bead = Bead {
            id: "bd-e5f6".to_string(),
            title: "This is a very long title that should be truncated".to_string(),
            status: BeadStatus::InProgress,
            priority: 2,
            issue_type: None,
            assignee: None,
            estimated_minutes: None,
            labels: vec![],
        };

        let display = format_bead_display(&bead, 30);
        assert!(display.chars().count() <= 30);
        assert!(display.ends_with('…'));
    }
}
