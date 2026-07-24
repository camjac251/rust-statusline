//! Workflow run-state and remote-agent task data structures.
//!
//! Claude Code writes a `wf_<runId>.json` run-state snapshot into
//! `<project>/<session>/workflows/` for each workflow run, and a
//! `remote-agent-<taskId>.meta.json` sidecar into
//! `<project>/<session>/remote-agents/` for each active remote task. Only the
//! fields the statusline renders are modeled here; unknown keys are ignored so
//! the structures tolerate schema growth.

use serde::Deserialize;

/// One entry in a workflow run's `workflowProgress[]`. The array mixes phase
/// markers (`type: "workflow_phase"`) and per-agent records
/// (`type: "workflow_agent"`) under a shared `type` tag. Both are captured with
/// the same tolerant shape so an unknown entry type never fails the parse.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowProgressEntry {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

impl WorkflowProgressEntry {
    /// Whether this entry describes a dispatched agent.
    pub fn is_agent(&self) -> bool {
        self.kind.as_deref() == Some("workflow_agent")
    }

    /// Whether this agent has finished (`state == "done"`).
    pub fn is_done(&self) -> bool {
        self.state.as_deref() == Some("done")
    }
}

/// Statuses that mean a workflow run has finished; a run in one of these is
/// not shown as active work in the status line.
const TERMINAL_STATUSES: &[&str] = &[
    "completed",
    "killed",
    "failed",
    "cancelled",
    "canceled",
    "error",
    "stopped",
];

/// Tolerant view of a `wf_<runId>.json` workflow run-state snapshot.
///
/// Claude Code writes that snapshot only when a run reaches a terminal state, so
/// a run that is still in flight is reconstructed from its journal instead. The
/// `live_agents_*` fields carry that journal-derived progress and are never part
/// of the on-disk snapshot.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkflowRun {
    #[serde(rename = "runId")]
    pub run_id: String,
    #[serde(rename = "workflowName", default)]
    pub workflow_name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(rename = "startTime", default)]
    pub start_time: Option<i64>,
    #[serde(rename = "totalTokens", default)]
    pub total_tokens: Option<u64>,
    #[serde(rename = "agentCount", default)]
    pub agent_count: Option<u64>,
    #[serde(rename = "workflowProgress", default)]
    pub workflow_progress: Vec<WorkflowProgressEntry>,
    #[serde(skip)]
    pub live_agents_done: Option<usize>,
    #[serde(skip)]
    pub live_agents_total: Option<usize>,
}

impl WorkflowRun {
    /// Whether the run has reached a terminal status.
    pub fn is_terminal(&self) -> bool {
        self.status
            .as_deref()
            .is_some_and(|s| TERMINAL_STATUSES.contains(&s))
    }

    /// Whether the run is still in flight (not terminal).
    pub fn is_running(&self) -> bool {
        !self.is_terminal()
    }

    /// Number of dispatched agents that have finished. Journal-derived progress
    /// (for a live run) takes precedence over the snapshot's progress records.
    pub fn agents_done(&self) -> usize {
        if let Some(done) = self.live_agents_done {
            return done;
        }
        self.workflow_progress
            .iter()
            .filter(|e| e.is_agent() && e.is_done())
            .count()
    }

    /// Total agents in the run. For a live run this is the count dispatched so
    /// far (the final total is only known once the run finishes); otherwise the
    /// declared `agentCount` when present and nonzero, falling back to the
    /// dispatched agents seen in the progress records.
    pub fn agents_total(&self) -> usize {
        if let Some(total) = self.live_agents_total {
            return total;
        }
        match self.agent_count {
            Some(count) if count > 0 => count as usize,
            _ => self
                .workflow_progress
                .iter()
                .filter(|e| e.is_agent())
                .count(),
        }
    }
}

/// Tolerant view of a `remote-agent-<taskId>.meta.json` sidecar describing an
/// active remote-agent task. No transcript exists locally for these tasks.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTask {
    pub task_id: String,
    #[serde(default)]
    pub remote_task_type: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Workflow runs and remote-agent tasks discovered for the current hook session.
#[derive(Debug, Clone, Default)]
pub struct SessionActivity {
    /// Workflow runs, most recent first by start time.
    pub workflows: Vec<WorkflowRun>,
    /// Active remote-agent tasks.
    pub remote_tasks: Vec<RemoteTask>,
}

impl SessionActivity {
    /// Whether the session has neither workflow runs nor remote tasks.
    pub fn is_empty(&self) -> bool {
        self.workflows.is_empty() && self.remote_tasks.is_empty()
    }

    /// The most recent workflow run, if any.
    pub fn latest_workflow(&self) -> Option<&WorkflowRun> {
        self.workflows.first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_run_parses_and_counts_agents() {
        let json = r#"{
            "runId": "run42",
            "workflowName": "review",
            "status": "running",
            "startTime": 1717000000000,
            "totalTokens": 12345,
            "agentCount": 6,
            "durationMs": 1000,
            "workflowProgress": [
                {"type": "workflow_phase", "index": 0, "title": "scan"},
                {"type": "workflow_agent", "label": "a", "state": "done"},
                {"type": "workflow_agent", "label": "b", "state": "done"},
                {"type": "workflow_agent", "label": "c", "state": "done"},
                {"type": "workflow_agent", "label": "d", "state": "running"}
            ]
        }"#;
        let run: WorkflowRun = serde_json::from_str(json).unwrap();
        assert_eq!(run.run_id, "run42");
        assert_eq!(run.workflow_name.as_deref(), Some("review"));
        assert_eq!(run.total_tokens, Some(12345));
        assert!(run.is_running());
        assert_eq!(run.agents_done(), 3);
        assert_eq!(run.agents_total(), 6);
    }

    #[test]
    fn workflow_run_terminal_status_is_not_running() {
        let json = r#"{"runId": "r1", "status": "completed", "workflowProgress": []}"#;
        let run: WorkflowRun = serde_json::from_str(json).unwrap();
        assert!(run.is_terminal());
        assert!(!run.is_running());

        let killed = r#"{"runId": "r2", "status": "killed", "error": "boom"}"#;
        let run: WorkflowRun = serde_json::from_str(killed).unwrap();
        assert!(run.is_terminal());
    }

    #[test]
    fn workflow_agents_total_falls_back_to_progress_records() {
        let json = r#"{
            "runId": "r1",
            "status": "running",
            "workflowProgress": [
                {"type": "workflow_agent", "state": "done"},
                {"type": "workflow_agent", "state": "running"}
            ]
        }"#;
        let run: WorkflowRun = serde_json::from_str(json).unwrap();
        assert_eq!(run.agents_total(), 2);
        assert_eq!(run.agents_done(), 1);
    }

    #[test]
    fn remote_task_parses_required_and_optional_fields() {
        let json = r#"{
            "taskId": "task-1",
            "remoteTaskType": "cloud",
            "sessionId": "session_01abc",
            "title": "build the thing",
            "command": "claude",
            "spawnedAt": 1717000000000,
            "flags": {}
        }"#;
        let task: RemoteTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.task_id, "task-1");
        assert_eq!(task.remote_task_type.as_deref(), Some("cloud"));
        assert_eq!(task.title.as_deref(), Some("build the thing"));

        // task_id is the only required field; its absence rejects the sidecar.
        assert!(serde_json::from_str::<RemoteTask>(r#"{"title": "x"}"#).is_err());
    }
}
