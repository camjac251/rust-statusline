use chrono::{DateTime, Local, Timelike};

use crate::beads::format_bead_display;
use crate::gastown::format_gastown_display;
use crate::models::{BeadsInfo, GasTownInfo};
use crate::models::{PromptCacheBucketKind, PromptCacheInfo};
use crate::provenance::CostProvenance;
use crate::tokens;
use crate::usage_api::is_direct_claude_api;
use std::env;
use std::fmt::Write as _;
use std::path::Path;

// ═══════════════════════════════════════════════════════════════════════════════
// UNICODE SYMBOLS - Matching Claude Code's icon set
// ═══════════════════════════════════════════════════════════════════════════════

const SYM_PROMPT: &str = "❯"; // Command prompt
const SYM_SEPARATOR: &str = "│"; // Vertical bar separator
const SYM_DOT: &str = "·"; // Dot separator (compact)
const SYM_ARROW_RIGHT: &str = "→"; // Projection arrow
const SYM_ARROW_UP: &str = "↑"; // Ahead indicator
const SYM_ARROW_DOWN: &str = "↓"; // Behind indicator
const SYM_DOLLAR: &str = "$"; // Cost indicator

// Terminal width thresholds for responsive formatting
const WIDTH_NARROW: u16 = 140;
const WIDTH_MEDIUM: u16 = 200;
// Account for Claude CLI padding/margins (status line container has padding)
const TERMINAL_MARGIN: u16 = 15;
// Claude Code's footer shares space with hints/notifications, so fit to a
// smaller budget than the full terminal width to avoid Ink truncation.
const CLAUDE_FOOTER_RESERVE: u16 = 44;
const SHORT_TERMINAL_ROWS: u16 = 24;
const DROP_BEFORE_SHRINK_MARGIN: u8 = 40;

// Provide a no-op color shim when "colors" feature is disabled.
// main.rs references this for its own trivial color usage.
#[cfg(not(feature = "colors"))]
pub mod color_shim {
    use std::fmt::{self, Display, Formatter};

    #[derive(Clone)]
    pub struct Plain(pub String);

    impl Display for Plain {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    pub trait ColorizeShim {
        fn as_str(&self) -> &str;

        fn bright_black(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn bright_white(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn cyan(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn bold(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn dimmed(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
    }

    impl ColorizeShim for &str {
        fn as_str(&self) -> &str {
            self
        }
    }
    impl ColorizeShim for String {
        fn as_str(&self) -> &str {
            self.as_str()
        }
    }
    impl ColorizeShim for Plain {
        fn as_str(&self) -> &str {
            &self.0
        }
    }
}

use crate::cli::{Args, LabelsArg, TimeFormatArg};
use crate::models::{Block, GitInfo, HookJson, RateLimitInfo};
use crate::usage_api::{UsageLimit, UsageSummary};
use crate::utils::{
    auto_compact_enabled, auto_compact_headroom_tokens, context_limit_for_model_display,
    deduce_provider_from_model, format_currency, format_path, format_tokens,
    reserved_output_tokens_for_model, system_overhead_tokens,
};
use crate::window::window_bounds;

fn format_pct(pct: f64) -> String {
    let rounded = pct.round();
    if (pct - rounded).abs() < 0.05 {
        format!("{:.0}%", rounded)
    } else {
        format!("{:.1}%", pct)
    }
}

// Helper: format with muted color for labels
fn muted_label(text: &str, tc: bool) -> String {
    tokens::MUTED.dim(text, tc)
}

// Helper: format separator
fn separator(tc: bool, compact: bool) -> String {
    let sym = if compact { SYM_DOT } else { SYM_SEPARATOR };
    format!(" {} ", tokens::MUTED.dim(sym, tc))
}

fn colorize_percent(pct: f64, args: &Args) -> String {
    let formatted = format_pct(pct);
    let tc = is_truecolor_enabled(args);
    let token = tokens::gradient(pct, 100.0);
    if pct >= 80.0 {
        token.bold(&formatted, tc)
    } else {
        token.paint(&formatted, tc)
    }
}

fn usage_limit_json(limit: &UsageLimit) -> serde_json::Value {
    serde_json::json!({
        "utilization": limit.utilization.map(|v| (v * 10.0).round() / 10.0),
        "used": limit.used,
        "remaining": limit.remaining,
        "resets_at": limit.resets_at.map(|d| d.to_rfc3339()),
    })
}

fn active_effort_level(hook: &HookJson) -> Option<String> {
    hook.effort
        .as_ref()
        .map(|effort| effort.level.trim().to_lowercase())
        .filter(|effort| !effort.is_empty())
        .or_else(|| {
            env::var("CLAUDE_CODE_EFFORT_LEVEL")
                .ok()
                .and_then(|effort| {
                    let effort = effort.trim().to_lowercase();
                    if effort.is_empty() || effort == "unset" {
                        None
                    } else {
                        Some(effort)
                    }
                })
        })
}

fn prompt_cache_json(info: Option<&PromptCacheInfo>) -> serde_json::Value {
    info.map(|info| {
        let primary = info.primary_bucket();
        let buckets = info
            .buckets
            .iter()
            .map(|bucket| {
                serde_json::json!({
                    "kind": bucket.kind.as_str(),
                    "input_tokens": bucket.input_tokens,
                    "ttl_seconds": bucket.ttl_seconds,
                    "created_at": bucket.created_at.to_rfc3339(),
                    "expires_at": bucket.expires_at().to_rfc3339(),
                    "age_seconds": bucket.age_seconds_at(info.now),
                    "remaining_seconds": bucket.remaining_seconds_at(info.now),
                    "percent_remaining": (bucket.percent_remaining_at(info.now) * 10.0).round() / 10.0,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "ttl_seconds": primary.map(|bucket| bucket.ttl_seconds),
            "last_response_at": primary.map(|bucket| bucket.created_at.to_rfc3339()),
            "last_activity_at": info.last_activity_at().map(|d| d.to_rfc3339()),
            "last_cache_write_at": info.last_cache_write_at.map(|d| d.to_rfc3339()),
            "last_cache_read_at": info.last_cache_read_at.map(|d| d.to_rfc3339()),
            "expires_at": primary.map(|bucket| bucket.expires_at().to_rfc3339()),
            "age_seconds": info.activity_age_seconds(),
            "write_age_seconds": info.write_age_seconds(),
            "read_age_seconds": info.read_age_seconds(),
            "remaining_seconds": info.remaining_seconds(),
            "percent_remaining": (info.percent_remaining() * 10.0).round() / 10.0,
            "cache_write_input_tokens": info.cache_write_input_tokens,
            "cache_read_input_tokens": info.cache_read_input_tokens,
            "buckets": buckets,
        })
    })
    .unwrap_or(serde_json::Value::Null)
}

fn render_prompt_cache_segment(info: &PromptCacheInfo, tc: bool) -> String {
    let mut parts: Vec<String> = info
        .buckets
        .iter()
        .filter(|bucket| bucket.remaining_seconds_at(info.now) > 0)
        .map(|bucket| match bucket.kind {
            PromptCacheBucketKind::FiveMinute => "5m".to_string(),
            PromptCacheBucketKind::OneHour => "1h".to_string(),
            PromptCacheBucketKind::Unknown => "?".to_string(),
        })
        .collect();
    if parts.is_empty() && info.cache_read_input_tokens > 0 {
        parts.push("hit".to_string());
    }
    if parts.is_empty() {
        parts.push("expired".to_string());
    }

    let remaining = info.remaining_seconds();
    let token = if remaining == 0 && info.cache_read_input_tokens == 0 {
        tokens::WARNING
    } else {
        tokens::PRIMARY_DIM
    };

    let mut token_parts = Vec::new();
    let show_read_tokens = match (info.last_cache_read_at, info.last_cache_write_at) {
        (Some(read), Some(write)) => read >= write,
        (Some(_), None) => true,
        (None, _) => false,
    };
    let show_write_tokens = match (info.last_cache_write_at, info.last_cache_read_at) {
        (Some(write), Some(read)) => write >= read,
        (Some(_), None) => true,
        (None, _) => false,
    };
    if show_read_tokens && info.cache_read_input_tokens > 0 {
        token_parts.push(format!("r:{}", format_tokens(info.cache_read_input_tokens)));
    }
    if show_write_tokens && info.cache_write_input_tokens > 0 {
        token_parts.push(format!(
            "w:{}",
            format_tokens(info.cache_write_input_tokens)
        ));
    }

    let mut segment = format!(
        "{}{}",
        muted_label("cache:", tc),
        token.paint(&parts.join(" "), tc)
    );
    if !token_parts.is_empty() {
        let _ = write!(
            segment,
            " {}",
            tokens::PRIMARY_DIM.paint(&token_parts.join(" "), tc)
        );
    }
    segment
}

fn is_truecolor_enabled(args: &Args) -> bool {
    if args.truecolor {
        return true; // Explicit flag always overrides
    }
    if let Ok(v) = env::var("CLAUDE_TRUECOLOR")
        && v.trim() == "1"
    {
        return true;
    }
    // Auto-detect common truecolor environment variables
    if env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit")) {
        return true;
    }
    if env::var("TERM").is_ok_and(|v| v.contains("xterm-truecolor") || v.contains("xterm-256color"))
    {
        return true;
    }
    false
}

fn env_dimension(name: &str) -> Option<u16> {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|value| *value > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalWidth {
    Narrow, // < 140 cols
    Medium, // 140-200 cols
    Wide,   // > 200 cols
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    Compact,
    Rich,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderProfile {
    width: TerminalWidth,
    mode: RenderMode,
    safe_width: u16,
}

struct StatusSegment {
    variants: Vec<String>,
    priority: u8,
}

fn status_segment(text: String, priority: u8) -> StatusSegment {
    adaptive_segment(vec![text], priority)
}

fn adaptive_segment(variants: Vec<String>, priority: u8) -> StatusSegment {
    let mut deduped: Vec<String> = Vec::new();
    for variant in variants {
        if variant.is_empty() {
            continue;
        }
        if deduped
            .last()
            .is_none_or(|last| strip_ansi(last) != strip_ansi(&variant))
        {
            deduped.push(variant);
        }
    }

    if deduped.is_empty() {
        deduped.push(String::new());
    }

    StatusSegment {
        variants: deduped,
        priority,
    }
}

fn strip_ansi(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek().is_some_and(|next| *next == '[') {
            chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
            continue;
        }

        stripped.push(ch);
    }

    stripped
}

fn visible_width(text: &str) -> usize {
    strip_ansi(text).chars().count()
}

fn fit_line_to_width(line: String, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if visible_width(&line) <= max_width {
        return line;
    }

    truncate_label(&strip_ansi(&line), max_width)
}

fn join_status_segments(
    prefix: &str,
    segments: &[StatusSegment],
    variant_indexes: &[usize],
    separator: &str,
) -> String {
    let body = segments
        .iter()
        .zip(variant_indexes)
        .map(|(segment, variant_index)| {
            segment
                .variants
                .get(*variant_index)
                .or_else(|| segment.variants.last())
                .map(String::as_str)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(separator);

    if prefix.is_empty() {
        body
    } else if body.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix} {body}")
    }
}

fn fit_status_segments(
    prefix: &str,
    mut segments: Vec<StatusSegment>,
    separator: &str,
    max_width: u16,
) -> String {
    let max_width = usize::from(max_width);
    let mut variant_indexes = vec![0; segments.len()];
    let mut line = join_status_segments(prefix, &segments, &variant_indexes, separator);

    while visible_width(&line) > max_width {
        let shrink_candidate = segments
            .iter()
            .enumerate()
            .filter(|(index, segment)| variant_indexes[*index] + 1 < segment.variants.len())
            .min_by_key(|(_, segment)| segment.priority)
            .map(|(index, segment)| (index, segment.priority));

        let drop_candidate = (segments.len() > 1)
            .then(|| {
                segments
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, segment)| segment.priority)
                    .map(|(index, segment)| (index, segment.priority))
            })
            .flatten();

        let should_drop = match (drop_candidate, shrink_candidate) {
            (Some((_, drop_priority)), Some((_, shrink_priority))) => {
                drop_priority.saturating_add(DROP_BEFORE_SHRINK_MARGIN) < shrink_priority
            }
            (Some(_), None) => true,
            _ => false,
        };

        if should_drop {
            if let Some((remove_index, _)) = drop_candidate {
                segments.remove(remove_index);
                variant_indexes.remove(remove_index);
                line = join_status_segments(prefix, &segments, &variant_indexes, separator);
                continue;
            }
        }

        if let Some((shrink_index, _)) = shrink_candidate {
            variant_indexes[shrink_index] += 1;
            line = join_status_segments(prefix, &segments, &variant_indexes, separator);
            continue;
        }

        if let Some((remove_index, _)) = drop_candidate {
            segments.remove(remove_index);
            variant_indexes.remove(remove_index);
            line = join_status_segments(prefix, &segments, &variant_indexes, separator);
        } else {
            break;
        }
    }

    fit_line_to_width(line, max_width)
}

fn width_class_for(safe_width: u16) -> TerminalWidth {
    if safe_width < WIDTH_NARROW {
        TerminalWidth::Narrow
    } else if safe_width < WIDTH_MEDIUM {
        TerminalWidth::Medium
    } else {
        TerminalWidth::Wide
    }
}

fn detect_terminal_dimensions() -> (u16, Option<u16>) {
    let override_width = env_dimension("CLAUDE_TERMINAL_WIDTH");
    let statusline_width = env_dimension("COLUMNS");
    let statusline_height = env_dimension("LINES");
    let detected = terminal_size::terminal_size();

    let width = override_width
        .or(statusline_width)
        .or_else(|| detected.map(|(terminal_size::Width(w), _)| w))
        .unwrap_or(WIDTH_MEDIUM + TERMINAL_MARGIN + CLAUDE_FOOTER_RESERVE);
    let height = statusline_height.or_else(|| detected.map(|(_, terminal_size::Height(h))| h));

    (width, height)
}

fn render_profile_for_dimensions(width: u16, height: Option<u16>) -> RenderProfile {
    let safe_width = width
        .saturating_sub(TERMINAL_MARGIN + CLAUDE_FOOTER_RESERVE)
        .max(1);
    let width_class = width_class_for(safe_width);
    let mode = if height.is_some_and(|rows| rows < SHORT_TERMINAL_ROWS) || safe_width < WIDTH_NARROW
    {
        RenderMode::Compact
    } else {
        RenderMode::Rich
    };

    RenderProfile {
        width: width_class,
        mode,
        safe_width,
    }
}

fn render_profile() -> RenderProfile {
    let (width, height) = detect_terminal_dimensions();
    render_profile_for_dimensions(width, height)
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }

    let mut truncated = String::new();
    for ch in text.chars().take(max_chars - 1) {
        truncated.push(ch);
    }
    truncated.push('…');
    truncated
}

fn hook_worktree_name(hook: &HookJson) -> Option<&str> {
    hook.worktree
        .as_ref()
        .map(|worktree| worktree.name.as_str())
        .or(hook.workspace.git_worktree.as_deref())
        .filter(|name| !name.is_empty())
}

fn is_claude_internal_worktree_path(path: &str) -> bool {
    path.contains("/.claude/worktrees/")
}

fn path_basename(path: &str) -> Option<&str> {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
}

fn should_show_header_worktree(hook: &HookJson) -> bool {
    let Some(worktree_name) = hook_worktree_name(hook) else {
        return false;
    };
    if is_claude_internal_worktree_path(&hook.workspace.current_dir) {
        return false;
    }

    !path_basename(&hook.workspace.current_dir)
        .is_some_and(|base| base.eq_ignore_ascii_case(worktree_name))
}

fn should_show_added_dirs(hook: &HookJson) -> bool {
    match hook.workspace.added_dirs.as_slice() {
        [] => false,
        [only_dir] => {
            let only_path = Path::new(only_dir);
            let matches_project = only_path == Path::new(&hook.workspace.project_dir);
            let matches_current = only_path == Path::new(&hook.workspace.current_dir);

            !(matches_project || matches_current)
        }
        _ => true,
    }
}

fn added_dirs_segment(hook: &HookJson, tc: bool) -> Option<String> {
    if !should_show_added_dirs(hook) {
        return None;
    }
    let count = hook.workspace.added_dirs.len();

    Some(format!(
        "{}{}",
        muted_label("dirs:", tc),
        tokens::ACCENT.paint(&format!("+{}", count), tc)
    ))
}

fn worktree_segment(
    hook: &HookJson,
    git_info: Option<&GitInfo>,
    tc: bool,
    width: TerminalWidth,
) -> Option<String> {
    let worktree_name = hook_worktree_name(hook)
        .map(ToOwned::to_owned)
        .or_else(|| {
            git_info
                .and_then(|info| info.is_linked_worktree)
                .filter(|linked| *linked)
                .map(|_| "linked".to_string())
        })?;
    let max_len = match width {
        TerminalWidth::Narrow => 10,
        TerminalWidth::Medium => 14,
        TerminalWidth::Wide => 18,
    };

    Some(format!(
        "{}{}",
        muted_label("wt:", tc),
        tokens::ACCENT.paint(&truncate_label(&worktree_name, max_len), tc)
    ))
}

fn compact_workspace_segment(hook: &HookJson, tc: bool) -> Option<StatusSegment> {
    let repo_name = hook
        .workspace
        .repo
        .as_ref()
        .map(|repo| repo.name.as_str())
        .filter(|name| !name.is_empty());
    let project_base = repo_name.or_else(|| path_basename(&hook.workspace.project_dir));
    let current_base = path_basename(&hook.workspace.current_dir);
    let current_differs =
        Path::new(&hook.workspace.current_dir) != Path::new(&hook.workspace.project_dir);

    let mut variants = Vec::new();
    if current_differs
        && let (Some(project), Some(current)) = (project_base, current_base)
        && !project.eq_ignore_ascii_case(current)
    {
        variants.push(tokens::ACCENT.paint(&format!("{project}/{current}"), tc));
        variants.push(tokens::ACCENT.paint(current, tc));
    }

    if let Some(project) = project_base.or(current_base) {
        variants.push(tokens::ACCENT.paint(project, tc));
    }

    (!variants.is_empty()).then(|| adaptive_segment(variants, 75))
}

pub fn model_colored_name(model_id: &str, display: &str, args: &Args) -> String {
    // Respect NO_COLOR if set: return plain string
    if env::var("NO_COLOR").is_ok() {
        return display.to_string();
    }
    let lower_id = model_id.to_lowercase();
    let lower_disp = display.to_lowercase();
    let tc = is_truecolor_enabled(args);

    // Fable/Mythos tier -> Rose
    let token = if lower_id.contains("fable")
        || lower_disp.contains("fable")
        || lower_id.contains("mythos")
        || lower_disp.contains("mythos")
    {
        tokens::MODEL_FABLE
    }
    // Opus family -> Purple
    else if lower_id.contains("opus") || lower_disp.contains("opus") {
        tokens::MODEL_OPUS
    }
    // Sonnet family -> Amber/Yellow
    else if lower_id.contains("sonnet") || lower_disp.contains("sonnet") {
        tokens::MODEL_SONNET
    }
    // Haiku family -> Cyan/Blue
    else if lower_id.contains("haiku") || lower_disp.contains("haiku") {
        tokens::MODEL_HAIKU
    } else {
        // Unknown/Other -> White
        tokens::PRIMARY
    };
    token.paint(display, tc)
}

fn normalized_model_label(
    model_id: &str,
    model_display_name: &str,
    context_limit_override: Option<u64>,
) -> String {
    let effective_limit = context_limit_override
        .unwrap_or_else(|| context_limit_for_model_display(model_id, model_display_name));
    let display_lower = model_display_name.to_lowercase();
    let already_shows_context = display_lower.contains("1m") || display_lower.contains("200k");

    if effective_limit >= 1_000_000 && !already_shows_context {
        format!("{model_display_name} 1M")
    } else if effective_limit < 1_000_000 && display_lower.contains("1m") {
        model_display_name
            .replace(" (with 1M context)", "")
            .replace(" [1m]", "")
            .replace("[1m]", "")
            .trim()
            .to_string()
    } else {
        model_display_name.to_string()
    }
}

fn model_family_label(model_id: &str, display: &str) -> Option<&'static str> {
    let lower_id = model_id.to_lowercase();
    let lower_display = display.to_lowercase();
    if lower_id.contains("fable") || lower_display.contains("fable") {
        Some("Fable")
    } else if lower_id.contains("mythos") || lower_display.contains("mythos") {
        Some("Mythos")
    } else if lower_id.contains("opus") || lower_display.contains("opus") {
        Some("Opus")
    } else if lower_id.contains("sonnet") || lower_display.contains("sonnet") {
        Some("Sonnet")
    } else if lower_id.contains("haiku") || lower_display.contains("haiku") {
        Some("Haiku")
    } else {
        None
    }
}

fn model_version_hint(display: &str) -> Option<&str> {
    display.split_whitespace().find(|part| {
        part.chars().any(|ch| ch.is_ascii_digit())
            && part
                .chars()
                .all(|ch| ch.is_ascii_digit() || ch == '.' || ch == '-')
    })
}

fn compact_model_label(model_id: &str, display: &str) -> String {
    let without_vendor = display
        .trim_start_matches("Claude ")
        .replace(" (with 1M context)", "")
        .replace(" [1m]", "")
        .replace("[1m]", "")
        .trim()
        .to_string();

    if !without_vendor.is_empty() && without_vendor != display {
        without_vendor
    } else if let Some(family) = model_family_label(model_id, display) {
        model_version_hint(display).map_or_else(
            || family.to_string(),
            |version| format!("{family} {version}"),
        )
    } else {
        display.to_string()
    }
}

fn tiny_model_label(model_id: &str, display: &str) -> String {
    let Some(family) = model_family_label(model_id, display) else {
        return truncate_label(display, 12);
    };
    family.to_string()
}

fn render_model_segment_variants(
    model_id: &str,
    model_display_name: &str,
    context_limit_override: Option<u64>,
    args: &Args,
    is_fast_mode: bool,
    max_chars: Option<usize>,
) -> StatusSegment {
    let tc = is_truecolor_enabled(args);
    let base = normalized_model_label(model_id, model_display_name, context_limit_override);
    let long = max_chars.map_or(base.clone(), |limit| truncate_label(&base, limit));
    let medium = compact_model_label(model_id, &base);
    let tiny = tiny_model_label(model_id, &base);
    let fast = tokens::WARNING.bold("fast", tc);
    let render = |label: &str| {
        let colored = model_colored_name(model_id, label, args);
        if is_fast_mode {
            format!("{colored} {fast}")
        } else {
            colored
        }
    };

    let mut variants = if max_chars.is_some() {
        vec![render(&medium), render(&tiny)]
    } else {
        vec![render(&long), render(&medium), render(&tiny)]
    };
    if is_fast_mode {
        variants.push(model_colored_name(model_id, &tiny, args));
    }
    adaptive_segment(variants, 130)
}

fn cost_segment_variants(
    long_label: &str,
    short_label: &str,
    value: f64,
    gradient_max: Option<f64>,
    tc: bool,
    priority: u8,
) -> StatusSegment {
    let cost_str = format_currency(value);
    let cost_value = if let Some(max) = gradient_max {
        tokens::gradient(value, max).paint(&cost_str, tc)
    } else {
        tokens::PRIMARY.bold(&cost_str, tc)
    };
    let dollar = tokens::MUTED.paint(SYM_DOLLAR, tc);
    adaptive_segment(
        vec![
            format!("{}{}{}", muted_label(long_label, tc), dollar, cost_value),
            format!("{}{}{}", muted_label(short_label, tc), dollar, cost_value),
            format!("{}{}", dollar, cost_value),
        ],
        priority,
    )
}

fn use_12h_time(args: &Args) -> bool {
    match args.time_fmt {
        TimeFormatArg::H12 => true,
        TimeFormatArg::H24 => false,
        TimeFormatArg::Auto => {
            if let Ok(forced) = env::var("CLAUDE_TIME_FORMAT") {
                forced.trim() == "12"
            } else {
                let lc = env::var("LC_TIME")
                    .or_else(|_| env::var("LANG"))
                    .unwrap_or_default()
                    .to_lowercase();
                lc.contains("en_us")
            }
        }
    }
}

fn render_reset_countdown(remaining_minutes: f64, tc: bool) -> String {
    let rem_h = (remaining_minutes as i64) / 60;
    let rem_m = (remaining_minutes as i64) % 60;
    let countdown = if rem_h > 0 {
        format!("{}h{}m", rem_h, rem_m)
    } else {
        format!("{}m", rem_m)
    };

    if remaining_minutes < 30.0 {
        tokens::ERROR.bold(&countdown, tc)
    } else if remaining_minutes < 60.0 {
        tokens::WARNING.bold(&countdown, tc)
    } else if remaining_minutes < 180.0 {
        tokens::WARNING.paint(&countdown, tc)
    } else {
        tokens::PRIMARY_DIM.paint(&countdown, tc)
    }
}

fn render_reset_clock(
    active_block: Option<&Block>,
    latest_reset: Option<DateTime<chrono::Utc>>,
    use_12h: bool,
) -> String {
    let window_end_local = if let Some(block) = active_block {
        block.end.with_timezone(&Local)
    } else {
        let now_utc = chrono::Utc::now();
        let (_, end) = window_bounds(now_utc, latest_reset);
        end.with_timezone(&Local)
    };

    if window_end_local.minute() == 0 {
        if use_12h {
            window_end_local.format("%-I%p").to_string().to_lowercase()
        } else {
            window_end_local.format("%H").to_string()
        }
    } else if use_12h {
        window_end_local
            .format("%-I:%M%p")
            .to_string()
            .to_lowercase()
    } else {
        window_end_local.format("%H:%M").to_string()
    }
}

fn render_reset_inline(
    remaining_minutes: f64,
    active_block: Option<&Block>,
    latest_reset: Option<DateTime<chrono::Utc>>,
    use_12h: bool,
    tc: bool,
) -> String {
    let countdown_colored = render_reset_countdown(remaining_minutes, tc);
    let reset_disp = render_reset_clock(active_block, latest_reset, use_12h);

    format!(
        " {} {}",
        countdown_colored,
        muted_label(&format!("({reset_disp})"), tc)
    )
}

fn build_git_status_segment(
    git_info: Option<&GitInfo>,
    tc: bool,
    width: TerminalWidth,
    lines_delta: Option<(i64, i64)>,
    include_lines_delta: bool,
) -> Option<String> {
    let git_info = git_info?;
    let mut git_seg = String::new();
    let branch_max_len = match (width, include_lines_delta) {
        (TerminalWidth::Narrow, true) => 12,
        (TerminalWidth::Medium, true) => 20,
        (TerminalWidth::Wide, true) => 28,
        (TerminalWidth::Narrow, false) => 12,
        (TerminalWidth::Medium, false) => 16,
        (TerminalWidth::Wide, false) => 24,
    };

    if let Some(branch) = git_info.branch.as_ref() {
        let branch_name = truncate_label(branch, branch_max_len);
        git_seg.push_str(&tokens::PRIMARY.paint(&branch_name, tc));
        if let Some(short_commit) = git_info.short_commit.as_ref() {
            git_seg.push_str(&muted_label("@", tc));
            git_seg.push_str(&tokens::PRIMARY.paint(short_commit, tc));
        }
    } else if let Some(short_commit) = git_info.short_commit.as_ref() {
        git_seg.push_str(&muted_label("detached@", tc));
        git_seg.push_str(&tokens::PRIMARY.paint(short_commit, tc));
    }

    if git_info.is_clean == Some(false) {
        git_seg.push_str(&tokens::WARNING.paint("*", tc));
    }

    if let (Some(ahead), Some(behind)) = (git_info.ahead, git_info.behind) {
        if ahead > 0 {
            if !git_seg.is_empty() {
                git_seg.push(' ');
            }
            git_seg.push_str(&tokens::SUCCESS.paint(&format!("{}{}", SYM_ARROW_UP, ahead), tc));
        }
        if behind > 0 {
            if !git_seg.is_empty() {
                git_seg.push(' ');
            }
            git_seg.push_str(&tokens::ERROR.paint(&format!("{}{}", SYM_ARROW_DOWN, behind), tc));
        }
    }

    if include_lines_delta
        && let Some((added, removed)) = lines_delta
        && (added != 0 || removed != 0)
    {
        if !git_seg.is_empty() {
            git_seg.push(' ');
        }
        git_seg.push_str(&tokens::SUCCESS.paint(&format!("+{}", added), tc));
        git_seg.push_str(&tokens::ERROR.paint(&format!("-{}", removed.abs()), tc));
    }

    if git_seg.is_empty() {
        None
    } else {
        Some(git_seg)
    }
}

struct UsageSegmentTiming<'a> {
    remaining_minutes: f64,
    active_block: Option<&'a Block>,
    latest_reset: Option<DateTime<chrono::Utc>>,
}

struct UsageSegmentLabels<'a> {
    long: &'a str,
    short: &'a str,
}

fn render_usage_segment_variants(
    model_id: &str,
    args: &Args,
    usage_percent: Option<f64>,
    projected_percent: Option<f64>,
    timing: UsageSegmentTiming<'_>,
    usage_limits: Option<&UsageSummary>,
    labels: UsageSegmentLabels<'_>,
) -> Option<StatusSegment> {
    if !is_direct_claude_api(Some(model_id)) {
        return None;
    }

    let usage_value = usage_percent?;
    let tc = is_truecolor_enabled(args);
    let (long_label, short_label) = if usage_limits.is_some_and(|summary| summary.stale) {
        (format!("~{}", labels.long), format!("~{}", labels.short))
    } else {
        (labels.long.to_string(), labels.short.to_string())
    };
    let usage_colored = colorize_percent(usage_value, args);
    let projected_colored = projected_percent.map(|value| colorize_percent(value, args));
    let projected = projected_colored
        .as_ref()
        .map(|projected| format!("{}{}", tokens::MUTED.dim(SYM_ARROW_RIGHT, tc), projected))
        .unwrap_or_default();
    let countdown = render_reset_countdown(timing.remaining_minutes, tc);
    let inline = render_reset_inline(
        timing.remaining_minutes,
        timing.active_block,
        timing.latest_reset,
        use_12h_time(args),
        tc,
    );

    Some(adaptive_segment(
        vec![
            format!(
                "{}{}{}{}",
                muted_label(&long_label, tc),
                usage_colored,
                projected,
                inline
            ),
            format!(
                "{}{}{} {}",
                muted_label(&short_label, tc),
                usage_colored,
                projected,
                countdown
            ),
            format!("{}{}", muted_label(&short_label, tc), usage_colored),
            usage_colored,
        ],
        100,
    ))
}

fn render_context_segment_variants(
    model_id: &str,
    model_display_name: &str,
    context: Option<(u64, u32)>,
    context_limit_override: Option<u64>,
    args: &Args,
    tpm_indicator: f64,
    rich_labels: bool,
) -> StatusSegment {
    let tc = is_truecolor_enabled(args);
    let long_label = if rich_labels { "context:" } else { "ctx:" };
    let short_label = "ctx:";
    let Some((ctx_tokens, pct)) = context else {
        return adaptive_segment(
            vec![
                format!("{}{}", muted_label(long_label, tc), muted_label("N/A", tc)),
                format!("{}{}", muted_label(short_label, tc), muted_label("N/A", tc)),
                muted_label("N/A", tc),
            ],
            110,
        );
    };

    let show_tokens = !args.no_context_tokens;
    let show_percent = !args.no_context_percent;
    let pct_text = format!("{}%", pct);
    let pct_token = tokens::gradient(pct as f64, 100.0);
    let pct_colored = if pct >= 80 {
        pct_token.bold(&pct_text, tc)
    } else {
        pct_token.paint(&pct_text, tc)
    };
    let ctx_limit_full = context_limit_override
        .unwrap_or_else(|| context_limit_for_model_display(model_id, model_display_name));
    let ctx_limit_usable =
        ctx_limit_full.saturating_sub(reserved_output_tokens_for_model(model_id));
    let output_reserve = reserved_output_tokens_for_model(model_id);
    let overhead = system_overhead_tokens();
    let raw_tokens = ctx_tokens.saturating_sub(overhead);
    let tokens_colored = tokens::PRIMARY_DIM.paint(&format_tokens(ctx_tokens), tc);
    let raw_tokens_colored = tokens::PRIMARY_DIM.paint(&format_tokens(raw_tokens), tc);
    let limit_label = muted_label(&format_tokens(ctx_limit_full), tc);
    let over_usable = (ctx_tokens > ctx_limit_usable).then(|| ctx_tokens - ctx_limit_usable);

    let mut long = match (show_tokens, show_percent, overhead > 0 && rich_labels) {
        (true, true, true) => format!(
            "{}{} {}{}{}",
            muted_label(long_label, tc),
            raw_tokens_colored,
            muted_label("+", tc),
            muted_label(&format!("{} sys = ", format_tokens(overhead)), tc),
            tokens::PRIMARY_DIM.paint(
                &format!(
                    "{}/{} ({})",
                    format_tokens(ctx_tokens),
                    format_tokens(ctx_limit_full),
                    pct_colored
                ),
                tc,
            )
        ),
        (true, true, false) => format!(
            "{}{}/{} {}",
            muted_label(long_label, tc),
            tokens_colored,
            limit_label,
            pct_colored
        ),
        (true, false, _) => format!(
            "{}{}/{}",
            muted_label(long_label, tc),
            tokens_colored,
            limit_label
        ),
        (false, true, _) => format!("{}{}", muted_label(long_label, tc), pct_colored),
        (false, false, _) => format!("{}{}", muted_label(long_label, tc), muted_label("on", tc)),
    };

    if let Some(used) = over_usable
        && show_tokens
    {
        let _ = write!(
            long,
            " {}{}{}",
            muted_label("rsv:", tc),
            tokens::ERROR.paint(&format_tokens(used), tc),
            muted_label(&format!("/{}", format_tokens(output_reserve)), tc)
        );
    }

    if rich_labels
        && show_percent
        && !args.no_context_compact_hint
        && pct >= 40
        && crate::utils::auto_compact_enabled()
    {
        let usable = ctx_limit_full.saturating_sub(reserved_output_tokens_for_model(model_id));
        let cushion = crate::utils::auto_compact_headroom_tokens();
        let compact_trigger = usable.saturating_sub(cushion) as f64;
        let headroom_to_compact = (compact_trigger - ctx_tokens as f64).max(0.0);
        let compact_text = if tpm_indicator > 0.0 && headroom_to_compact > 0.0 {
            let eta_min = headroom_to_compact / tpm_indicator;
            let eta_min_i = eta_min.round() as i64;
            let eta_disp = if eta_min_i >= 120 {
                format!("~{}h", eta_min_i / 60)
            } else if eta_min_i >= 60 {
                format!("~{}h{}m", eta_min_i / 60, eta_min_i % 60)
            } else {
                format!("~{}m", eta_min_i)
            };
            format!(
                "{}@{}K {}",
                muted_label("compact:", tc),
                compact_trigger as u64 / 1000,
                eta_disp
            )
        } else {
            format!(
                "{}@{}K",
                muted_label("compact:", tc),
                compact_trigger as u64 / 1000
            )
        };
        let _ = write!(
            long,
            "{}{}",
            separator(tc, false),
            tokens::WARNING.paint(&compact_text, tc)
        );
    }

    let medium = match (show_tokens, show_percent) {
        (true, true) => format!(
            "{}{} {}",
            muted_label(short_label, tc),
            tokens_colored,
            pct_colored
        ),
        (true, false) => format!("{}{}", muted_label(short_label, tc), tokens_colored),
        (false, true) => format!("{}{}", muted_label(short_label, tc), pct_colored),
        (false, false) => format!("{}{}", muted_label(short_label, tc), muted_label("on", tc)),
    };
    let short = match (show_tokens, show_percent) {
        (_, true) => format!("{}{}", muted_label(short_label, tc), pct_colored),
        (true, false) => format!("{}{}", muted_label(short_label, tc), tokens_colored),
        (false, false) => muted_label("ctx", tc),
    };
    let tiny = match (show_tokens, show_percent) {
        (_, true) => pct_colored,
        (true, false) => tokens_colored,
        (false, false) => muted_label("ctx", tc),
    };

    adaptive_segment(vec![long, medium, short, tiny], 110)
}

fn wrap_header_segment(content: String, tc: bool) -> String {
    format!(
        "{}{}{}",
        tokens::MUTED.paint("[", tc),
        content,
        tokens::MUTED.paint("]", tc)
    )
}

fn wrap_header_segment_variants(segment: StatusSegment, tc: bool) -> StatusSegment {
    adaptive_segment(
        segment
            .variants
            .into_iter()
            .map(|variant| wrap_header_segment(variant, tc))
            .collect(),
        segment.priority,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_header_line(
    hook: &HookJson,
    git_info: Option<&GitInfo>,
    args: &Args,
    api_key_source: Option<&str>,
    lines_delta: Option<(i64, i64)>,
    beads_info: Option<&BeadsInfo>,
    gastown_info: Option<&GasTownInfo>,
    context_limit_override: Option<u64>,
    is_fast_mode: bool,
) -> Option<String> {
    let profile = render_profile();
    if profile.mode == RenderMode::Compact {
        return None;
    }

    let dir_fmt = format_path(&hook.workspace.current_dir);
    let tc = is_truecolor_enabled(args);

    // Build header segments: git (minimal) + model + beads + output_style + optional provider hints
    let mut header_parts: Vec<StatusSegment> = Vec::new();
    if !args.no_workspace_cwd {
        if let Some(base) = path_basename(&hook.workspace.current_dir) {
            header_parts.push(adaptive_segment(
                vec![
                    tokens::ACCENT.paint(&dir_fmt, tc),
                    tokens::ACCENT.paint(base, tc),
                ],
                90,
            ));
        } else {
            header_parts.push(status_segment(tokens::ACCENT.paint(&dir_fmt, tc), 90));
        }
    }
    if let Some(git_seg) = build_git_status_segment(git_info, tc, profile.width, lines_delta, true)
    {
        let compact_git = build_git_status_segment(git_info, tc, profile.width, None, false);
        let mut variants = vec![git_seg];
        if let Some(compact_git) = compact_git {
            variants.push(compact_git);
        }
        header_parts.push(wrap_header_segment_variants(
            adaptive_segment(variants, 80),
            tc,
        ));
    }
    if !args.no_git_worktree
        && should_show_header_worktree(hook)
        && let Some(wt_seg) = worktree_segment(hook, git_info, tc, profile.width)
    {
        header_parts.push(wrap_header_segment_variants(
            adaptive_segment(vec![wt_seg, muted_label("wt", tc)], 40),
            tc,
        ));
    }
    if !args.no_workspace_added_dirs
        && let Some(dirs_seg) = added_dirs_segment(hook, tc)
    {
        header_parts.push(wrap_header_segment_variants(
            adaptive_segment(
                vec![
                    dirs_seg,
                    format!(
                        "{}{}d",
                        tokens::ACCENT.paint("+", tc),
                        hook.workspace.added_dirs.len()
                    ),
                ],
                30,
            ),
            tc,
        ));
    }

    // Model segment (with fast mode indicator)
    if !args.no_workspace_model {
        let model_seg = render_model_segment_variants(
            &hook.model.id,
            &hook.model.display_name,
            context_limit_override,
            args,
            is_fast_mode,
            None,
        );
        header_parts.push(wrap_header_segment_variants(model_seg, tc));
    }

    // Beads current work segment (if available)
    if !args.no_integrations_beads
        && let Some(beads) = beads_info
    {
        if let Some(ref work) = beads.current_work {
            // Max display length depends on terminal width
            let max_len = match profile.width {
                TerminalWidth::Narrow => 25,
                TerminalWidth::Medium => 35,
                TerminalWidth::Wide => 50,
            };
            let work_display = format_bead_display(work, max_len);

            // Color based on priority (lower = more urgent)
            let work_colored = if work.priority == 0 {
                // P0 critical - red
                tokens::ERROR.bold(&work_display, tc)
            } else if work.priority == 1 {
                // P1 high - warning yellow
                tokens::WARNING.paint(&work_display, tc)
            } else {
                // P2+ normal - accent blue
                tokens::ACCENT.paint(&work_display, tc)
            };

            header_parts.push(status_segment(wrap_header_segment(work_colored, tc), 20));
        } else if beads.total_open > 0 {
            // No current work but there are open issues - show count
            let count_text = format!("{} open", beads.total_open);
            let count_colored = tokens::MUTED.dim(&count_text, tc);
            header_parts.push(status_segment(
                wrap_header_segment(format!("{}{}", muted_label("bd:", tc), count_colored), tc),
                20,
            ));
        }
    }

    // Beads alert segment (P0 + blocked); independently gated from the work segment.
    if !args.no_integrations_beads_alerts
        && let Some(beads) = beads_info
    {
        let mut alerts: Vec<String> = Vec::new();
        if beads.priorities.p0_critical > 0 {
            let p0_text = format!("🔴{}", beads.priorities.p0_critical);
            alerts.push(tokens::ERROR.bold(&p0_text, tc));
        }
        if beads.counts.blocked > 0 {
            let blocked_text = format!("⚠{}", beads.counts.blocked);
            alerts.push(tokens::WARNING.paint(&blocked_text, tc));
        }
        if !alerts.is_empty() {
            header_parts.push(status_segment(
                wrap_header_segment(alerts.join(" "), tc),
                25,
            ));
        }
    }

    // Gas Town segment (if in a Gas Town workspace)
    if !args.no_integrations_gastown
        && let Some(gt) = gastown_info
    {
        let max_len = match profile.width {
            TerminalWidth::Narrow => 30,
            TerminalWidth::Medium => 45,
            TerminalWidth::Wide => 60,
        };
        let gt_display = format_gastown_display(gt, max_len);
        if !gt_display.is_empty() {
            // Color based on context - warning for unread mail, accent otherwise
            let gt_token = if gt.mail.as_ref().is_some_and(|m| m.unread_count > 0) {
                tokens::WARNING
            } else {
                tokens::ACCENT
            };
            let gt_colored = gt_token.paint(&gt_display, tc);

            header_parts.push(status_segment(
                wrap_header_segment(format!("{}{}", muted_label("gt:", tc), gt_colored), tc),
                20,
            ));
        }
    }

    // Agent segment (if running as a subagent via --agent)
    if !args.no_workspace_agent
        && let Some(ref agent) = hook.agent
    {
        let agent_colored = tokens::ACCENT.paint(&agent.name, tc);
        header_parts.push(status_segment(
            wrap_header_segment(
                format!("{}{}", muted_label("agent:", tc), agent_colored),
                tc,
            ),
            50,
        ));
    }

    // Output style segment (skip "default")
    if !args.no_workspace_output_style {
        let name_lower = hook.output_style.name.to_lowercase();
        if name_lower != "default" {
            let style_colored = tokens::ACCENT.paint(&hook.output_style.name, tc);
            header_parts.push(wrap_header_segment_variants(
                adaptive_segment(
                    vec![
                        format!("{}{}", muted_label("style:", tc), style_colored),
                        format!("{}{}", muted_label("st:", tc), style_colored),
                    ],
                    10,
                ),
                tc,
            ));
        }
    }

    // Effort level segment (prefer live hook data, fall back to env var)
    if !args.no_workspace_effort
        && let Some(effort_lower) = active_effort_level(hook)
    {
        let label = match profile.width {
            TerminalWidth::Narrow => "eff:",
            _ => "effort:",
        };
        let effort_colored = match effort_lower.as_str() {
            "low" => tokens::EFFORT_LOW.paint(&effort_lower, tc),
            "medium" => tokens::EFFORT_MEDIUM.paint(&effort_lower, tc),
            "high" => tokens::EFFORT_HIGH.paint(&effort_lower, tc),
            "xhigh" => tokens::EFFORT_MAX.paint(&effort_lower, tc),
            "max" => tokens::EFFORT_MAX.bold(&effort_lower, tc),
            other => tokens::EFFORT_MEDIUM.paint(other, tc),
        };
        header_parts.push(wrap_header_segment_variants(
            adaptive_segment(
                vec![
                    format!("{}{}", muted_label(label, tc), effort_colored),
                    format!("{}{}", muted_label("eff:", tc), effort_colored),
                    effort_colored,
                ],
                55,
            ),
            tc,
        ));
    }

    // Optional provider hints; each sub-element gated independently.
    if args.provider_key_source || args.provider_name {
        let mut prov_hint_parts: Vec<String> = Vec::new();
        if args.provider_key_source {
            if let Some(src) = api_key_source {
                prov_hint_parts.push(format!(
                    "{}{}",
                    muted_label("key:", tc),
                    tokens::PRIMARY_DIM.paint(src, tc)
                ));
            }
        }
        if args.provider_name {
            let prov_disp = if let Ok(provider_env) = env::var("CLAUDE_PROVIDER") {
                match provider_env.to_lowercase().as_str() {
                    "firstparty" => "anthropic".to_string(),
                    other => other.to_string(),
                }
            } else {
                deduce_provider_from_model(&hook.model.id).to_string()
            };
            prov_hint_parts.push(format!(
                "{}{}",
                muted_label("prov:", tc),
                tokens::PRIMARY_DIM.paint(&prov_disp, tc)
            ));
        }
        if !prov_hint_parts.is_empty() {
            header_parts.push(status_segment(
                wrap_header_segment(prov_hint_parts.join(" "), tc),
                10,
            ));
        }
    }

    // Print header line: cwd then segments
    Some(fit_status_segments(
        "",
        header_parts,
        " ",
        profile.safe_width,
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn print_header(
    hook: &HookJson,
    git_info: Option<&GitInfo>,
    args: &Args,
    api_key_source: Option<&str>,
    lines_delta: Option<(i64, i64)>,
    beads_info: Option<&BeadsInfo>,
    gastown_info: Option<&GasTownInfo>,
    context_limit_override: Option<u64>,
    is_fast_mode: bool,
) {
    if let Some(line) = render_header_line(
        hook,
        git_info,
        args,
        api_key_source,
        lines_delta,
        beads_info,
        gastown_info,
        context_limit_override,
        is_fast_mode,
    ) {
        println!("{}", line);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_compact_text_output(
    hook: &HookJson,
    git_info: Option<&GitInfo>,
    args: &Args,
    is_fast_mode: bool,
    session_cost: f64,
    usage_percent: Option<f64>,
    remaining_minutes: f64,
    active_block: Option<&Block>,
    latest_reset: Option<DateTime<chrono::Utc>>,
    context: Option<(u64, u32)>,
    lines_delta: Option<(i64, i64)>,
    usage_limits: Option<&UsageSummary>,
    context_limit_override: Option<u64>,
) -> String {
    let profile = render_profile();
    let tc = is_truecolor_enabled(args);
    let prompt = tokens::ACCENT.paint(SYM_PROMPT, tc);
    let mut segments = Vec::new();

    if !args.no_workspace_cwd
        && let Some(cwd_seg) = compact_workspace_segment(hook, tc)
    {
        segments.push(cwd_seg);
    }

    if let Some(git_seg) = build_git_status_segment(git_info, tc, profile.width, lines_delta, false)
    {
        segments.push(status_segment(git_seg, 30));
    }
    if !args.no_git_worktree
        && let Some(wt_seg) = worktree_segment(hook, git_info, tc, profile.width)
    {
        segments.push(status_segment(wt_seg, 20));
    }
    if !args.no_workspace_added_dirs
        && let Some(dirs_seg) = added_dirs_segment(hook, tc)
    {
        segments.push(status_segment(dirs_seg, 10));
    }

    if !args.no_workspace_model {
        let model_max = match profile.width {
            TerminalWidth::Narrow => 16,
            TerminalWidth::Medium => 22,
            TerminalWidth::Wide => 28,
        };
        segments.push(render_model_segment_variants(
            &hook.model.id,
            &hook.model.display_name,
            context_limit_override,
            args,
            is_fast_mode,
            Some(model_max),
        ));
    }

    if !args.no_cost_session {
        segments.push(cost_segment_variants(
            "session:",
            "s:",
            session_cost,
            None,
            tc,
            80,
        ));
    }

    if !args.no_usage_five_hour
        && let Some(usage_seg) = render_usage_segment_variants(
            &hook.model.id,
            args,
            usage_percent,
            None,
            UsageSegmentTiming {
                remaining_minutes,
                active_block,
                latest_reset,
            },
            usage_limits,
            UsageSegmentLabels {
                long: "usage:",
                short: "u:",
            },
        )
    {
        segments.push(usage_seg);
    }

    if !args.no_context_tokens || !args.no_context_percent {
        segments.push(render_context_segment_variants(
            &hook.model.id,
            &hook.model.display_name,
            context,
            context_limit_override,
            args,
            0.0,
            false,
        ));
    }

    let separator = separator(tc, true);
    fit_status_segments(&prompt, segments, &separator, profile.safe_width)
}

/// Currency symbol for the extra-usage token; mirrors the Claude Code
/// formatter's symbol map and falls back to the ISO code for the rest.
fn extra_usage_symbol(currency: Option<&str>) -> String {
    match currency.unwrap_or("USD") {
        "USD" => SYM_DOLLAR.to_string(),
        "EUR" => "€".to_string(),
        "GBP" => "£".to_string(),
        "JPY" => "¥".to_string(),
        "BRL" => "R$".to_string(),
        "CAD" => "CA$".to_string(),
        "AUD" => "AU$".to_string(),
        "NZD" => "NZ$".to_string(),
        "SGD" => "SG$".to_string(),
        other => format!("{other} "),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_rich_text_output(
    args: &Args,
    model_id: &str,
    model_display_name: &str,
    session_cost: f64,
    today_cost: f64,
    total_cost: f64,
    usage_percent: Option<f64>,
    projected_percent: Option<f64>,
    remaining_minutes: f64,
    active_block: Option<&Block>,
    latest_reset: Option<DateTime<chrono::Utc>>,
    tpm_indicator: f64,
    context: Option<(u64, u32)>,
    tokens_input: u64,
    tokens_output: u64,
    tokens_cache_create: u64,
    tokens_cache_read: u64,
    web_search_requests: u64,
    usage_limits: Option<&UsageSummary>,
    context_limit_override: Option<u64>,
    prompt_cache: Option<&PromptCacheInfo>,
) -> String {
    let profile = render_profile();
    let term_width = profile.width;
    let tc = is_truecolor_enabled(args);
    let prompt = tokens::ACCENT.paint(SYM_PROMPT, tc);
    let long_labels = matches!(args.labels, LabelsArg::Long);
    let is_claude = is_direct_claude_api(Some(model_id));
    let use_12h = use_12h_time(args);
    let mut segments: Vec<StatusSegment> = Vec::new();

    if !args.no_cost_session {
        let session_label = match term_width {
            TerminalWidth::Narrow => "s:",
            TerminalWidth::Medium => "sess:",
            TerminalWidth::Wide => "session:",
        };
        segments.push(cost_segment_variants(
            session_label,
            "s:",
            session_cost,
            None,
            tc,
            80,
        ));
    }

    if !args.no_cost_today {
        let today_label = match term_width {
            TerminalWidth::Narrow => "t:",
            _ => "today:",
        };
        segments.push(cost_segment_variants(
            today_label,
            "t:",
            today_cost,
            Some(10.0),
            tc,
            30,
        ));
    }

    if is_claude && !args.no_cost_window {
        let window_label = match term_width {
            TerminalWidth::Narrow => "w:",
            TerminalWidth::Medium => "win:",
            TerminalWidth::Wide if long_labels => "window:",
            TerminalWidth::Wide => "win:",
        };
        segments.push(cost_segment_variants(
            window_label,
            "w:",
            total_cost,
            Some(5.0),
            tc,
            40,
        ));
    }

    if is_claude
        && !args.no_usage_five_hour
        && let Some(usage_value) = usage_percent
    {
        let usage_label = match term_width {
            TerminalWidth::Narrow => "u:",
            _ => "usage:",
        };
        if let Some(usage_segment) = render_usage_segment_variants(
            model_id,
            args,
            Some(usage_value),
            projected_percent,
            UsageSegmentTiming {
                remaining_minutes,
                active_block,
                latest_reset,
            },
            usage_limits,
            UsageSegmentLabels {
                long: usage_label,
                short: "u:",
            },
        ) {
            segments.push(usage_segment);
        }

        if let Some(summary) = usage_limits {
            if !args.no_usage_weekly
                && let Some(pct) = summary.seven_day.utilization
            {
                let label = if long_labels { "weekly:" } else { "7d:" };
                let mut text = format!("{}{}", muted_label(label, tc), colorize_percent(pct, args));
                if let Some(reset) = summary.seven_day.resets_at {
                    let local_reset = reset.with_timezone(&Local);
                    let now = Local::now();
                    let hours_until = (reset - now.with_timezone(&chrono::Utc)).num_hours();
                    let reset_fmt = if hours_until < 24 {
                        if use_12h {
                            if local_reset.minute() == 0 {
                                local_reset.format("%-I%p").to_string().to_lowercase()
                            } else {
                                local_reset.format("%-I:%M%p").to_string().to_lowercase()
                            }
                        } else if local_reset.minute() == 0 {
                            local_reset.format("%H:00").to_string()
                        } else {
                            local_reset.format("%H:%M").to_string()
                        }
                    } else {
                        local_reset.format("%a").to_string()
                    };
                    let _ = write!(text, " {}", muted_label(&format!("({reset_fmt})"), tc));
                }
                segments.push(status_segment(text, 15));
            }
            if !args.no_usage_opus
                && let Some(pct) = summary.seven_day_opus.utilization
            {
                segments.push(status_segment(
                    format!(
                        "{}{}",
                        muted_label("opus:", tc),
                        colorize_percent(pct, args)
                    ),
                    14,
                ));
            }
            if !args.no_usage_sonnet
                && let Some(pct) = summary.seven_day_sonnet.utilization
            {
                segments.push(status_segment(
                    format!(
                        "{}{}",
                        muted_label("sonnet:", tc),
                        colorize_percent(pct, args)
                    ),
                    13,
                ));
            }

            if !args.no_usage_extra
                && let Some(ref extra) = summary.extra_usage
                && extra.is_enabled
            {
                let label = if long_labels { "extra:" } else { "ex:" };
                let symbol = extra_usage_symbol(extra.currency.as_deref());
                let spent = extra.used_credits.unwrap_or(0.0);
                let limit = extra.monthly_limit.unwrap_or(0.0);
                let spent_token = if limit > 0.0 {
                    tokens::gradient(spent / limit * 100.0, 100.0)
                } else {
                    tokens::PRIMARY_DIM
                };
                let extra_segment = if limit > 0.0 {
                    format!(
                        "{}{}{}/{}",
                        muted_label(label, tc),
                        tokens::MUTED.paint(&symbol, tc),
                        spent_token.paint(&format!("{:.0}", spent), tc),
                        muted_label(&format!("{:.0}", limit), tc)
                    )
                } else {
                    format!(
                        "{}{}{}",
                        muted_label(label, tc),
                        tokens::MUTED.paint(&symbol, tc),
                        spent_token.paint(&format!("{:.2}", spent), tc)
                    )
                };
                segments.push(status_segment(extra_segment, 12));
            }
        }
    }

    if args.cost_breakdown {
        let ti = format_tokens(tokens_input);
        let to = format_tokens(tokens_output);
        let tcc = format_tokens(tokens_cache_create);
        let tcr = format_tokens(tokens_cache_read);
        segments.push(status_segment(
            format!(
                "{}{} {}{} {}{}",
                muted_label("tok:", tc),
                tokens::PRIMARY_DIM.paint(&format!("{}/{}", ti, to), tc),
                muted_label("cache:", tc),
                tokens::PRIMARY_DIM.paint(&format!("{}/{}", tcc, tcr), tc),
                muted_label("ws:", tc),
                tokens::PRIMARY_DIM.paint(&web_search_requests.to_string(), tc)
            ),
            5,
        ));
    }

    if !args.no_integrations_prompt_cache
        && let Some(info) = prompt_cache
    {
        segments.push(status_segment(render_prompt_cache_segment(info, tc), 50));
    }

    if !args.no_context_tokens || !args.no_context_percent {
        segments.push(render_context_segment_variants(
            model_id,
            model_display_name,
            context,
            context_limit_override,
            args,
            tpm_indicator,
            true,
        ));
    }

    let separator = separator(tc, false);
    fit_status_segments(&prompt, segments, &separator, profile.safe_width)
}

#[allow(clippy::too_many_arguments)]
pub fn print_text_output(
    hook: &HookJson,
    git_info: Option<&GitInfo>,
    args: &Args,
    is_fast_mode: bool,
    session_cost: f64,
    today_cost: f64,
    total_cost: f64,
    usage_percent: Option<f64>,
    projected_percent: Option<f64>,
    remaining_minutes: f64,
    active_block: Option<&Block>,
    latest_reset: Option<DateTime<chrono::Utc>>,
    _tpm: f64,
    tpm_indicator: f64,
    _cost_per_hour: f64,
    context: Option<(u64, u32)>,
    tokens_input: u64,
    tokens_output: u64,
    tokens_cache_create: u64,
    tokens_cache_read: u64,
    _sess_tokens_input: u64,
    _sess_tokens_output: u64,
    _sess_tokens_cache_create: u64,
    _sess_tokens_cache_read: u64,
    web_search_requests: u64,
    _session_cost_per_hour: Option<f64>,
    lines_delta: Option<(i64, i64)>,
    _rate_limit: Option<&RateLimitInfo>,
    usage_limits: Option<&UsageSummary>,
    context_limit_override: Option<u64>,
    cost_provenance: Option<&CostProvenance>,
    prompt_cache: Option<&PromptCacheInfo>,
) {
    let profile = render_profile();
    let mut line = if profile.mode == RenderMode::Compact {
        render_compact_text_output(
            hook,
            git_info,
            args,
            is_fast_mode,
            session_cost,
            usage_percent,
            remaining_minutes,
            active_block,
            latest_reset,
            context,
            lines_delta,
            usage_limits,
            context_limit_override,
        )
    } else {
        let _ = lines_delta;
        render_rich_text_output(
            args,
            &hook.model.id,
            &hook.model.display_name,
            session_cost,
            today_cost,
            total_cost,
            usage_percent,
            projected_percent,
            remaining_minutes,
            active_block,
            latest_reset,
            tpm_indicator,
            context,
            tokens_input,
            tokens_output,
            tokens_cache_create,
            tokens_cache_read,
            web_search_requests,
            usage_limits,
            context_limit_override,
            prompt_cache,
        )
    };

    if args.cost_provenance
        && let Some(provenance) = cost_provenance
    {
        let tc = is_truecolor_enabled(args);
        let compact = profile.mode == RenderMode::Compact;
        let provenance_segment = format!(
            "{}{} {}{} {}{}",
            muted_label("src:", tc),
            tokens::PRIMARY_DIM.paint(provenance.session_cost.as_str(), tc),
            muted_label("today:", tc),
            tokens::PRIMARY_DIM.paint(provenance.today_cost.as_str(), tc),
            muted_label("price:", tc),
            tokens::PRIMARY_DIM.paint(provenance.pricing.as_str(), tc)
        );
        let separator = separator(tc, compact);
        let candidate = format!("{line}{separator}{provenance_segment}");
        if visible_width(&candidate) <= usize::from(profile.safe_width) {
            line = candidate;
        }
    }

    println!("{}", line);
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use clap::Parser;
    use serial_test::serial;

    use crate::models::PromptCacheBucketInfo;
    use crate::models::hook::{
        HookContextWindow, HookCost, HookJson, HookModel, HookThinking, HookWorkspace, OutputStyle,
    };

    fn test_args() -> Args {
        Args::parse_from(["claude_statusline"])
    }

    fn long_args() -> Args {
        Args::parse_from(["claude_statusline", "--labels", "long"])
    }

    fn test_hook(added_dirs: Vec<&str>, git_worktree: Option<&str>) -> HookJson {
        HookJson {
            session_id: "sess-test".to_string(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            model: HookModel {
                id: "claude-sonnet-4-5".to_string(),
                display_name: "Claude Sonnet 4.5".to_string(),
            },
            workspace: HookWorkspace {
                current_dir: "/tmp/project".to_string(),
                project_dir: "/tmp/project".to_string(),
                added_dirs: added_dirs.into_iter().map(str::to_string).collect(),
                git_worktree: git_worktree.map(str::to_string),
                repo: None,
            },
            version: "2.1.157".to_string(),
            output_style: OutputStyle {
                name: "default".to_string(),
            },
            cost: HookCost {
                total_cost_usd: 0.0,
                total_duration_ms: 0,
                total_api_duration_ms: 0,
                total_lines_added: 0,
                total_lines_removed: 0,
            },
            context_window: HookContextWindow {
                total_input_tokens: 0,
                total_output_tokens: 0,
                context_window_size: 200_000,
                current_usage: None,
                used_percentage: 0,
                remaining_percentage: 100,
            },
            exceeds_200k_tokens: false,
            fast_mode: false,
            effort: None,
            thinking: HookThinking { enabled: false },
            rate_limits: None,
            session_name: None,
            vim: None,
            agent: None,
            worktree: None,
            remote: None,
            pr: None,
        }
    }

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(names: &[&'static str]) -> Self {
            Self {
                saved: names
                    .iter()
                    .map(|name| (*name, std::env::var(name).ok()))
                    .collect(),
            }
        }

        fn set(&self, name: &str, value: &str) {
            // SAFETY: env-mutating display tests are marked serial.
            unsafe { std::env::set_var(name, value) };
        }

        fn remove(&self, name: &str) {
            // SAFETY: env-mutating display tests are marked serial.
            unsafe { std::env::remove_var(name) };
        }

        fn force_dimensions(&self, columns: &str, lines: &str) {
            self.remove("CLAUDE_TERMINAL_WIDTH");
            self.set("COLUMNS", columns);
            self.set("LINES", lines);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                // SAFETY: env-mutating display tests are marked serial.
                unsafe {
                    if let Some(value) = value {
                        std::env::set_var(name, value);
                    } else {
                        std::env::remove_var(name);
                    }
                }
            }
        }
    }

    fn terminal_env_guard() -> EnvGuard {
        EnvGuard::new(&[
            "CLAUDE_TERMINAL_WIDTH",
            "COLUMNS",
            "LINES",
            "NO_COLOR",
            "TERM",
            "COLORTERM",
            "CLAUDE_TRUECOLOR",
        ])
    }

    #[test]
    fn render_profile_prefers_compact_for_claude_safe_fit() {
        assert_eq!(
            render_profile_for_dimensions(320, Some(32)).mode,
            RenderMode::Rich
        );
        assert_eq!(
            render_profile_for_dimensions(200, Some(32)).mode,
            RenderMode::Rich
        );
        assert_eq!(
            render_profile_for_dimensions(180, Some(32)).mode,
            RenderMode::Compact
        );
        assert_eq!(
            render_profile_for_dimensions(320, Some(20)).mode,
            RenderMode::Compact
        );
    }

    #[test]
    fn header_is_suppressed_in_compact_mode() {
        assert_eq!(
            render_profile_for_dimensions(180, Some(32)).mode,
            RenderMode::Compact
        );
    }

    #[test]
    #[serial]
    fn detect_terminal_dimensions_uses_statusline_columns_and_lines() {
        let env = terminal_env_guard();
        env.force_dimensions("120", "20");

        assert_eq!(detect_terminal_dimensions(), (120, Some(20)));
        assert_eq!(render_profile().mode, RenderMode::Compact);
    }

    #[test]
    #[serial]
    fn terminal_width_override_wins_over_columns() {
        let env = terminal_env_guard();
        env.force_dimensions("120", "32");
        env.set("CLAUDE_TERMINAL_WIDTH", "320");

        assert_eq!(detect_terminal_dimensions(), (320, Some(32)));
        assert_eq!(render_profile().mode, RenderMode::Rich);
    }

    #[test]
    #[serial]
    fn compact_line_includes_added_dirs_and_hook_worktree() {
        let env = terminal_env_guard();
        env.force_dimensions("320", "32");

        let hook = test_hook(vec!["/tmp/project/docs"], Some("hook-wt"));
        let git_info = GitInfo {
            branch: Some("feature/footer".to_string()),
            short_commit: Some("abc1234".to_string()),
            is_clean: Some(false),
            ahead: Some(1),
            behind: Some(0),
            remote_url: None,
            is_head_on_remote: None,
            worktree_count: Some(2),
            is_linked_worktree: Some(true),
        };

        let line = render_compact_text_output(
            &hook,
            Some(&git_info),
            &test_args(),
            true,
            1.25,
            Some(12.0),
            95.0,
            None,
            None,
            Some((12_345, 6)),
            Some((8, 3)),
            None,
            Some(200_000),
        );

        assert!(!line.contains('\n'));
        assert!(line.contains("dirs:"));
        assert!(line.contains("+1"));
        assert!(line.contains("wt:"));
        assert!(line.contains("hook-wt"));
        assert!(!line.contains("linked"));
        assert!(line.contains("fast"));
    }

    #[test]
    #[serial]
    fn compact_line_fits_safe_width_from_columns() {
        let env = terminal_env_guard();
        env.force_dimensions("120", "32");
        env.set("NO_COLOR", "1");
        env.set("TERM", "dumb");
        env.remove("COLORTERM");
        env.remove("CLAUDE_TRUECOLOR");

        let hook = test_hook(
            vec!["/tmp/project/docs", "/tmp/project/scripts"],
            Some("very-long-worktree-name-that-would-overflow"),
        );
        let git_info = GitInfo {
            branch: Some("feature/very-long-responsive-statusline-branch".to_string()),
            short_commit: Some("abc1234".to_string()),
            is_clean: Some(false),
            ahead: Some(4),
            behind: Some(1),
            remote_url: None,
            is_head_on_remote: None,
            worktree_count: Some(2),
            is_linked_worktree: Some(true),
        };

        let line = render_compact_text_output(
            &hook,
            Some(&git_info),
            &test_args(),
            false,
            1.25,
            Some(12.0),
            95.0,
            None,
            None,
            Some((12_345, 6)),
            Some((8, 3)),
            None,
            Some(200_000),
        );
        let profile = render_profile();
        let plain = strip_ansi(&line);

        assert_eq!(profile.mode, RenderMode::Compact);
        assert!(!line.contains('\n'));
        assert!(
            visible_width(&line) <= usize::from(profile.safe_width),
            "{} > {}: {}",
            visible_width(&line),
            profile.safe_width,
            line
        );
        assert!(plain.contains("project"));
        assert!(plain.contains("Sonnet"));
        assert!(!plain.contains("S4"));
        assert!(plain.contains("$1.25"));
        assert!(plain.contains("u:") || plain.contains("12%"));
        assert!(plain.contains("ctx:") || plain.contains("6%"));
        assert!(!plain.contains("dirs:"));
        assert!(!plain.contains("very-long-worktree"));
    }

    #[test]
    fn extra_usage_symbol_maps_currencies() {
        assert_eq!(extra_usage_symbol(None), "$");
        assert_eq!(extra_usage_symbol(Some("USD")), "$");
        assert_eq!(extra_usage_symbol(Some("EUR")), "€");
        assert_eq!(extra_usage_symbol(Some("CHF")), "CHF ");
    }

    #[test]
    fn model_labels_recognize_fable_and_mythos() {
        assert_eq!(
            model_family_label("claude-fable-5", "Fable 5"),
            Some("Fable")
        );
        assert_eq!(
            model_family_label("claude-mythos-5", "Mythos 5"),
            Some("Mythos")
        );
        assert_eq!(tiny_model_label("claude-fable-5[1m]", "Fable 5"), "Fable");
        assert_eq!(compact_model_label("claude-fable-5", "Fable 5"), "Fable 5");
        assert_eq!(
            compact_model_label("claude-mythos-5", "Claude Mythos 5"),
            "Mythos 5"
        );
    }

    #[test]
    #[serial]
    fn compact_line_keeps_family_name_at_tiny_width() {
        let env = terminal_env_guard();
        env.force_dimensions("80", "32");
        env.set("NO_COLOR", "1");
        env.set("TERM", "dumb");
        env.remove("COLORTERM");
        env.remove("CLAUDE_TRUECOLOR");

        let hook = test_hook(vec![], None);
        let line = render_compact_text_output(
            &hook,
            None,
            &test_args(),
            false,
            1.25,
            Some(12.0),
            95.0,
            None,
            None,
            Some((12_345, 6)),
            None,
            None,
            Some(200_000),
        );
        let profile = render_profile();
        let plain = strip_ansi(&line);

        assert_eq!(profile.mode, RenderMode::Compact);
        assert!(
            visible_width(&line) <= usize::from(profile.safe_width),
            "{} > {}: {}",
            visible_width(&line),
            profile.safe_width,
            line
        );
        assert!(plain.contains("Sonnet"));
        assert!(!plain.contains("S4"));
    }

    #[test]
    #[serial]
    fn compact_line_prefers_repo_name_for_workspace_identity() {
        let env = terminal_env_guard();
        env.force_dimensions("180", "32");
        env.set("NO_COLOR", "1");
        env.set("TERM", "dumb");
        env.remove("COLORTERM");
        env.remove("CLAUDE_TRUECOLOR");

        let mut hook = test_hook(vec![], None);
        hook.workspace.current_dir = "/tmp/local-folder/src".to_string();
        hook.workspace.project_dir = "/tmp/local-folder".to_string();
        hook.workspace.repo = Some(crate::models::hook::HookRepo {
            host: "github.com".to_string(),
            owner: "cam".to_string(),
            name: "origin-repo".to_string(),
        });

        let line = render_compact_text_output(
            &hook,
            None,
            &test_args(),
            false,
            1.25,
            Some(12.0),
            95.0,
            None,
            None,
            Some((12_345, 6)),
            None,
            None,
            Some(200_000),
        );
        let plain = strip_ansi(&line);

        assert!(plain.contains("origin-repo"));
        assert!(!plain.contains("local-folder"));
    }

    #[test]
    #[serial]
    fn rich_header_uses_workspace_segments() {
        let env = terminal_env_guard();
        env.force_dimensions("320", "32");

        let hook = test_hook(
            vec!["/tmp/project/docs", "/tmp/project/scripts"],
            Some("hook-wt"),
        );
        let git_info = GitInfo {
            branch: Some("feature/footer".to_string()),
            short_commit: Some("abc1234".to_string()),
            is_clean: Some(true),
            ahead: Some(0),
            behind: Some(0),
            remote_url: None,
            is_head_on_remote: None,
            worktree_count: Some(2),
            is_linked_worktree: Some(true),
        };

        let line = render_header_line(
            &hook,
            Some(&git_info),
            &long_args(),
            None,
            None,
            None,
            None,
            Some(200_000),
            false,
        )
        .unwrap_or_default();

        assert!(line.contains("dirs:"));
        assert!(line.contains("+2"));
        assert!(line.contains("wt:"));
        assert!(line.contains("hook-wt"));
    }

    #[test]
    #[serial]
    fn short_rich_header_hides_redundant_workspace_noise() {
        let env = terminal_env_guard();
        env.force_dimensions("320", "32");

        let mut hook = test_hook(vec!["/tmp/project"], Some("topic+sample-worktree"));
        hook.workspace.current_dir =
            "/tmp/project/.claude/worktrees/topic+sample-worktree".to_string();

        let git_info = GitInfo {
            branch: Some("topic/sample-worktree".to_string()),
            short_commit: Some("abc1234".to_string()),
            is_clean: Some(false),
            ahead: Some(3),
            behind: Some(0),
            remote_url: None,
            is_head_on_remote: None,
            worktree_count: Some(2),
            is_linked_worktree: Some(true),
        };

        let line = render_header_line(
            &hook,
            Some(&git_info),
            &test_args(),
            None,
            Some((12, 4)),
            None,
            None,
            Some(200_000),
            false,
        )
        .unwrap_or_default();

        assert!(line.contains(".claude/worktrees"));
        assert!(!line.contains("wt:"));
        assert!(!line.contains("dirs:"));
    }

    #[test]
    #[serial]
    fn rich_usage_row_keeps_existing_detail_by_default() {
        let env = terminal_env_guard();
        env.force_dimensions("320", "32");

        let summary = UsageSummary {
            seven_day: UsageLimit {
                utilization: Some(22.0),
                ..UsageLimit::default()
            },
            seven_day_sonnet: UsageLimit {
                utilization: Some(0.0),
                ..UsageLimit::default()
            },
            extra_usage: Some(crate::usage_api::ExtraUsage {
                is_enabled: true,
                monthly_limit: Some(60.0),
                used_credits: Some(17.0),
                utilization: Some(28.3),
                ..Default::default()
            }),
            ..UsageSummary::default()
        };

        let line = render_rich_text_output(
            &test_args(),
            "claude-opus-4-7",
            "Opus 4.7",
            3.0,
            11.99,
            11.99,
            Some(2.0),
            None,
            274.0,
            None,
            None,
            0.0,
            Some((133_800, 13)),
            0,
            0,
            0,
            0,
            0,
            Some(&summary),
            Some(1_000_000),
            None,
        );

        assert!(line.contains("session:"));
        assert!(line.contains("today:"));
        assert!(line.contains("win:"));
        assert!(line.contains("7d:"));
        assert!(line.contains("sonnet:"));
        assert!(line.contains("ex:"));
        assert!(line.contains("context:"));
        assert!(line.contains("1M"));
        assert!(line.contains("13%"));
    }

    #[test]
    fn prompt_cache_segment_shows_read_write_tokens_for_same_turn_activity() {
        let write_ts = chrono::Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        let info = PromptCacheInfo {
            buckets: vec![PromptCacheBucketInfo {
                kind: PromptCacheBucketKind::OneHour,
                created_at: write_ts,
                ttl_seconds: 3600,
                input_tokens: 2000,
            }],
            last_cache_write_at: Some(write_ts),
            last_cache_read_at: Some(write_ts),
            cache_write_input_tokens: 2000,
            cache_read_input_tokens: 1800,
            now: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 42).unwrap(),
        };

        let segment = render_prompt_cache_segment(&info, false);

        assert!(segment.contains("1h"));
        assert!(segment.contains("r:1.8K"));
        assert!(segment.contains("w:2K"));
        assert!(!segment.contains("59m"));
        assert!(!segment.contains("made:"));
        assert!(!segment.contains("hit:"));
        assert!(!segment.contains("age:"));
    }

    #[test]
    fn prompt_cache_segment_shows_latest_read_tokens_for_later_reads() {
        let write_ts = chrono::Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        let read_ts = chrono::Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 30).unwrap();
        let info = PromptCacheInfo {
            buckets: vec![PromptCacheBucketInfo {
                kind: PromptCacheBucketKind::OneHour,
                created_at: write_ts,
                ttl_seconds: 3600,
                input_tokens: 2000,
            }],
            last_cache_write_at: Some(write_ts),
            last_cache_read_at: Some(read_ts),
            cache_write_input_tokens: 2000,
            cache_read_input_tokens: 1800,
            now: chrono::Utc.with_ymd_and_hms(2026, 5, 1, 12, 1, 0).unwrap(),
        };

        let segment = render_prompt_cache_segment(&info, false);

        assert!(segment.contains("1h"));
        assert!(segment.contains("r:1.8K"));
        assert!(!segment.contains("w:2K"));
        assert!(!segment.contains("59m"));
        assert!(!segment.contains("made:"));
        assert!(!segment.contains("hit:"));
        assert!(!segment.contains("age:"));
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_json_output(
    hook: &HookJson,
    session_cost: f64,
    today_cost: f64,
    sessions_count: usize,
    total_cost: f64,
    total_tokens: f64,
    noncache_tokens: f64,
    tokens_input: u64,
    tokens_output: u64,
    tokens_cache_create: u64,
    tokens_cache_read: u64,
    // session-scoped tokens
    sess_tokens_input: u64,
    sess_tokens_output: u64,
    sess_tokens_cache_create: u64,
    sess_tokens_cache_read: u64,
    web_search_requests: u64,
    service_tier: Option<String>,
    usage_percent: Option<f64>,
    projected_percent: Option<f64>,
    remaining_minutes: f64,
    active_block: Option<&Block>,
    latest_reset: Option<DateTime<chrono::Utc>>,
    tpm: f64,
    tpm_indicator: f64,
    session_nc_tpm: f64,
    global_nc_tpm: f64,
    cost_per_hour: f64,
    context: Option<(u64, u32)>,
    context_source: Option<&'static str>,
    api_key_source: Option<String>,
    git_info: Option<GitInfo>,
    rate_limit: Option<&RateLimitInfo>,
    oauth_org_type: Option<String>,
    oauth_rate_tier: Option<String>,
    usage_limits: Option<&UsageSummary>,
    // Override context limit from hook.context_window.context_window_size
    context_limit_override: Option<u64>,
    // Beads issue tracker info
    beads_info: Option<&BeadsInfo>,
    // Gas Town multi-agent info
    gastown_info: Option<&GasTownInfo>,
    // Fast mode detected from transcript
    is_fast_mode: bool,
    // Per-subagent cost breakdown (computed from entries with agent_id)
    subagent_breakdown: Option<serde_json::Value>,
    cost_provenance: Option<&CostProvenance>,
    prompt_cache: Option<&PromptCacheInfo>,
) -> serde_json::Value {
    // Provider from env or deduced from model id
    let provider_env = env::var("CLAUDE_PROVIDER").ok().map(|s| {
        if s.eq_ignore_ascii_case("firstParty") {
            "anthropic".to_string()
        } else {
            s
        }
    });
    let provider_final = provider_env
        .clone()
        .unwrap_or_else(|| deduce_provider_from_model(&hook.model.id).to_string());

    let reset_iso = latest_reset.map(|d| d.to_rfc3339());
    // The modern hook schema ships an authoritative fast_mode flag;
    // is_fast_mode (transcript-derived) is retained as a defensive OR for any
    // mid-turn divergence between transcript signals and the hook snapshot.
    let fast_mode = hook.fast_mode || is_fast_mode;
    let effort = active_effort_level(hook);
    let (ctx_tokens, ctx_pct) = context
        .map(|(t, p)| (Some(t), Some(p)))
        .unwrap_or((None, None));
    // Use hook-provided context limit if available, otherwise fall back to model detection
    let ctx_limit = context_limit_override.unwrap_or_else(|| {
        context_limit_for_model_display(&hook.model.id, &hook.model.display_name)
    });
    let overhead_value = if context_source == Some("hook") {
        0
    } else {
        system_overhead_tokens()
    };
    let overhead_display = if ctx_tokens.is_some() && overhead_value > 0 {
        Some(overhead_value)
    } else {
        None
    };
    let ctx_tokens_raw = ctx_tokens.map(|t| t.saturating_sub(overhead_value));
    let ctx_usable_limit =
        ctx_limit.saturating_sub(reserved_output_tokens_for_model(&hook.model.id));
    let ctx_usable_percent = ctx_tokens.and_then(|tokens| {
        (ctx_usable_limit > 0)
            .then(|| ((tokens as f64 / ctx_usable_limit as f64) * 100.0).round() as u32)
    });
    let auto_compact_buffer = auto_compact_enabled().then(auto_compact_headroom_tokens);
    // Optional headroom and ETA for consumers
    let (context_headroom, context_eta_minutes) = if let Some(toks) = ctx_tokens {
        let head = (ctx_limit as i64 - toks as i64).max(0) as u64;
        let eta = if tpm_indicator > 0.0 && head > 0 {
            Some(((head as f64) / tpm_indicator).round() as i64)
        } else {
            None
        };
        (Some(head), eta)
    } else {
        (None, None)
    };

    // Git json fields (present even if nulls to keep schema stable)
    let (
        git_branch,
        git_short,
        git_clean,
        git_ahead,
        git_behind,
        git_on_remote,
        git_remote_url,
        git_wt_count,
        git_is_wt,
    ) = if let Some(gi) = git_info {
        (
            gi.branch,
            gi.short_commit,
            gi.is_clean,
            gi.ahead,
            gi.behind,
            gi.is_head_on_remote,
            gi.remote_url,
            gi.worktree_count,
            gi.is_linked_worktree,
        )
    } else {
        (None, None, None, None, None, None, None, None, None)
    };

    let block_json = serde_json::json!({
        "cost_usd": (total_cost * 100.0).round() / 100.0,
        "total_tokens": (total_tokens as u64),
        "noncache_tokens": (noncache_tokens as u64),
        "input_tokens": tokens_input,
        "output_tokens": tokens_output,
        "cache_creation_input_tokens": tokens_cache_create,
        "cache_read_input_tokens": tokens_cache_read,
        "web_search_requests": web_search_requests,
        "service_tier": service_tier,
        "start": active_block.map(|b| b.start.to_rfc3339()),
        "end": active_block.map(|b| b.end.to_rfc3339()),
        "end_epoch": active_block.map(|b| b.end.timestamp()),
        "reset_anchor_epoch": active_block.map(|b| b.start.timestamp()),
        "remaining_minutes": (remaining_minutes as i64).max(0),
        "usage_percent": usage_percent.map(|v| (v * 10.0).round()/10.0),
        "usage_percent_left": usage_percent.map(|v| ((100.0 - v).max(0.0) * 10.0).round()/10.0),
        "usage_stale": usage_limits.is_some_and(|s| s.stale),
        "projected_percent": projected_percent.map(|v| (v * 10.0).round()/10.0),
        "projected_percent_left": projected_percent.map(|v| ((100.0 - v).max(0.0) * 10.0).round()/10.0),
        "tokens_per_minute": (tpm * 10.0).round()/10.0,
        "tokens_per_minute_indicator": (tpm_indicator * 10.0).round()/10.0,
        "tokens_per_minute_noncache_session": (session_nc_tpm * 10.0).round()/10.0,
        "tokens_per_minute_noncache_global": (global_nc_tpm * 10.0).round()/10.0,
        "cost_per_hour": (cost_per_hour * 100.0).round()/100.0,
    });

    // The modern hook schema ships these aggregate session fields.
    let cost = &hook.cost;
    let sess_duration_ms = cost.total_duration_ms;
    let sess_api_ms = cost.total_api_duration_ms;
    let sess_lines_added = cost.total_lines_added;
    let sess_lines_removed = cost.total_lines_removed;
    let sess_cph_json = if sess_duration_ms > 0 {
        let hrs = (sess_duration_ms as f64) / 3_600_000.0;
        Some(((session_cost / hrs) * 100.0).round() / 100.0)
    } else {
        None
    };

    let usage_limits_value = usage_limits.map(|summary| {
        serde_json::json!({
            "five_hour": usage_limit_json(&summary.window),
            "seven_day": usage_limit_json(&summary.seven_day),
            "seven_day_opus": usage_limit_json(&summary.seven_day_opus),
            "seven_day_sonnet": usage_limit_json(&summary.seven_day_sonnet),
            "seven_day_oauth_apps": usage_limit_json(&summary.seven_day_oauth_apps),
            "seven_day_cowork": usage_limit_json(&summary.seven_day_cowork),
            "cinder_cove": usage_limit_json(&summary.cinder_cove),
            "extra_usage": summary.extra_usage.as_ref().map(|e| serde_json::json!({
                "is_enabled": e.is_enabled,
                "monthly_limit": e.monthly_limit,
                "used_credits": e.used_credits,
                "utilization": e.utilization,
                "currency": e.currency,
                "disabled_reason": e.disabled_reason
            }))
        })
    });

    let mut json = serde_json::json!({
        "model": {
            "id": hook.model.id.clone(),
            "display_name": hook.model.display_name.clone(),
            "fast_mode": fast_mode,
        },
        "workspace": {
            "current_dir": hook.workspace.current_dir.clone(),
            "project_dir": hook.workspace.project_dir.clone(),
            "added_dirs": hook.workspace.added_dirs.clone(),
            "git_worktree": hook.workspace.git_worktree.clone(),
            "repo": hook.workspace.repo.as_ref().map(|repo| serde_json::json!({
                "host": repo.host.clone(),
                "owner": repo.owner.clone(),
                "name": repo.name.clone()
            })),
        },
        "version": hook.version.clone(),
        "output_style": {"name": hook.output_style.name.clone()},
        "effort": effort,
        "thinking": {"enabled": hook.thinking.enabled},
        "provider": {"apiKeySource": api_key_source, "env": provider_final},
        "oauth_profile": {
            "organization_type": oauth_org_type,
            "rate_limit_tier": oauth_rate_tier
        },
        "reset_at": reset_iso,
        "session": {
            "cost_usd": (session_cost * 100.0).round() / 100.0,
            "cost_source": cost_provenance.map(|p| p.session_cost.as_str()),
            "duration_ms": sess_duration_ms,
            "api_duration_ms": sess_api_ms,
            "lines_added": sess_lines_added,
            "lines_removed": sess_lines_removed,
            "cost_per_hour": sess_cph_json,
            "tokens": {
                "input_tokens": sess_tokens_input,
                "output_tokens": sess_tokens_output,
                "cache_creation_input_tokens": sess_tokens_cache_create,
                "cache_read_input_tokens": sess_tokens_cache_read,
                "total_tokens": (sess_tokens_input + sess_tokens_output + sess_tokens_cache_create + sess_tokens_cache_read)
            }
        },
        "today": {
            "cost_usd": (today_cost * 100.0).round() / 100.0,
            "cost_source": cost_provenance.map(|p| p.today_cost.as_str()),
            "sessions_count": sessions_count
        },
        "window": block_json,
        "context": {
            "tokens": ctx_tokens,
            "tokens_raw": ctx_tokens_raw,
            "system_overhead_tokens": overhead_display,
            "percent": ctx_pct,
            "limit": ctx_limit,
            "limit_full": ctx_limit, // Same as limit, uses hook override when available
            "usable_limit": ctx_usable_limit,
            "usable_percent": ctx_usable_percent,
            "auto_compact_buffer_tokens": auto_compact_buffer,
            "output_reserve": reserved_output_tokens_for_model(&hook.model.id),
            "output_reserve_used": ctx_tokens.map(|t| t.saturating_sub(ctx_usable_limit)),
            "source": context_source,
            "headroom_tokens": context_headroom,
            "eta_minutes": context_eta_minutes
        },
        "prompt_cache": prompt_cache_json(prompt_cache),
        "provenance": {
            "session_cost": cost_provenance.map(|p| p.session_cost.as_str()),
            "today_cost": cost_provenance.map(|p| p.today_cost.as_str()),
            "pricing": cost_provenance.map(|p| p.pricing.as_str()),
            "context": context_source
        },
        "usage_limits": usage_limits_value,
        "rate_limit": rate_limit.as_ref().map(|rl| serde_json::json!({
            "status": rl.status,
            "resets_at": rl.resets_at.map(|d| d.to_rfc3339()),
            "fallback_available": rl.fallback_available,
            "fallback_percentage": rl.fallback_percentage,
            "rate_limit_type": rl.rate_limit_type,
            "overage_status": rl.overage_status,
            "overage_resets_at": rl.overage_resets_at.map(|d| d.to_rfc3339()),
            "is_using_overage": rl.is_using_overage,
        })),
        "git": {
            "branch": git_branch,
            "short_commit": git_short,
            "is_clean": git_clean,
            "ahead": git_ahead,
            "behind": git_behind,
            "is_head_on_remote": git_on_remote,
            "remote_url": git_remote_url,
            "worktree_count": git_wt_count,
            "is_linked_worktree": git_is_wt
        },
        "session_name": hook.session_name.clone(),
        "exceeds_200k_tokens": hook.exceeds_200k_tokens,
        "vim": hook.vim.as_ref().map(|v| serde_json::json!({"mode": v.mode.clone()})),
        "remote": hook.remote.as_ref().map(|remote| serde_json::json!({
            "session_id": remote.session_id.clone()
        })),
        "pr": hook.pr.as_ref().map(|pr| serde_json::json!({
            "number": pr.number,
            "url": pr.url.clone(),
            "review_state": pr.review_state.clone()
        })),
        "agent": hook.agent.as_ref().map(|a| serde_json::json!({
            "name": a.name.clone(),
            "type": a.agent_type.clone()
        })),
        "worktree": hook.worktree.as_ref().map(|w| serde_json::json!({
            "name": w.name.clone(),
            "path": w.path.clone(),
            "branch": w.branch.clone(),
            "original_cwd": w.original_cwd.clone(),
            "original_branch": w.original_branch.clone()
        })),
        "beads": beads_info.map(|b| serde_json::json!({
            "beads_dir": b.beads_dir.clone(),
            "current_work": b.current_work.as_ref().map(|w| serde_json::json!({
                "id": w.id.clone(),
                "title": w.title.clone(),
                "status": w.status.as_str(),
                "priority": w.priority,
                "issue_type": w.issue_type.clone(),
                "assignee": w.assignee.clone(),
                "estimated_minutes": w.estimated_minutes
            })),
            "counts": {
                "open": b.counts.open,
                "in_progress": b.counts.in_progress,
                "blocked": b.counts.blocked,
                "hooked": b.counts.hooked,
                "deferred": b.counts.deferred,
                "pinned": b.counts.pinned
            },
            "priorities": {
                "p0_critical": b.priorities.p0_critical,
                "p1_high": b.priorities.p1_high,
                "p2_medium": b.priorities.p2_medium,
                "p3_p4_low": b.priorities.p3_p4_low
            },
            "types": {
                "task": b.types.task,
                "bug": b.types.bug,
                "feature": b.types.feature,
                "epic": b.types.epic,
                "other": b.types.other
            },
            "total_open": b.total_open,
            "epic_count": b.epic_count,
            "top_labels": b.top_labels.clone()
        })),
        "gastown": gastown_info.map(|gt| serde_json::json!({
            "town_root": gt.town_root.clone(),
            "town_name": gt.town_name.clone(),
            "agent": gt.agent.as_ref().map(|a| serde_json::json!({
                "type": a.agent_type.as_str(),
                "emoji": a.agent_type.emoji(),
                "rig": a.rig.clone(),
                "name": a.name.clone(),
                "identity": a.identity.clone()
            })),
            "mail": gt.mail.as_ref().map(|m| serde_json::json!({
                "unread_count": m.unread_count,
                "preview": m.preview.clone()
            })),
            "hooked_issue": gt.hooked_issue.clone(),
            "rigs": gt.rigs.iter().map(|r| serde_json::json!({
                "name": r.name.clone(),
                "status": match r.status {
                    crate::models::RigStatus::Active => "active",
                    crate::models::RigStatus::Partial => "partial",
                    crate::models::RigStatus::Inactive => "inactive",
                },
                "led": r.status.led(),
                "polecat_count": r.polecat_count,
                "crew_count": r.crew_count,
                "has_witness": r.has_witness,
                "has_refinery": r.has_refinery
            })).collect::<Vec<_>>(),
            "total_polecats": gt.total_polecats,
            "refinery_queue": gt.refinery_queue.as_ref().map(|q| serde_json::json!({
                "current": q.current.clone(),
                "pending": q.pending
            }))
        }))
    });

    // Inject subagent cost breakdown into the session object if available
    if let Some(breakdown) = subagent_breakdown {
        if let Some(session) = json.get_mut("session") {
            session["subagents"] = breakdown;
        }
    }

    json
}

/// Remove JSON fields gated by `--no-json-*` toggles. Runs after `build_json_output`
/// so the omission policy is enforced in one place.
fn apply_json_toggles(json: &mut serde_json::Value, args: &Args) {
    let Some(obj) = json.as_object_mut() else {
        return;
    };
    if args.no_json_rate_limit {
        obj.remove("rate_limit");
    }
    if args.no_json_usage_limits {
        obj.remove("usage_limits");
    }
    if args.no_json_subagents {
        if let Some(session) = obj.get_mut("session").and_then(|v| v.as_object_mut()) {
            session.remove("subagents");
        }
    }
    if args.no_json_duration {
        if let Some(session) = obj.get_mut("session").and_then(|v| v.as_object_mut()) {
            session.remove("duration_ms");
            session.remove("api_duration_ms");
            session.remove("cost_per_hour");
            session.remove("lines_added");
            session.remove("lines_removed");
        }
    }
    if args.no_json_tokens_breakdown {
        if let Some(session) = obj.get_mut("session").and_then(|v| v.as_object_mut()) {
            session.remove("tokens");
        }
        if let Some(window) = obj.get_mut("window").and_then(|v| v.as_object_mut()) {
            window.remove("input_tokens");
            window.remove("output_tokens");
            window.remove("cache_creation_input_tokens");
            window.remove("cache_read_input_tokens");
        }
    }
}
#[allow(clippy::too_many_arguments)]
pub fn print_json_output(
    args: &Args,
    hook: &HookJson,
    session_cost: f64,
    today_cost: f64,
    sessions_count: usize,
    total_cost: f64,
    total_tokens: f64,
    noncache_tokens: f64,
    tokens_input: u64,
    tokens_output: u64,
    tokens_cache_create: u64,
    tokens_cache_read: u64,
    // session-scoped
    sess_tokens_input: u64,
    sess_tokens_output: u64,
    sess_tokens_cache_create: u64,
    sess_tokens_cache_read: u64,
    web_search_requests: u64,
    service_tier: Option<String>,
    usage_percent: Option<f64>,
    projected_percent: Option<f64>,
    remaining_minutes: f64,
    active_block: Option<&Block>,
    latest_reset: Option<DateTime<chrono::Utc>>,
    tpm: f64,
    tpm_indicator: f64,
    session_nc_tpm: f64,
    global_nc_tpm: f64,
    cost_per_hour: f64,
    context: Option<(u64, u32)>,
    context_source: Option<&'static str>,
    api_key_source: Option<String>,
    git_info: Option<GitInfo>,
    rate_limit: Option<&RateLimitInfo>,
    oauth_org_type: Option<String>,
    oauth_rate_tier: Option<String>,
    usage_limits: Option<&UsageSummary>,
    context_limit_override: Option<u64>,
    beads_info: Option<&BeadsInfo>,
    gastown_info: Option<&GasTownInfo>,
    is_fast_mode: bool,
    subagent_breakdown: Option<serde_json::Value>,
    cost_provenance: Option<&CostProvenance>,
    prompt_cache: Option<&PromptCacheInfo>,
) -> anyhow::Result<()> {
    let mut json = build_json_output(
        hook,
        session_cost,
        today_cost,
        sessions_count,
        total_cost,
        total_tokens,
        noncache_tokens,
        tokens_input,
        tokens_output,
        tokens_cache_create,
        tokens_cache_read,
        sess_tokens_input,
        sess_tokens_output,
        sess_tokens_cache_create,
        sess_tokens_cache_read,
        web_search_requests,
        service_tier,
        usage_percent,
        projected_percent,
        remaining_minutes,
        active_block,
        latest_reset,
        tpm,
        tpm_indicator,
        session_nc_tpm,
        global_nc_tpm,
        cost_per_hour,
        context,
        context_source,
        api_key_source,
        git_info,
        rate_limit,
        oauth_org_type,
        oauth_rate_tier,
        usage_limits,
        context_limit_override,
        beads_info,
        gastown_info,
        is_fast_mode,
        subagent_breakdown,
        cost_provenance,
        prompt_cache,
    );
    apply_json_toggles(&mut json, args);
    println!("{}", serde_json::to_string(&json)?);
    Ok(())
}
