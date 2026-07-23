//! Discovery of workflow runs and remote-agent tasks for the current session.
//!
//! Claude Code stores per-session orchestration state beside the main
//! transcript: workflow run-state snapshots in `<project>/<session>/workflows/`
//! and remote-agent task sidecars in `<project>/<session>/remote-agents/`. This
//! module reads only the current hook session's directory and tolerates missing
//! directories, unreadable files, and parse failures by skipping them.

use std::fs;
use std::path::{Path, PathBuf};

use crate::models::{RemoteTask, SessionActivity, WorkflowRun};

const WORKFLOWS_DIR: &str = "workflows";
const REMOTE_AGENTS_DIR: &str = "remote-agents";

/// Derive the `<project>/<session>/` directory from the main transcript path,
/// which sits at `<project>/<session>.jsonl`.
fn session_dir(transcript_path: &Path) -> Option<PathBuf> {
    let parent = transcript_path.parent()?;
    let stem = transcript_path.file_stem()?;
    Some(parent.join(stem))
}

/// Gather workflow runs and remote-agent tasks for the session whose main
/// transcript is at `transcript_path`. Returns `None` when the session has
/// neither, so callers can treat absence uniformly.
pub fn get_session_activity(transcript_path: &Path) -> Option<SessionActivity> {
    let dir = session_dir(transcript_path)?;
    let activity = SessionActivity {
        workflows: load_workflow_runs(&dir.join(WORKFLOWS_DIR)),
        remote_tasks: load_remote_tasks(&dir.join(REMOTE_AGENTS_DIR)),
    };
    (!activity.is_empty()).then_some(activity)
}

/// Read every `wf_<runId>.json` run-state snapshot in `dir`, sorted most recent
/// first by start time. Missing, unreadable, or unparseable files are skipped.
fn load_workflow_runs(dir: &Path) -> Vec<WorkflowRun> {
    let mut runs: Vec<WorkflowRun> = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return runs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("wf_") || !name.ends_with(".json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(run) = serde_json::from_str::<WorkflowRun>(&text) {
            runs.push(run);
        }
    }
    // Runs without a start time sort last so any dated run wins.
    runs.sort_by(|a, b| {
        b.start_time
            .unwrap_or(i64::MIN)
            .cmp(&a.start_time.unwrap_or(i64::MIN))
    });
    runs
}

/// Read every `remote-agent-<taskId>.meta.json` sidecar in `dir`. Missing,
/// unreadable, or unparseable files are skipped.
fn load_remote_tasks(dir: &Path) -> Vec<RemoteTask> {
    let mut tasks: Vec<RemoteTask> = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return tasks;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("remote-agent-") || !name.ends_with(".meta.json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(task) = serde_json::from_str::<RemoteTask>(&text) {
            tasks.push(task);
        }
    }
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_latest_workflow_and_remote_tasks_from_session_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let session_id = "sess-c";
        let transcript_path = tmp.path().join(format!("{session_id}.jsonl"));

        let workflows_dir = tmp.path().join(session_id).join("workflows");
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(
            workflows_dir.join("wf_old.json"),
            r#"{"runId":"old","workflowName":"first","status":"completed","startTime":1000,"agentCount":2,"workflowProgress":[]}"#,
        )
        .unwrap();
        fs::write(
            workflows_dir.join("wf_new.json"),
            r#"{"runId":"new","workflowName":"second","status":"running","startTime":2000,"agentCount":4,"totalTokens":900,"workflowProgress":[{"type":"workflow_agent","state":"done"},{"type":"workflow_agent","state":"running"}]}"#,
        )
        .unwrap();
        // An unparseable file is skipped silently.
        fs::write(workflows_dir.join("wf_broken.json"), "not json").unwrap();
        // A non-matching file is ignored.
        fs::write(workflows_dir.join("journal.jsonl"), "{}").unwrap();

        let remote_dir = tmp.path().join(session_id).join("remote-agents");
        fs::create_dir_all(&remote_dir).unwrap();
        fs::write(
            remote_dir.join("remote-agent-task9.meta.json"),
            r#"{"taskId":"task9","remoteTaskType":"cloud","sessionId":"session_01x","title":"remote build"}"#,
        )
        .unwrap();

        let activity = get_session_activity(&transcript_path).expect("session has activity");
        assert_eq!(activity.workflows.len(), 2);
        let latest = activity.latest_workflow().unwrap();
        assert_eq!(latest.run_id, "new");
        assert!(latest.is_running());
        assert_eq!(latest.agents_done(), 1);
        assert_eq!(latest.agents_total(), 4);
        assert_eq!(activity.remote_tasks.len(), 1);
        assert_eq!(activity.remote_tasks[0].task_id, "task9");
    }

    #[test]
    fn returns_none_when_session_has_no_activity() {
        let tmp = tempfile::tempdir().unwrap();
        let transcript_path = tmp.path().join("empty.jsonl");
        assert!(get_session_activity(&transcript_path).is_none());
    }
}
