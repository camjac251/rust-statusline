//! Agent-panel statusline support for Claude Code's `subagentStatusLine` hook.
//!
//! Claude Code sends one JSON payload per tick covering every eligible
//! agent-panel task (`columns` plus a `tasks` array, alongside the session base
//! keys) and consumes stdout as JSONL mapped by task id: one `{"id","content"}`
//! object per task to decorate. A task with no line keeps Claude Code's default
//! row rendering; an empty content string hides the row. Decoration content
//! replaces the whole default row body after the status-bullet gutter, ANSI SGR
//! styling is re-interpreted, and overlong content is truncated, so rendering
//! keeps visible width within the per-row `columns` budget.

use crate::tokens;
use crate::utils::{format_tokens, friendly_model_name};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct SubagentStatusInput {
    pub columns: usize,
    pub tasks: Vec<SubagentStatusTask>,
}

/// Effort as Claude Code sends it: agent definitions admit either the string
/// enum ("low" through "max") or an integer level, so both shapes must parse.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SubagentEffort {
    Label(String),
    Level(i64),
}

/// One agent-panel task row. Deserialization reads the camelCase payload keys;
/// serialization keeps the snake_case field names used by `--json` output.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub struct SubagentStatusTask {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename(deserialize = "type"))]
    pub task_type: String,
    pub status: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub label: String,
    pub start_time: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<SubagentEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_size: Option<u64>,
    #[serde(default)]
    pub token_count: u64,
    #[serde(default)]
    pub token_samples: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SubagentStatusDecoration {
    pub id: String,
    pub content: String,
}

/// Structured `--json` view of the agent-panel payload: the parsed task rows
/// under a top-level `tasks` array, snake_case keys, absent optional fields
/// omitted rather than serialized as null. Each row carries a derived
/// `tokens_per_minute` field when the token samples support one.
pub fn subagent_status_json(input: &SubagentStatusInput) -> serde_json::Value {
    let tasks: Vec<serde_json::Value> = input
        .tasks
        .iter()
        .map(|task| {
            let mut value = serde_json::json!(task);
            if let Some(rate) = tokens_per_minute(&task.token_samples)
                && let Some(object) = value.as_object_mut()
            {
                object.insert(
                    "tokens_per_minute".to_string(),
                    serde_json::json!((rate * 10.0).round() / 10.0),
                );
            }
            value
        })
        .collect();
    serde_json::json!({ "tasks": tasks })
}

/// Claude Code pushes one cumulative token-count sample per agent-panel tick
/// (every five seconds, sixteen most recent kept), so the series carries its
/// own time base: the span is one tick per gap between samples.
const TOKEN_SAMPLE_INTERVAL_SECONDS: f64 = 5.0;

fn tokens_per_minute(samples: &[u64]) -> Option<f64> {
    let (Some(&first), Some(&last)) = (samples.first(), samples.last()) else {
        return None;
    };
    // A single sample has no span, and a flat or shrinking series would only
    // render a noise chip, so both omit the rate.
    if samples.len() < 2 || last <= first {
        return None;
    }
    let span_seconds = (samples.len() - 1) as f64 * TOKEN_SAMPLE_INTERVAL_SECONDS;
    Some((last - first) as f64 * 60.0 / span_seconds)
}

fn burn_label(tokens_per_minute: f64) -> String {
    format!("{}/m", format_tokens(tokens_per_minute.round() as u64))
}

pub fn render_subagent_statusline(
    input: &SubagentStatusInput,
    now_ms: u64,
    truecolor: bool,
) -> Vec<SubagentStatusDecoration> {
    input
        .tasks
        .iter()
        .map(|task| SubagentStatusDecoration {
            id: task.id.clone(),
            content: render_task(task, input.columns, now_ms, truecolor),
        })
        .collect()
}

fn render_task(task: &SubagentStatusTask, columns: usize, now_ms: u64, truecolor: bool) -> String {
    if columns == 0 {
        return String::new();
    }

    let glyph = status_glyph(&task.status, truecolor);
    if columns == 1 {
        return glyph;
    }

    let name = task
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| task_type_label(&task.task_type));
    let model = task.model.as_deref().map(model_label);
    let tokens = token_label(task.token_count, task.context_window_size);
    let elapsed = elapsed_label(now_ms.saturating_sub(task.start_time));
    let detail = (!task.label.trim().is_empty())
        .then(|| task.label.trim())
        .or_else(|| (!task.description.trim().is_empty()).then(|| task.description.trim()));

    let burn = tokens_per_minute(&task.token_samples).map(burn_label);

    // The burn chip rides only the richest variant so width reduction sheds it
    // before any other field.
    let mut with_burn = vec![name.as_str()];
    if let Some(model) = model.as_deref() {
        with_burn.push(model);
    }
    with_burn.push(tokens.as_str());
    if let Some(burn) = burn.as_deref() {
        with_burn.push(burn);
    }
    with_burn.push(elapsed.as_str());
    if let Some(detail) = detail {
        with_burn.push(detail);
    }

    let mut full = vec![name.as_str()];
    if let Some(model) = model.as_deref() {
        full.push(model);
    }
    full.push(tokens.as_str());
    full.push(elapsed.as_str());
    if let Some(detail) = detail {
        full.push(detail);
    }

    let mut without_detail = vec![name.as_str()];
    if let Some(model) = model.as_deref() {
        without_detail.push(model);
    }
    without_detail.push(tokens.as_str());
    without_detail.push(elapsed.as_str());

    let variants = [
        with_burn.join(" · "),
        full.join(" · "),
        without_detail.join(" · "),
        [name.as_str(), tokens.as_str(), elapsed.as_str()].join(" · "),
        [name.as_str(), tokens.as_str()].join(" · "),
        name,
    ];
    let body_width = columns.saturating_sub(2);
    let body = variants
        .iter()
        .find(|variant| visible_width(variant) <= body_width)
        .cloned()
        .unwrap_or_else(|| truncate_with_ellipsis(&variants[5], body_width));

    if body.is_empty() {
        glyph
    } else {
        format!("{glyph} {body}")
    }
}

fn status_glyph(status: &str, truecolor: bool) -> String {
    // Claude Code labels an in-flight task with several synonyms (`running`,
    // `active`, `in_progress`), and likewise for the other lifecycle states;
    // group them so a live agent never renders as an unknown-status bullet.
    match status {
        "running" | "active" | "in_progress" => tokens::ACCENT.paint("●", truecolor),
        "completed" | "done" => tokens::SUCCESS.paint("✓", truecolor),
        "failed" | "error" => tokens::ERROR.paint("✗", truecolor),
        "paused" | "blocked" => tokens::WARNING.paint("◌", truecolor),
        "cancelled" | "canceled" | "killed" => tokens::MUTED.paint("■", truecolor),
        // `pending`, `queued`, and any unrecognized status share a neutral bullet.
        _ => tokens::MUTED.paint("○", truecolor),
    }
}

fn task_type_label(task_type: &str) -> String {
    task_type
        .strip_prefix("local_")
        .unwrap_or(task_type)
        .replace('_', " ")
}

fn model_label(model: &str) -> String {
    let model = model.rsplit('/').next().unwrap_or(model);
    let friendly = friendly_model_name(model, model);
    friendly
        .strip_prefix("Claude ")
        .unwrap_or(&friendly)
        .to_string()
}

fn token_label(token_count: u64, context_window_size: Option<u64>) -> String {
    match context_window_size.filter(|limit| *limit > 0) {
        Some(limit) => {
            let percent = ((token_count as f64 / limit as f64) * 100.0).round() as u64;
            format!(
                "{}/{} {}%",
                format_tokens(token_count),
                format_tokens(limit),
                percent
            )
        }
        None => format_tokens(token_count),
    }
}

fn elapsed_label(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1_000;
    if seconds < 60 {
        return format!("{seconds}s");
    }

    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }

    let hours = minutes / 60;
    if hours < 24 {
        let remaining_minutes = minutes % 60;
        return if remaining_minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{remaining_minutes}m")
        };
    }

    let days = hours / 24;
    let remaining_hours = hours % 24;
    if remaining_hours == 0 {
        format!("{days}d")
    } else {
        format!("{days}d{remaining_hours}h")
    }
}

fn visible_width(value: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for ch in value.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if ch == '\u{1b}' {
            in_escape = true;
        } else {
            width += 1;
        }
    }
    width
}

fn truncate_with_ellipsis(value: &str, max_width: usize) -> String {
    if visible_width(value) <= max_width {
        return value.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut output: String = value.chars().take(max_width - 1).collect();
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> SubagentStatusTask {
        SubagentStatusTask {
            id: "agent-test".to_string(),
            name: Some("Explore".to_string()),
            task_type: "local_agent".to_string(),
            status: "running".to_string(),
            description: "Inspect usage accounting".to_string(),
            label: "tracing transcript entries".to_string(),
            start_time: 1_000,
            model: Some("claude-opus-4-8".to_string()),
            effort: Some(SubagentEffort::Label("high".to_string())),
            context_window_size: Some(1_000_000),
            token_count: 48_213,
            token_samples: vec![12_040, 25_890, 48_213],
            cwd: Some("/repo".to_string()),
        }
    }

    #[test]
    fn renders_full_task_details() {
        let content = render_task(&task(), 120, 121_000, false);
        assert!(content.contains("Explore"));
        assert!(content.contains("Opus 4.8"));
        assert!(content.contains("48.2K/1M 5%"));
        assert!(content.contains("217.0K/m"));
        assert!(content.contains("2m"));
        assert!(content.contains("tracing transcript entries"));
    }

    #[test]
    fn narrows_task_content_to_columns() {
        let content = render_task(&task(), 12, 121_000, false);
        assert!(visible_width(&content) <= 12);
        assert!(content.contains("Explore"));
        assert!(!content.contains("Opus 4.8"));
        assert!(!content.contains("/m"));
    }

    #[test]
    fn drops_burn_chip_before_other_fields_when_narrow() {
        let content = render_task(&task(), 70, 121_000, false);
        assert!(!content.contains("/m"), "content: {content}");
        assert!(content.contains("Opus 4.8"), "content: {content}");
        assert!(
            content.contains("tracing transcript entries"),
            "content: {content}"
        );
    }

    #[test]
    fn renders_no_burn_chip_without_enough_samples() {
        let mut task = task();
        task.token_samples = vec![48_213];
        let content = render_task(&task, 120, 121_000, false);
        assert!(!content.contains("/m"), "content: {content}");
    }

    #[test]
    fn derives_tokens_per_minute_from_sample_span() {
        assert_eq!(tokens_per_minute(&[1_000, 2_000, 3_000]), Some(12_000.0));
    }

    #[test]
    fn omits_burn_rate_when_sample_span_is_zero() {
        assert_eq!(tokens_per_minute(&[48_213]), None);
        assert_eq!(tokens_per_minute(&[]), None);
    }

    #[test]
    fn omits_burn_rate_when_tokens_do_not_grow() {
        assert_eq!(tokens_per_minute(&[500, 500]), None);
        assert_eq!(tokens_per_minute(&[500, 400]), None);
    }

    #[test]
    fn falls_back_to_task_type_and_description() {
        let mut task = task();
        task.name = None;
        task.label.clear();
        task.model = None;
        task.context_window_size = None;
        let content = render_task(&task, 80, 1_000, false);
        assert!(content.contains("agent"));
        assert!(content.contains("Inspect usage accounting"));
        assert!(content.contains("48.2K"));
    }

    #[test]
    fn zero_columns_hides_the_row() {
        assert_eq!(render_task(&task(), 0, 1_000, false), "");
    }

    #[test]
    fn status_glyph_maps_claude_code_status_synonyms() {
        // `contains` so the assertion holds whether or not colors wrap the glyph.
        for s in ["running", "active", "in_progress"] {
            assert!(status_glyph(s, false).contains('●'), "status {s}");
        }
        for s in ["completed", "done"] {
            assert!(status_glyph(s, false).contains('✓'), "status {s}");
        }
        for s in ["failed", "error"] {
            assert!(status_glyph(s, false).contains('✗'), "status {s}");
        }
        // pending, queued, and unrecognized statuses share a neutral bullet
        // rather than rendering as an unknown-status marker.
        for s in ["pending", "queued", "brand-new-status"] {
            let glyph = status_glyph(s, false);
            assert!(glyph.contains('○'), "status {s}");
            assert!(!glyph.contains('?'), "status {s}");
        }
    }
}
