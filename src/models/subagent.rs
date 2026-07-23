use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::Entry;

/// Tolerant view of an `agent-<id>.meta.json` sidecar that Claude Code writes
/// next to each subagent transcript. Unknown keys are ignored; every field is
/// optional except `agent_type`, whose absence marks the file as not a usable
/// sidecar.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentMeta {
    pub agent_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub spawn_depth: Option<i64>,
    #[serde(default)]
    pub parent_agent_id: Option<String>,
}

/// A parsed sidecar plus the `wf_<runId>` directory segment it was found under,
/// present only for agents nested inside `subagents/workflows/wf_*/`.
#[derive(Debug, Clone)]
struct SubagentSidecar {
    meta: SubagentMeta,
    workflow_run_id: Option<String>,
}

/// One row of the JSON `session.subagents` array. The base cost/token fields are
/// always present; enrichment fields are omitted (never null) when no sidecar
/// backs the agent.
#[derive(Serialize)]
struct SubagentBreakdownRow {
    agent_id: String,
    cost_usd: f64,
    input_tokens: u64,
    output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spawn_depth: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_run_id: Option<String>,
}

/// Derive the `subagents/` directory for the session whose main transcript is at
/// `transcript_path`. The transcript lives at `<project>/<session>.jsonl`, and its
/// subagent sidecars sit under `<project>/<session>/subagents/`.
fn session_subagents_dir(transcript_path: &str) -> Option<PathBuf> {
    let path = Path::new(transcript_path);
    let parent = path.parent()?;
    let stem = path.file_stem()?;
    Some(parent.join(stem).join("subagents"))
}

/// Extract `<id>` from an `agent-<id>.meta.json` file name.
fn sidecar_agent_id(file_name: &str) -> Option<&str> {
    file_name
        .strip_prefix("agent-")
        .and_then(|rest| rest.strip_suffix(".meta.json"))
}

/// Scan a session's `subagents/` directory (and one level of `workflows/wf_*/`
/// nesting) for `agent-<id>.meta.json` sidecars whose id is in `agent_ids`. Files
/// that are missing, unreadable, or fail to parse are skipped so callers degrade
/// to the un-enriched breakdown.
fn load_session_sidecars(
    subagents_dir: &Path,
    agent_ids: &HashSet<&str>,
) -> HashMap<String, SubagentSidecar> {
    let mut out = HashMap::new();
    if agent_ids.is_empty() {
        return out;
    }
    // Depth-1 sidecars sit directly in subagents/.
    scan_sidecar_dir(subagents_dir, None, agent_ids, &mut out);
    // Workflow-run agents nest one level deeper under workflows/wf_<runId>/.
    let workflows_dir = subagents_dir.join("workflows");
    if let Ok(entries) = fs::read_dir(&workflows_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("wf_") || !path.is_dir() {
                continue;
            }
            let run_id = name.to_string();
            scan_sidecar_dir(&path, Some(&run_id), agent_ids, &mut out);
        }
    }
    out
}

fn scan_sidecar_dir(
    dir: &Path,
    workflow_run_id: Option<&str>,
    agent_ids: &HashSet<&str>,
    out: &mut HashMap<String, SubagentSidecar>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(id) = sidecar_agent_id(file_name) else {
            continue;
        };
        if !agent_ids.contains(id) || out.contains_key(id) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<SubagentMeta>(&text) else {
            continue;
        };
        out.insert(
            id.to_string(),
            SubagentSidecar {
                meta,
                workflow_run_id: workflow_run_id.map(str::to_string),
            },
        );
    }
}

/// Build the per-subagent cost breakdown for the JSON `session.subagents` array,
/// enriched with sidecar metadata when available. Returns `None` when the session
/// has no subagent entries. Enrichment fields are omitted (never null) when the
/// matching sidecar is missing or unreadable, so consumers of the un-enriched
/// shape are unaffected.
pub fn build_subagent_breakdown(
    entries: &[Entry],
    session_id: &str,
    transcript_path: &str,
) -> Option<serde_json::Value> {
    let mut totals: BTreeMap<String, (f64, u64, u64)> = BTreeMap::new();
    for e in entries {
        if e.session_id.as_deref() != Some(session_id) {
            continue;
        }
        if let Some(aid) = e.agent_id.as_deref() {
            let slot = totals.entry(aid.to_string()).or_insert((0.0, 0, 0));
            slot.0 += e.cost;
            slot.1 += e.input + e.cache_create + e.cache_read;
            slot.2 += e.output;
        }
    }
    if totals.is_empty() {
        return None;
    }

    let agent_ids: HashSet<&str> = totals.keys().map(String::as_str).collect();
    let sidecars = session_subagents_dir(transcript_path)
        .map(|dir| load_session_sidecars(&dir, &agent_ids))
        .unwrap_or_default();

    let rows: Vec<SubagentBreakdownRow> = totals
        .iter()
        .map(|(aid, (cost, input, output))| {
            let sidecar = sidecars.get(aid);
            SubagentBreakdownRow {
                agent_id: aid.clone(),
                cost_usd: (cost * 10000.0).round() / 10000.0,
                input_tokens: *input,
                output_tokens: *output,
                agent_type: sidecar.map(|s| s.meta.agent_type.clone()),
                name: sidecar.and_then(|s| s.meta.name.clone()),
                model: sidecar.and_then(|s| s.meta.model.clone()),
                description: sidecar.and_then(|s| s.meta.description.clone()),
                spawn_depth: sidecar.and_then(|s| s.meta.spawn_depth),
                parent_agent_id: sidecar.and_then(|s| s.meta.parent_agent_id.clone()),
                workflow_run_id: sidecar.and_then(|s| s.workflow_run_id.clone()),
            }
        })
        .collect();

    serde_json::to_value(rows).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(session_id: &str, agent_id: &str, cost: f64, input: u64, output: u64) -> Entry {
        Entry {
            ts: Utc::now(),
            input,
            output,
            cache_create: 0,
            cache_read: 0,
            web_search_requests: 0,
            speed: None,
            service_tier: None,
            cost,
            model: None,
            session_id: Some(session_id.to_string()),
            msg_id: None,
            req_id: None,
            project: None,
            agent_id: Some(agent_id.to_string()),
        }
    }

    #[test]
    fn sidecar_parses_workflow_and_tooluse_shapes() {
        let workflow =
            r#"{"agentType":"workflow-subagent","spawnDepth":2,"model":"claude-sonnet-4-6"}"#;
        let meta: SubagentMeta = serde_json::from_str(workflow).unwrap();
        assert_eq!(meta.agent_type, "workflow-subagent");
        assert_eq!(meta.spawn_depth, Some(2));
        assert_eq!(meta.model.as_deref(), Some("claude-sonnet-4-6"));
        assert!(meta.name.is_none());
        assert!(meta.parent_agent_id.is_none());
        assert!(meta.description.is_none());

        let tool_use = r#"{"agentType":"code-reviewer","description":"review the diff","toolUseId":"tu_abc","name":"reviewer","parentAgentId":"root-agent","spawnDepth":1,"worktreePath":"/tmp/wt","isFork":false,"color":"blue","permissionMode":"default"}"#;
        let meta: SubagentMeta = serde_json::from_str(tool_use).unwrap();
        assert_eq!(meta.agent_type, "code-reviewer");
        assert_eq!(meta.name.as_deref(), Some("reviewer"));
        assert_eq!(meta.description.as_deref(), Some("review the diff"));
        assert_eq!(meta.parent_agent_id.as_deref(), Some("root-agent"));
        assert_eq!(meta.spawn_depth, Some(1));

        // agent_type is the only required field; its absence rejects the file.
        assert!(serde_json::from_str::<SubagentMeta>(r#"{"name":"x"}"#).is_err());
    }

    #[test]
    fn build_subagent_breakdown_enriches_depth1_and_workflow_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let session_id = "sess1";
        let transcript_path = tmp.path().join(format!("{session_id}.jsonl"));

        let subagents_dir = tmp.path().join(session_id).join("subagents");
        fs::create_dir_all(&subagents_dir).unwrap();
        fs::write(
            subagents_dir.join("agent-aaa111.meta.json"),
            r#"{"agentType":"code-reviewer","name":"reviewer","model":"claude-opus-4-6","description":"review the diff","spawnDepth":1,"parentAgentId":"root-agent","toolUseId":"tu_1"}"#,
        )
        .unwrap();

        let wf_dir = subagents_dir.join("workflows").join("wf_run42");
        fs::create_dir_all(&wf_dir).unwrap();
        fs::write(
            wf_dir.join("agent-bbb222.meta.json"),
            r#"{"agentType":"workflow-subagent","spawnDepth":2,"model":"claude-sonnet-4-6"}"#,
        )
        .unwrap();

        let entries = vec![
            entry(session_id, "aaa111", 0.12, 5000, 200),
            entry(session_id, "bbb222", 0.03, 1000, 50),
        ];

        let value =
            build_subagent_breakdown(&entries, session_id, transcript_path.to_str().unwrap())
                .expect("session has subagents");
        let arr = value.as_array().expect("array");
        assert_eq!(arr.len(), 2);

        // BTreeMap ordering keeps the array sorted by agent id.
        let depth1 = &arr[0];
        assert_eq!(depth1["agent_id"], "aaa111");
        assert_eq!(depth1["cost_usd"], 0.12);
        assert_eq!(depth1["input_tokens"], 5000);
        assert_eq!(depth1["output_tokens"], 200);
        assert_eq!(depth1["agent_type"], "code-reviewer");
        assert_eq!(depth1["name"], "reviewer");
        assert_eq!(depth1["model"], "claude-opus-4-6");
        assert_eq!(depth1["description"], "review the diff");
        assert_eq!(depth1["spawn_depth"], 1);
        assert_eq!(depth1["parent_agent_id"], "root-agent");
        // A depth-1 agent carries no workflow run id.
        assert!(depth1.get("workflow_run_id").is_none());

        let workflow = &arr[1];
        assert_eq!(workflow["agent_id"], "bbb222");
        assert_eq!(workflow["agent_type"], "workflow-subagent");
        assert_eq!(workflow["spawn_depth"], 2);
        assert_eq!(workflow["model"], "claude-sonnet-4-6");
        assert_eq!(workflow["workflow_run_id"], "wf_run42");
        // Absent sidecar fields are omitted rather than serialized as null.
        assert!(workflow.get("name").is_none());
        assert!(workflow.get("description").is_none());
        assert!(workflow.get("parent_agent_id").is_none());
    }

    #[test]
    fn build_subagent_breakdown_degrades_without_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let session_id = "sess2";
        let transcript_path = tmp.path().join(format!("{session_id}.jsonl"));
        // No subagents directory is created, so enrichment must be skipped.
        let entries = vec![entry(session_id, "ccc333", 0.5, 100, 10)];

        let value =
            build_subagent_breakdown(&entries, session_id, transcript_path.to_str().unwrap())
                .expect("session has subagents");
        let row = &value.as_array().unwrap()[0];
        assert_eq!(row["agent_id"], "ccc333");
        assert_eq!(row["cost_usd"], 0.5);
        assert!(row.get("agent_type").is_none());
        assert!(row.get("workflow_run_id").is_none());
    }

    #[test]
    fn build_subagent_breakdown_returns_none_without_subagent_entries() {
        let entries = vec![Entry {
            agent_id: None,
            ..entry("sess3", "ignored", 1.0, 1, 1)
        }];
        assert!(build_subagent_breakdown(&entries, "sess3", "/tmp/sess3.jsonl").is_none());
    }
}
