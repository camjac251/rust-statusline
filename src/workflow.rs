//! Discovery of workflow runs and remote-agent tasks for the current session.
//!
//! Claude Code stores per-session orchestration state beside the main
//! transcript: workflow run-state snapshots in `<project>/<session>/workflows/`
//! and remote-agent task sidecars in `<project>/<session>/remote-agents/`. This
//! module reads only the current hook session's directory and tolerates missing
//! directories, unreadable files, and parse failures by skipping them.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::models::{RemoteTask, SessionActivity, WorkflowRun};

const WORKFLOWS_DIR: &str = "workflows";
const REMOTE_AGENTS_DIR: &str = "remote-agents";
const SUBAGENTS_DIR: &str = "subagents";
const SCRIPTS_DIR: &str = "scripts";
const JOURNAL_FILE: &str = "journal.jsonl";
/// A live run whose journal has not been touched within this window is treated
/// as abandoned (e.g. a hard crash that skipped terminal-state writing) and is
/// no longer shown as running.
const LIVE_WORKFLOW_MAX_IDLE: Duration = Duration::from_secs(1800);

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
    get_session_activity_at(transcript_path, SystemTime::now())
}

fn get_session_activity_at(transcript_path: &Path, now: SystemTime) -> Option<SessionActivity> {
    let dir = session_dir(transcript_path)?;
    // Live (in-flight) runs first so the running-workflow segment picks one over
    // any finished snapshot; terminal snapshots follow, already sorted by start.
    let mut workflows = load_live_workflow_runs(&dir, now);
    workflows.extend(load_workflow_runs(&dir.join(WORKFLOWS_DIR)));
    let activity = SessionActivity {
        workflows,
        remote_tasks: load_remote_tasks(&dir.join(REMOTE_AGENTS_DIR)),
    };
    (!activity.is_empty()).then_some(activity)
}

/// Reconstruct workflow runs that are still in flight. Claude Code writes the
/// terminal `workflows/wf_<runId>.json` snapshot only once a run finishes, so a
/// run is live when its journal directory `subagents/workflows/<runId>/` exists
/// with a recently-touched journal and no terminal snapshot yet. Progress is
/// read from the journal's `started`/`result` lines.
fn load_live_workflow_runs(session_dir: &Path, now: SystemTime) -> Vec<WorkflowRun> {
    let mut runs = Vec::new();
    let live_dir = session_dir.join(SUBAGENTS_DIR).join(WORKFLOWS_DIR);
    let Ok(entries) = fs::read_dir(&live_dir) else {
        return runs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(run_id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !run_id.starts_with("wf_") || !path.is_dir() {
            continue;
        }
        // A terminal snapshot means the run has already finished.
        if session_dir
            .join(WORKFLOWS_DIR)
            .join(format!("{run_id}.json"))
            .exists()
        {
            continue;
        }
        let journal = path.join(JOURNAL_FILE);
        let Ok(meta) = fs::metadata(&journal) else {
            continue;
        };
        if let Ok(modified) = meta.modified()
            && now
                .duration_since(modified)
                .is_ok_and(|idle| idle > LIVE_WORKFLOW_MAX_IDLE)
        {
            continue;
        }
        let (done, dispatched) = count_journal_progress(&journal);
        runs.push(WorkflowRun {
            run_id: run_id.to_string(),
            workflow_name: workflow_name_from_script(session_dir, run_id),
            status: Some("running".to_string()),
            live_agents_done: Some(done),
            live_agents_total: Some(dispatched),
            ..Default::default()
        });
    }
    runs
}

/// Count finished (`result`) and dispatched (`started`) agents in a workflow
/// journal. Lines that are blank or not JSON objects are ignored.
fn count_journal_progress(journal: &Path) -> (usize, usize) {
    let Ok(text) = fs::read_to_string(journal) else {
        return (0, 0);
    };
    let mut done = 0;
    let mut dispatched = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("result") => done += 1,
            Some("started") => dispatched += 1,
            _ => {}
        }
    }
    (done, dispatched)
}

/// Recover a run's workflow name from its launch-time script file, written as
/// `workflows/scripts/<name>-<runId>.js` before the run starts.
fn workflow_name_from_script(session_dir: &Path, run_id: &str) -> Option<String> {
    let scripts = session_dir.join(WORKFLOWS_DIR).join(SCRIPTS_DIR);
    let suffix = format!("-{run_id}.js");
    for entry in fs::read_dir(&scripts).ok()?.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && let Some(stem) = name.strip_suffix(&suffix)
            && !stem.is_empty()
        {
            return Some(stem.to_string());
        }
    }
    None
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

    fn seed(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn detects_live_workflow_from_journal_without_terminal_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let session_id = "sess-live";
        let transcript = tmp.path().join(format!("{session_id}.jsonl"));
        let base = tmp.path().join(session_id);
        seed(
            &base.join("subagents/workflows/wf_run/journal.jsonl"),
            "{\"type\":\"started\",\"agentId\":\"a1\"}\n{\"type\":\"started\",\"agentId\":\"a2\"}\n{\"type\":\"result\",\"agentId\":\"a1\"}\n",
        );
        seed(
            &base.join("workflows/scripts/my-flow-wf_run.js"),
            "export const meta = {}\n",
        );

        let activity =
            get_session_activity_at(&transcript, SystemTime::now()).expect("live run detected");
        assert_eq!(activity.workflows.len(), 1);
        let run = &activity.workflows[0];
        assert_eq!(run.run_id, "wf_run");
        assert_eq!(run.workflow_name.as_deref(), Some("my-flow"));
        assert!(run.is_running());
        assert_eq!(run.agents_done(), 1);
        assert_eq!(run.agents_total(), 2);
    }

    #[test]
    fn terminal_snapshot_supersedes_live_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let session_id = "sess-term";
        let transcript = tmp.path().join(format!("{session_id}.jsonl"));
        let base = tmp.path().join(session_id);
        seed(
            &base.join("subagents/workflows/wf_run/journal.jsonl"),
            "{\"type\":\"result\",\"agentId\":\"a1\"}\n",
        );
        seed(
            &base.join("workflows/wf_run.json"),
            "{\"runId\":\"wf_run\",\"status\":\"completed\",\"agentCount\":1,\"workflowProgress\":[{\"type\":\"workflow_agent\",\"state\":\"done\"}]}",
        );

        let activity =
            get_session_activity_at(&transcript, SystemTime::now()).expect("terminal run present");
        // The finished journal is not surfaced as a second, running run.
        assert_eq!(activity.workflows.len(), 1);
        assert!(!activity.workflows[0].is_running());
    }

    #[test]
    fn live_run_sorts_ahead_of_finished_run() {
        let tmp = tempfile::tempdir().unwrap();
        let session_id = "sess-mix";
        let transcript = tmp.path().join(format!("{session_id}.jsonl"));
        let base = tmp.path().join(session_id);
        seed(
            &base.join("subagents/workflows/wf_old/journal.jsonl"),
            "{\"type\":\"result\",\"agentId\":\"a\"}\n",
        );
        seed(
            &base.join("workflows/wf_old.json"),
            "{\"runId\":\"wf_old\",\"status\":\"completed\",\"startTime\":1000,\"workflowProgress\":[]}",
        );
        seed(
            &base.join("subagents/workflows/wf_new/journal.jsonl"),
            "{\"type\":\"started\",\"agentId\":\"a\"}\n",
        );

        let activity =
            get_session_activity_at(&transcript, SystemTime::now()).expect("has activity");
        let running = activity
            .workflows
            .iter()
            .find(|r| r.is_running())
            .expect("a running run");
        assert_eq!(running.run_id, "wf_new");
        assert_eq!(activity.latest_workflow().unwrap().run_id, "wf_new");
    }

    #[test]
    fn abandoned_live_run_with_cold_journal_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let session_id = "sess-cold";
        let transcript = tmp.path().join(format!("{session_id}.jsonl"));
        let base = tmp.path().join(session_id);
        seed(
            &base.join("subagents/workflows/wf_cold/journal.jsonl"),
            "{\"type\":\"started\",\"agentId\":\"a\"}\n",
        );
        // Evaluate far enough in the future that the journal reads as abandoned.
        let future = SystemTime::now() + Duration::from_secs(7200);
        assert!(get_session_activity_at(&transcript, future).is_none());
    }
}
