//! Gas Town multi-agent orchestration integration.
//!
//! This module detects Gas Town workspaces and extracts agent context
//! from environment variables and tmux sessions.
//!
//! Gas Town is a multi-agent orchestration system for Claude agents.
//! See: https://github.com/steveyegge/gastown

use crate::models::{
    AgentIdentity, AgentType, GasTownInfo, MailPreview, RefineryQueue, RigInfo, RigStatus,
};
use rusqlite::Connection;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Primary marker for Gas Town workspace (mayor/town.json)
const PRIMARY_MARKER: &str = "mayor/town.json";

/// Secondary marker for Gas Town workspace (mayor/ directory)
const SECONDARY_MARKER: &str = "mayor";

/// Beads database filename
const BEADS_DB_NAME: &str = "beads.db";

/// Minimal town.json structure for name extraction
#[derive(Debug, Deserialize)]
struct TownConfig {
    name: Option<String>,
}

/// Find the Gas Town root by walking up from the given directory.
///
/// Prefers mayor/town.json over mayor/ directory as workspace marker.
/// When in a worktree path (polecats/ or crew/), continues to outermost workspace.
fn find_town_root(start_dir: &Path) -> Option<PathBuf> {
    let abs_dir = start_dir.canonicalize().ok()?;
    let in_worktree = is_in_worktree_path(&abs_dir);

    let mut primary_match: Option<PathBuf> = None;
    let mut secondary_match: Option<PathBuf> = None;

    let mut current = abs_dir;
    loop {
        // Check for primary marker (mayor/town.json)
        if current.join(PRIMARY_MARKER).is_file() {
            if !in_worktree {
                return Some(current);
            }
            primary_match = Some(current.clone());
        }

        // Check for secondary marker (mayor/ directory)
        if secondary_match.is_none() {
            let sec_path = current.join(SECONDARY_MARKER);
            if sec_path.is_dir() {
                secondary_match = Some(current.clone());
            }
        }

        // Move up
        if !current.pop() {
            break;
        }
    }

    // Prefer primary match, fallback to secondary
    primary_match.or(secondary_match)
}

/// Check if path is inside a worktree directory
fn is_in_worktree_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    path_str.contains("/polecats/") || path_str.contains("/crew/")
}

/// Load town name from town.json config
fn load_town_name(town_root: &Path) -> Option<String> {
    let config_path = town_root.join(PRIMARY_MARKER);
    let content = std::fs::read_to_string(config_path).ok()?;
    let config: TownConfig = serde_json::from_str(&content).ok()?;
    config.name
}

/// Get agent identity from environment variables
///
/// Gas Town sets these in tmux sessions:
/// - GT_RIG: Rig name (for rig-level agents)
/// - GT_POLECAT: Polecat name
/// - GT_CREW: Crew worker name
/// - GT_ROLE: Agent role (mayor, deacon, witness, refinery, crew, polecat)
fn get_agent_identity_from_env() -> Option<AgentIdentity> {
    let role = env::var("GT_ROLE").ok();
    let rig = env::var("GT_RIG").ok();
    let polecat = env::var("GT_POLECAT").ok();
    let crew = env::var("GT_CREW").ok();

    // Determine agent type from role or infer from other vars
    let agent_type = if let Some(ref r) = role {
        AgentType::parse(r)?
    } else if polecat.is_some() {
        AgentType::Polecat
    } else if crew.is_some() {
        AgentType::Crew
    } else {
        return None;
    };

    // Build identity string
    let (name, identity) = match agent_type {
        AgentType::Mayor => (None, "mayor".to_string()),
        AgentType::Deacon => (None, "deacon".to_string()),
        AgentType::Witness => {
            let rig_name = rig.as_ref()?;
            (None, format!("{}/witness", rig_name))
        }
        AgentType::Refinery => {
            let rig_name = rig.as_ref()?;
            (None, format!("{}/refinery", rig_name))
        }
        AgentType::Crew => {
            let rig_name = rig.as_ref()?;
            let crew_name = crew.clone()?;
            (
                Some(crew_name.clone()),
                format!("{}/crew/{}", rig_name, crew_name),
            )
        }
        AgentType::Polecat => {
            let rig_name = rig.as_ref()?;
            let polecat_name = polecat.clone()?;
            (
                Some(polecat_name.clone()),
                format!("{}/{}", rig_name, polecat_name),
            )
        }
    };

    Some(AgentIdentity {
        agent_type,
        rig: rig.clone(),
        name,
        identity,
    })
}

/// Get hooked issue from GT_ISSUE environment variable
fn get_hooked_issue() -> Option<String> {
    env::var("GT_ISSUE").ok().filter(|s| !s.is_empty())
}

/// Query mail inbox from beads database
///
/// Returns unread count and preview of first unread message.
/// All gastown mail uses town-level beads ({townRoot}/.beads).
fn query_mail_inbox(town_root: &Path, identity: &str) -> Option<MailPreview> {
    // All mail uses town-level beads (rig-level beads are for project issues only)
    let town_beads = town_root.join(".beads");
    query_mail_from_beads(&town_beads, identity)
}

/// Query mail messages from a specific beads database
///
/// Gastown mail uses issue_type='message' with assignee = recipient identity.
/// All mail goes through town-level beads ({townRoot}/.beads).
fn query_mail_from_beads(beads_dir: &Path, identity: &str) -> Option<MailPreview> {
    let db_path = beads_dir.join(BEADS_DB_NAME);
    if !db_path.is_file() {
        return None;
    }

    let conn = Connection::open(&db_path).ok()?;

    // Query for open/hooked messages assigned to this identity
    // Gastown stores mail as beads issues with issue_type='message'
    let mut stmt = conn
        .prepare(
            r#"
            SELECT id, title
            FROM issues
            WHERE assignee = ?1
              AND status IN ('open', 'hooked')
              AND (deleted_at IS NULL OR deleted_at = '')
              AND issue_type = 'message'
            ORDER BY updated_at DESC
            "#,
        )
        .ok()?;

    let rows: Vec<(String, String)> = stmt
        .query_map([identity], |row| Ok((row.get(0)?, row.get(1)?)))
        .ok()?
        .flatten()
        .collect();

    if rows.is_empty() {
        return None;
    }

    let preview = rows.first().map(|(_, title)| {
        if title.len() > 45 {
            format!("{}…", &title[..44])
        } else {
            title.clone()
        }
    });

    Some(MailPreview {
        unread_count: rows.len(),
        preview,
    })
}

/// Get rig status by querying tmux sessions
///
/// Returns a list of rigs with their status (active/partial/inactive)
fn get_rig_status() -> Vec<RigInfo> {
    // Try to get tmux sessions
    let sessions = match get_tmux_sessions() {
        Some(s) => s,
        None => return Vec::new(),
    };

    // Track per-rig status
    let mut rig_status: HashMap<String, RigInfo> = HashMap::new();

    for session in sessions {
        if let Some((rig, agent_type, agent_name)) = parse_session_name(&session) {
            let entry = rig_status.entry(rig.clone()).or_insert(RigInfo {
                name: rig,
                status: RigStatus::Inactive,
                polecat_count: 0,
                crew_count: 0,
                has_witness: false,
                has_refinery: false,
            });

            match agent_type {
                AgentType::Witness => entry.has_witness = true,
                AgentType::Refinery => entry.has_refinery = true,
                AgentType::Polecat => entry.polecat_count += 1,
                AgentType::Crew => entry.crew_count += 1,
                _ => {}
            }

            // Save agent name for crew/polecat display
            if agent_name.is_some()
                && (agent_type == AgentType::Crew || agent_type == AgentType::Polecat)
            {
                // Already counted above
            }
        }
    }

    // Calculate status and convert to vec
    let mut rigs: Vec<RigInfo> = rig_status
        .into_values()
        .map(|mut info| {
            info.status = if info.has_witness && info.has_refinery {
                RigStatus::Active
            } else if info.has_witness || info.has_refinery {
                RigStatus::Partial
            } else {
                RigStatus::Inactive
            };
            info
        })
        .collect();

    rigs.sort_by(|a, b| a.name.cmp(&b.name));
    rigs
}

/// Get list of tmux sessions
fn get_tmux_sessions() -> Option<Vec<String>> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(
        stdout
            .lines()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
    )
}

/// Parse a tmux session name to extract rig and agent info
///
/// Session naming:
/// - Town-level: hq-mayor, hq-deacon
/// - Rig-level: gt-<rig>-witness, gt-<rig>-refinery
/// - Workers: gt-<rig>-crew-<name>, gt-<rig>-<polecat-name>
fn parse_session_name(name: &str) -> Option<(String, AgentType, Option<String>)> {
    // Town-level agents
    if name.starts_with("hq-") {
        let suffix = name.strip_prefix("hq-")?;
        match suffix {
            "mayor" => return Some(("hq".to_string(), AgentType::Mayor, None)),
            "deacon" => return Some(("hq".to_string(), AgentType::Deacon, None)),
            _ => return None,
        }
    }

    // Rig-level agents
    if !name.starts_with("gt-") {
        return None;
    }

    let suffix = name.strip_prefix("gt-")?;

    // Legacy witness format: gt-witness-<rig>
    if suffix.starts_with("witness-") {
        let rig = suffix.strip_prefix("witness-")?;
        return Some((rig.to_string(), AgentType::Witness, None));
    }

    // Standard format: gt-<rig>-<type> or gt-<rig>-crew-<name>
    let parts: Vec<&str> = suffix.splitn(2, '-').collect();
    if parts.len() < 2 {
        return None;
    }

    let rig = parts[0];
    let remainder = parts[1];

    // Crew: gt-<rig>-crew-<name>
    if remainder.starts_with("crew-") {
        let name = remainder.strip_prefix("crew-")?;
        return Some((rig.to_string(), AgentType::Crew, Some(name.to_string())));
    }

    // Known roles
    match remainder {
        "witness" => Some((rig.to_string(), AgentType::Witness, None)),
        "refinery" => Some((rig.to_string(), AgentType::Refinery, None)),
        _ => {
            // Everything else is a polecat
            Some((
                rig.to_string(),
                AgentType::Polecat,
                Some(remainder.to_string()),
            ))
        }
    }
}

/// Get refinery queue status (stub - would need to query refinery state)
fn get_refinery_queue(_town_root: &Path, _rig: &str) -> Option<RefineryQueue> {
    // Refinery state would need to be queried from refinery's MQ
    // This is complex and requires accessing refinery's internal state files
    // For now, return None - can be enhanced later
    None
}

/// Get complete Gas Town information for the current project
///
/// Returns None if:
/// - Not in a Gas Town workspace
/// - No agent identity can be determined
pub fn get_gastown_info(project_dir: &Path) -> Option<GasTownInfo> {
    let town_root = find_town_root(project_dir)?;

    // Load town name from config
    let town_name = load_town_name(&town_root);

    // Get agent identity from environment
    let agent = get_agent_identity_from_env();

    // Get hooked issue
    let hooked_issue = get_hooked_issue();

    // Get mail preview if we have an identity
    let mail = agent
        .as_ref()
        .and_then(|a| query_mail_inbox(&town_root, &a.identity));

    // Get rig status (useful for mayor/deacon views)
    let rigs = if agent
        .as_ref()
        .is_some_and(|a| matches!(a.agent_type, AgentType::Mayor | AgentType::Deacon))
    {
        get_rig_status()
    } else {
        Vec::new()
    };

    // Calculate total polecats
    let total_polecats = if !rigs.is_empty() {
        Some(rigs.iter().map(|r| r.polecat_count).sum())
    } else {
        None
    };

    // Get refinery queue if we're a refinery
    let refinery_queue = if let Some(ref a) = agent {
        if a.agent_type == AgentType::Refinery {
            a.rig
                .as_ref()
                .and_then(|r| get_refinery_queue(&town_root, r))
        } else {
            None
        }
    } else {
        None
    };

    Some(GasTownInfo {
        town_root: town_root.to_string_lossy().to_string(),
        town_name,
        agent,
        mail,
        hooked_issue,
        rigs,
        total_polecats,
        refinery_queue,
    })
}

/// Format Gas Town status for display in statusline header
///
/// Returns a compact status string based on agent role:
/// - Mayor: "3 😺 🟢rig1 🟡rig2"
/// - Witness: "2 😺 1 crew"
/// - Polecat/Crew: "😺 <hooked work or mail>"
pub fn format_gastown_display(info: &GasTownInfo, max_len: usize) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(ref agent) = info.agent {
        // Add agent icon
        parts.push(agent.agent_type.emoji().to_string());

        match agent.agent_type {
            AgentType::Mayor | AgentType::Deacon => {
                // Show polecat count and rig LEDs
                if let Some(total) = info.total_polecats {
                    parts.push(format!("{} 😺", total));
                }
                // Rig LEDs
                let rig_leds: Vec<String> = info
                    .rigs
                    .iter()
                    .map(|r| format!("{}{}", r.status.led(), r.name))
                    .collect();
                if !rig_leds.is_empty() {
                    parts.push(rig_leds.join(" "));
                }
            }
            AgentType::Witness => {
                // Show polecat/crew counts for this rig
                if let Some(ref rig_name) = agent.rig {
                    if let Some(rig) = info.rigs.iter().find(|r| &r.name == rig_name) {
                        parts.push(format!("{} 😺", rig.polecat_count));
                        if rig.crew_count > 0 {
                            parts.push(format!("{} crew", rig.crew_count));
                        }
                    }
                }
            }
            AgentType::Refinery => {
                // Show merge queue status
                if let Some(ref queue) = info.refinery_queue {
                    if let Some(ref current) = queue.current {
                        parts.push(format!("merging {}", current));
                        if queue.pending > 0 {
                            parts.push(format!("+{} queued", queue.pending));
                        }
                    } else if queue.pending > 0 {
                        parts.push(format!("{} queued", queue.pending));
                    } else {
                        parts.push("idle".to_string());
                    }
                }
            }
            AgentType::Crew | AgentType::Polecat => {
                // Show hooked work or mail preview
                if let Some(ref issue) = info.hooked_issue {
                    parts.push(format!("🪝 {}", issue));
                } else if let Some(ref mail) = info.mail {
                    if mail.unread_count > 0 {
                        if let Some(ref preview) = mail.preview {
                            parts.push(format!("📬 {}", preview));
                        } else {
                            parts.push(format!("📬 {}", mail.unread_count));
                        }
                    }
                }
            }
        }
    }

    let result = parts.join(" | ");
    if result.chars().count() > max_len {
        format!("{}…", result.chars().take(max_len - 1).collect::<String>())
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_session_name_polecat() {
        let (rig, agent_type, name) = parse_session_name("gt-gastown-slit").unwrap();
        assert_eq!(rig, "gastown");
        assert_eq!(agent_type, AgentType::Polecat);
        assert_eq!(name, Some("slit".to_string()));
    }

    #[test]
    fn test_parse_session_name_witness() {
        let (rig, agent_type, name) = parse_session_name("gt-gastown-witness").unwrap();
        assert_eq!(rig, "gastown");
        assert_eq!(agent_type, AgentType::Witness);
        assert_eq!(name, None);
    }

    #[test]
    fn test_parse_session_name_crew() {
        let (rig, agent_type, name) = parse_session_name("gt-gastown-crew-max").unwrap();
        assert_eq!(rig, "gastown");
        assert_eq!(agent_type, AgentType::Crew);
        assert_eq!(name, Some("max".to_string()));
    }

    #[test]
    fn test_parse_session_name_mayor() {
        let (rig, agent_type, name) = parse_session_name("hq-mayor").unwrap();
        assert_eq!(rig, "hq");
        assert_eq!(agent_type, AgentType::Mayor);
        assert_eq!(name, None);
    }

    #[test]
    fn test_parse_session_name_legacy_witness() {
        let (rig, agent_type, name) = parse_session_name("gt-witness-gastown").unwrap();
        assert_eq!(rig, "gastown");
        assert_eq!(agent_type, AgentType::Witness);
        assert_eq!(name, None);
    }

    #[test]
    fn test_agent_type_emoji() {
        assert_eq!(AgentType::Mayor.emoji(), "🎩");
        assert_eq!(AgentType::Polecat.emoji(), "😺");
        assert_eq!(AgentType::Witness.emoji(), "🦉");
    }

    #[test]
    fn test_rig_status_led() {
        assert_eq!(RigStatus::Active.led(), "🟢");
        assert_eq!(RigStatus::Partial.led(), "🟡");
        assert_eq!(RigStatus::Inactive.led(), "⚫");
    }
}
