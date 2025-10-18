//! cc-sessions integration
//!
//! Detects and parses cc-sessions state when present in a project.
//! Provides task tracking, mode detection, and enhanced git information.

use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::models::{SessionsInfo, SessionsMode, SessionsState, UpstreamInfo};

/// Detect if cc-sessions is active in the project
pub fn detect_sessions(project_dir: Option<&Path>) -> Option<PathBuf> {
    // 1. Check CLAUDE_PROJECT_DIR env + sessions/sessions-state.json
    if let Ok(env_project_dir) = env::var("CLAUDE_PROJECT_DIR") {
        let state_file = PathBuf::from(env_project_dir).join("sessions/sessions-state.json");
        if state_file.exists() {
            return Some(state_file);
        }
    }

    // 2. Check provided project_dir + sessions/sessions-state.json
    if let Some(proj_dir) = project_dir {
        let state_file = proj_dir.join("sessions/sessions-state.json");
        if state_file.exists() {
            return Some(state_file);
        }
    }

    // 3. Walk up from current directory looking for sessions/sessions-state.json
    let mut current = env::current_dir().ok()?;
    loop {
        let state_file = current.join("sessions/sessions-state.json");
        if state_file.exists() {
            return Some(state_file);
        }

        let parent = current.parent()?;
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }

    None
}

/// Parse sessions state file
pub fn parse_sessions_state(state_file: &Path) -> Result<SessionsState> {
    let contents = fs::read_to_string(state_file)
        .with_context(|| format!("Failed to read sessions state: {}", state_file.display()))?;

    let state: SessionsState = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse sessions state: {}", state_file.display()))?;

    Ok(state)
}

/// Count open tasks in sessions/tasks directory
pub fn count_open_tasks(project_dir: &Path) -> u32 {
    let tasks_dir = project_dir.join("sessions/tasks");
    if !tasks_dir.is_dir() {
        return 0;
    }

    let mut count = 0;

    if let Ok(entries) = fs::read_dir(&tasks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();

            if path.is_file() {
                // Count .md files except TEMPLATE.md
                if file_name_str.ends_with(".md") && file_name_str != "TEMPLATE.md" {
                    count += 1;
                }
            } else if path.is_dir() {
                // Count directories except done/ and indexes/
                if file_name_str != "done" && file_name_str != "indexes" {
                    count += 1;
                }
            }
        }
    }

    count
}

/// Count edited files in git repository (unstaged + staged)
pub fn count_edited_files(repo_dir: &Path) -> u32 {
    let mut count = 0;

    // Count unstaged changes
    if let Ok(output) = std::process::Command::new("git")
        .args(["-C", &repo_dir.to_string_lossy(), "diff", "--name-only"])
        .output()
    {
        if output.status.success() {
            let unstaged = String::from_utf8_lossy(&output.stdout);
            count += unstaged.lines().filter(|l| !l.trim().is_empty()).count();
        }
    }

    // Count staged changes
    if let Ok(output) = std::process::Command::new("git")
        .args([
            "-C",
            &repo_dir.to_string_lossy(),
            "diff",
            "--cached",
            "--name-only",
        ])
        .output()
    {
        if output.status.success() {
            let staged = String::from_utf8_lossy(&output.stdout);
            count += staged.lines().filter(|l| !l.trim().is_empty()).count();
        }
    }

    count as u32
}

/// Get git upstream tracking info (ahead/behind)
pub fn get_upstream_info(repo_dir: &Path) -> Option<UpstreamInfo> {
    // Get ahead count
    let ahead = std::process::Command::new("git")
        .args([
            "-C",
            &repo_dir.to_string_lossy(),
            "rev-list",
            "--count",
            "@{u}..HEAD",
        ])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .parse::<u32>()
                    .ok()
            } else {
                None
            }
        })?;

    // Get behind count
    let behind = std::process::Command::new("git")
        .args([
            "-C",
            &repo_dir.to_string_lossy(),
            "rev-list",
            "--count",
            "HEAD..@{u}",
        ])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .parse::<u32>()
                    .ok()
            } else {
                None
            }
        })?;

    Some(UpstreamInfo { ahead, behind })
}

/// Build complete sessions info
pub fn gather_sessions_info(project_dir: Option<&Path>) -> Option<SessionsInfo> {
    let state_file = detect_sessions(project_dir)?;
    let project_root = state_file.parent()?.parent()?;

    let state = parse_sessions_state(&state_file).ok()?;

    let current_task = state.current_task.as_ref().and_then(|t| t.name.clone());

    let mode = state.mode.map(|m| match m {
        SessionsMode::Discussion => "Discussion".to_string(),
        SessionsMode::Implementation => "Implementation".to_string(),
    });

    let open_tasks = count_open_tasks(project_root);
    let edited_files = count_edited_files(project_root);
    let upstream = get_upstream_info(project_root);

    Some(SessionsInfo {
        detected: true,
        current_task,
        mode,
        open_tasks,
        edited_files,
        upstream,
    })
}
