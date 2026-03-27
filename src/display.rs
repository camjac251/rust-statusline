use chrono::{DateTime, Local, Timelike};

use crate::beads::format_bead_display;
use crate::gastown::format_gastown_display;
use crate::models::{BeadsInfo, GasTownInfo};
use crate::tokens;
use crate::usage_api::is_direct_claude_api;
use std::env;

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
    context_limit_for_model_display, deduce_provider_from_model, format_currency, format_path,
    format_tokens, reserved_output_tokens_for_model, system_overhead_tokens,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalWidth {
    Narrow, // < 140 cols
    Medium, // 140-200 cols
    Wide,   // > 200 cols
}

fn get_terminal_width() -> TerminalWidth {
    // Check for override via env var (useful for testing)
    if let Ok(override_width) = env::var("CLAUDE_TERMINAL_WIDTH")
        && let Ok(width) = override_width.parse::<u16>()
    {
        let effective_width = width.saturating_sub(TERMINAL_MARGIN);
        return if effective_width < WIDTH_NARROW {
            TerminalWidth::Narrow
        } else if effective_width < WIDTH_MEDIUM {
            TerminalWidth::Medium
        } else {
            TerminalWidth::Wide
        };
    }

    // Detect actual terminal width and subtract margin for CLI padding
    if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
        let effective_width = w.saturating_sub(TERMINAL_MARGIN);
        if effective_width < WIDTH_NARROW {
            TerminalWidth::Narrow
        } else if effective_width < WIDTH_MEDIUM {
            TerminalWidth::Medium
        } else {
            TerminalWidth::Wide
        }
    } else {
        // Fallback to medium if detection fails
        TerminalWidth::Medium
    }
}

pub fn model_colored_name(model_id: &str, display: &str, args: &Args) -> String {
    // Respect NO_COLOR if set: return plain string
    if env::var("NO_COLOR").is_ok() {
        return display.to_string();
    }
    let lower_id = model_id.to_lowercase();
    let lower_disp = display.to_lowercase();
    let tc = is_truecolor_enabled(args);

    // Opus family (and Claude 2 legacy) -> Purple
    let token = if lower_id.contains("opus")
        || lower_disp.contains("opus")
        || lower_id.contains("claude-2")
    {
        tokens::MODEL_OPUS
    }
    // Sonnet family -> Amber/Yellow
    else if lower_id.contains("sonnet") || lower_disp.contains("sonnet") {
        tokens::MODEL_SONNET
    }
    // Haiku family (and Instant legacy) -> Cyan/Blue
    else if lower_id.contains("haiku")
        || lower_disp.contains("haiku")
        || lower_id.contains("claude-instant")
    {
        tokens::MODEL_HAIKU
    } else {
        // Unknown/Other -> White
        tokens::PRIMARY
    };
    token.paint(display, tc)
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
    let dir_fmt = format_path(&hook.workspace.current_dir);
    let tc = is_truecolor_enabled(args);

    // Determine effective context limit and build model display
    let effective_limit = context_limit_override.unwrap_or_else(|| {
        context_limit_for_model_display(&hook.model.id, &hook.model.display_name)
    });
    let display_lower = hook.model.display_name.to_lowercase();
    let already_shows_context = display_lower.contains("1m") || display_lower.contains("200k");

    let mdisp = if effective_limit >= 1_000_000 && !already_shows_context {
        // 1M context active but display name doesn't mention it -- append indicator
        let base = model_colored_name(&hook.model.id, &hook.model.display_name, args);
        format!("{} {}", base, tokens::MUTED.dim("1M", tc))
    } else if effective_limit < 1_000_000 && display_lower.contains("1m") {
        // Display name says 1M but effective limit is lower -- strip the misleading text
        let cleaned = hook
            .model
            .display_name
            .replace(" (with 1M context)", "")
            .replace(" [1m]", "")
            .replace("[1m]", "");
        model_colored_name(&hook.model.id, cleaned.trim(), args)
    } else {
        model_colored_name(&hook.model.id, &hook.model.display_name, args)
    };

    // Build header segments: git (minimal) + model + beads + output_style + optional provider hints
    let mut header_parts: Vec<String> = Vec::new();

    // Helper for bracket styling
    let bracket = |open: bool| -> String {
        let ch = if open { "[" } else { "]" };
        tokens::MUTED.paint(ch, tc)
    };

    // Git info from project_dir or current_dir
    if let Some(gi) = git_info {
        let mut git_seg = String::new();
        // worktree indicator
        if gi.is_linked_worktree == Some(true) {
            git_seg.push_str(&muted_label("wt ", tc));
        }
        if let (Some(br), Some(sc)) = (gi.branch.as_ref(), gi.short_commit.as_ref()) {
            // branch and short sha
            git_seg.push_str(&tokens::PRIMARY.paint(br, tc));
            git_seg.push_str(&muted_label("@", tc));
            git_seg.push_str(&tokens::PRIMARY.paint(sc, tc));
        } else if let Some(sc) = gi.short_commit.as_ref() {
            git_seg.push_str(&muted_label("detached@", tc));
            git_seg.push_str(&tokens::PRIMARY.paint(sc, tc));
        }
        // dirty marker
        if gi.is_clean == Some(false) {
            git_seg.push_str(&tokens::WARNING.paint("*", tc));
        }
        // ahead/behind
        if let (Some(a), Some(b)) = (gi.ahead, gi.behind) {
            if a > 0 {
                git_seg.push(' ');
                git_seg.push_str(&tokens::SUCCESS.paint(&format!("{}{}", SYM_ARROW_UP, a), tc));
            }
            if b > 0 {
                if a == 0 {
                    git_seg.push(' ');
                }
                git_seg.push_str(&tokens::ERROR.paint(&format!("{}{}", SYM_ARROW_DOWN, b), tc));
            }
        }
        // lines delta (working tree changes)
        if let Some((added, removed)) = lines_delta {
            if added != 0 || removed != 0 {
                if !git_seg.is_empty() {
                    git_seg.push(' ');
                }
                git_seg.push_str(&tokens::SUCCESS.paint(&format!("+{}", added), tc));
                git_seg.push_str(&tokens::ERROR.paint(&format!("-{}", removed.abs()), tc));
            }
        }
        if !git_seg.is_empty() {
            header_parts.push(format!("{}{}{}", bracket(true), git_seg, bracket(false)));
        }
    }

    // Model segment (with fast mode indicator)
    let model_seg = if is_fast_mode {
        let fast_label = tokens::WARNING.bold("fast", tc);
        format!("{} {}", mdisp, fast_label)
    } else {
        mdisp
    };
    header_parts.push(format!("{}{}{}", bracket(true), model_seg, bracket(false)));

    // Beads current work segment (if available)
    if let Some(beads) = beads_info {
        if let Some(ref work) = beads.current_work {
            // Max display length depends on terminal width
            let term_width = get_terminal_width();
            let max_len = match term_width {
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

            header_parts.push(format!(
                "{}{}{}",
                bracket(true),
                work_colored,
                bracket(false)
            ));
        } else if beads.total_open > 0 {
            // No current work but there are open issues - show count
            let count_text = format!("{} open", beads.total_open);
            let count_colored = tokens::MUTED.dim(&count_text, tc);
            header_parts.push(format!(
                "{}{}{}{}",
                bracket(true),
                muted_label("bd:", tc),
                count_colored,
                bracket(false)
            ));
        }

        // Show alerts for blocked and P0 issues (separate segment)
        let mut alerts: Vec<String> = Vec::new();

        // P0 critical issues alert
        if beads.priorities.p0_critical > 0 {
            let p0_text = format!("🔴{}", beads.priorities.p0_critical);
            alerts.push(tokens::ERROR.bold(&p0_text, tc));
        }

        // Blocked issues alert
        if beads.counts.blocked > 0 {
            let blocked_text = format!("⚠{}", beads.counts.blocked);
            alerts.push(tokens::WARNING.paint(&blocked_text, tc));
        }

        if !alerts.is_empty() {
            header_parts.push(format!(
                "{}{}{}",
                bracket(true),
                alerts.join(" "),
                bracket(false)
            ));
        }
    }

    // Gas Town segment (if in a Gas Town workspace)
    if let Some(gt) = gastown_info {
        let term_width = get_terminal_width();
        let max_len = match term_width {
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

            header_parts.push(format!(
                "{}{}{}{}",
                bracket(true),
                muted_label("gt:", tc),
                gt_colored,
                bracket(false)
            ));
        }
    }

    // Agent segment (if running as a subagent via --agent)
    if let Some(ref agent) = hook.agent {
        let agent_colored = tokens::ACCENT.paint(&agent.name, tc);
        header_parts.push(format!(
            "{}{}{}{}",
            bracket(true),
            muted_label("agent:", tc),
            agent_colored,
            bracket(false),
        ));
    }

    // Worktree segment (if in a --worktree session)
    if let Some(ref wt) = hook.worktree {
        let wt_name = tokens::ACCENT.paint(&wt.name, tc);
        header_parts.push(format!(
            "{}{}{}{}",
            bracket(true),
            muted_label("wt:", tc),
            wt_name,
            bracket(false),
        ));
    }

    // Output style segment (if present, skip "default")
    if let Some(ref output_style) = hook.output_style {
        let name_lower = output_style.name.to_lowercase();
        if name_lower != "default" {
            let style_colored = tokens::ACCENT.paint(&output_style.name, tc);
            header_parts.push(format!(
                "{}{}{}{}",
                bracket(true),
                muted_label("style:", tc),
                style_colored,
                bracket(false),
            ));
        }
    }

    // Effort level segment (from env var, skip default "medium")
    if let Ok(effort) = env::var("CLAUDE_CODE_EFFORT_LEVEL") {
        let effort_lower = effort.to_lowercase();
        if effort_lower != "unset" && !effort_lower.is_empty() {
            let term_width = get_terminal_width();
            let label = match term_width {
                TerminalWidth::Narrow => "eff:",
                _ => "effort:",
            };
            let effort_colored = match effort_lower.as_str() {
                "low" => tokens::EFFORT_LOW.paint(&effort_lower, tc),
                "medium" => tokens::EFFORT_MEDIUM.paint(&effort_lower, tc),
                "high" => tokens::EFFORT_HIGH.paint(&effort_lower, tc),
                "max" => tokens::EFFORT_MAX.bold(&effort_lower, tc),
                other => tokens::EFFORT_MEDIUM.paint(other, tc),
            };
            header_parts.push(format!(
                "{}{}{}{}",
                bracket(true),
                muted_label(label, tc),
                effort_colored,
                bracket(false),
            ));
        }
    }

    // Optional provider hints grouped (only when --show-provider is set)
    if args.show_provider {
        let mut prov_hint_parts: Vec<String> = Vec::new();
        if let Some(src) = api_key_source {
            prov_hint_parts.push(format!(
                "{}{}",
                muted_label("key:", tc),
                tokens::PRIMARY_DIM.paint(src, tc)
            ));
        }
        // Provider hint from env or deduced from model id
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
        if !prov_hint_parts.is_empty() {
            header_parts.push(format!(
                "{}{}{}",
                bracket(true),
                prov_hint_parts.join(" "),
                bracket(false)
            ));
        }
    }

    // Print header line: cwd then segments
    let dir_colored = tokens::ACCENT.paint(&dir_fmt, tc);
    println!("{} {}", dir_colored, header_parts.join(" "));
}

#[allow(clippy::too_many_arguments)]
pub fn print_text_output(
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
    _tpm: f64,
    tpm_indicator: f64,
    _cost_per_hour: f64,
    context: Option<(u64, u32)>,
    tokens_input: u64,
    tokens_output: u64,
    tokens_cache_create: u64,
    tokens_cache_read: u64,
    // session-scoped tokens within the current window
    _sess_tokens_input: u64,
    _sess_tokens_output: u64,
    _sess_tokens_cache_create: u64,
    _sess_tokens_cache_read: u64,
    web_search_requests: u64,
    // Session cost per hour (from hook duration); reserved for future display use
    _session_cost_per_hour: Option<f64>,
    _lines_delta: Option<(i64, i64)>,
    _rate_limit: Option<&RateLimitInfo>,
    usage_limits: Option<&UsageSummary>,
    // Override context limit from hook.context_window.context_window_size
    context_limit_override: Option<u64>,
) {
    // Detect terminal width for responsive formatting
    let term_width = get_terminal_width();
    let tc = is_truecolor_enabled(args);
    let compact = term_width == TerminalWidth::Narrow;

    // Prompt symbol
    let prompt = tokens::ACCENT.paint(SYM_PROMPT, tc);
    print!("{} ", prompt);

    // Labels preference
    let long_labels = matches!(args.labels, LabelsArg::Long) && !compact;

    // ═══════════════════════════════════════════════════════════════════════════
    // COST SECTION: session | today | window
    // ═══════════════════════════════════════════════════════════════════════════

    // Session cost
    let session_label = match term_width {
        TerminalWidth::Narrow => "s:",
        TerminalWidth::Medium => "sess:",
        TerminalWidth::Wide => "session:",
    };
    let session_cost_str = format_currency(session_cost);
    let session_colored = format!(
        "{}{}",
        tokens::MUTED.paint(SYM_DOLLAR, tc),
        tokens::PRIMARY.bold(&session_cost_str, tc)
    );
    print!("{}{}", muted_label(session_label, tc), session_colored);
    print!("{}", separator(tc, compact));

    // Today cost
    let today_label = match term_width {
        TerminalWidth::Narrow => "t:",
        _ => "today:",
    };
    let today_cost_str = format_currency(today_cost);
    let dollar_muted = tokens::MUTED.paint(SYM_DOLLAR, tc);
    let cost_token = tokens::gradient(today_cost, 10.0);
    let today_colored = format!("{}{}", dollar_muted, cost_token.paint(&today_cost_str, tc));
    print!("{}{}", muted_label(today_label, tc), today_colored);
    print!("{}", separator(tc, compact));

    // Check if we're using direct Claude API (for window/reset display)
    let is_claude = is_direct_claude_api(Some(model_id));

    // Window cost (Claude-specific)
    if is_claude {
        let window_label = match term_width {
            TerminalWidth::Narrow => "w:",
            TerminalWidth::Medium => "win:",
            TerminalWidth::Wide if long_labels => "window:",
            TerminalWidth::Wide => "win:",
        };
        let window_cost_str = format_currency(total_cost);
        let dollar_win = tokens::MUTED.paint(SYM_DOLLAR, tc);
        let win_token = tokens::gradient(total_cost, 5.0);
        let window_colored = format!("{}{}", dollar_win, win_token.bold(&window_cost_str, tc));
        print!("{}{}", muted_label(window_label, tc), window_colored);
        print!("{}", separator(tc, compact));
    }

    let use_12h = match args.time_fmt {
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
    };

    // ═══════════════════════════════════════════════════════════════════════════
    // USAGE SECTION: usage% -> projected% | 7d | model-specific
    // ═══════════════════════════════════════════════════════════════════════════

    // Usage (only if a plan/window max is configured)
    let is_stale = usage_limits.is_some_and(|s| s.stale);
    if is_claude {
        if let Some(usage_value) = usage_percent {
            let usage_colored = colorize_percent(usage_value, args);

            let usage_label = match (term_width, is_stale) {
                (TerminalWidth::Narrow, true) => "~u:",
                (TerminalWidth::Narrow, false) => "u:",
                (_, true) => "~usage:",
                (_, false) => "usage:",
            };

            // Build reset countdown for inline display next to usage
            let reset_inline = if is_claude {
                let rem_h = (remaining_minutes as i64) / 60;
                let rem_m = (remaining_minutes as i64) % 60;
                let countdown = if rem_h > 0 {
                    format!("{}h{}m", rem_h, rem_m)
                } else {
                    format!("{}m", rem_m)
                };

                let countdown_colored = if remaining_minutes < 30.0 {
                    tokens::ERROR.bold(&countdown, tc)
                } else if remaining_minutes < 60.0 {
                    tokens::WARNING.bold(&countdown, tc)
                } else if remaining_minutes < 180.0 {
                    tokens::WARNING.paint(&countdown, tc)
                } else {
                    tokens::PRIMARY_DIM.paint(&countdown, tc)
                };

                let window_end_local = if let Some(b) = active_block {
                    b.end.with_timezone(&Local)
                } else {
                    let now_utc = chrono::Utc::now();
                    let (_start, end) = window_bounds(now_utc, latest_reset);
                    end.with_timezone(&Local)
                };

                let reset_disp = if window_end_local.minute() == 0 {
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
                };

                format!(
                    " {} {}",
                    countdown_colored,
                    muted_label(&format!("({})", reset_disp), tc)
                )
            } else {
                String::new()
            };

            // Usage with optional projection arrow + inline reset
            if let Some(projected_value) = projected_percent {
                let proj_colored = colorize_percent(projected_value, args);
                let arrow = tokens::MUTED.dim(SYM_ARROW_RIGHT, tc);
                print!(
                    "{}{}{}{}{}",
                    muted_label(usage_label, tc),
                    usage_colored,
                    arrow,
                    proj_colored,
                    reset_inline
                );
            } else {
                print!(
                    "{}{}{}",
                    muted_label(usage_label, tc),
                    usage_colored,
                    reset_inline
                );
            }

            // 7-day and model-specific usage limits
            if let Some(summary) = usage_limits {
                let mut segments: Vec<String> = Vec::new();
                if let Some(pct) = summary.seven_day.utilization {
                    let label = if long_labels { "weekly:" } else { "7d:" };
                    let mut text =
                        format!("{}{}", muted_label(label, tc), colorize_percent(pct, args));
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
                        text.push_str(&format!(
                            " {}",
                            muted_label(&format!("({})", reset_fmt), tc)
                        ));
                    }
                    segments.push(text);
                }
                if let Some(pct) = summary.seven_day_opus.utilization {
                    segments.push(format!(
                        "{}{}",
                        muted_label("opus:", tc),
                        colorize_percent(pct, args)
                    ));
                }
                if let Some(pct) = summary.seven_day_sonnet.utilization {
                    segments.push(format!(
                        "{}{}",
                        muted_label("sonnet:", tc),
                        colorize_percent(pct, args)
                    ));
                }
                if !segments.is_empty() {
                    print!("{}", separator(tc, compact));
                    print!("{}", segments.join(&separator(tc, compact)));
                }

                // Extra usage (overuse credits)
                if let Some(ref extra) = summary.extra_usage {
                    if extra.is_enabled {
                        let label = if long_labels { "extra:" } else { "ex:" };
                        let spent = extra.used_credits.unwrap_or(0.0);
                        let limit = extra.monthly_limit.unwrap_or(0.0);
                        let dollar = tokens::MUTED.paint(SYM_DOLLAR, tc);
                        let spent_token = if limit > 0.0 {
                            tokens::gradient(spent / limit * 100.0, 100.0)
                        } else {
                            tokens::PRIMARY_DIM
                        };
                        let text = if limit > 0.0 {
                            format!(
                                "{}{}{}/{}",
                                muted_label(label, tc),
                                dollar,
                                spent_token.paint(&format!("{:.0}", spent), tc),
                                muted_label(&format!("{:.0}", limit), tc)
                            )
                        } else {
                            format!(
                                "{}{}{}",
                                muted_label(label, tc),
                                dollar,
                                spent_token.paint(&format!("{:.2}", spent), tc)
                            )
                        };
                        print!("{}{}", separator(tc, compact), text);
                    }
                }
            }

            print!("{}", separator(tc, compact));
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // TOKENS SECTION (optional breakdown)
    // ═══════════════════════════════════════════════════════════════════════════

    if args.show_breakdown {
        let ti = format_tokens(tokens_input);
        let to = format_tokens(tokens_output);
        let tcc = format_tokens(tokens_cache_create);
        let tcr = format_tokens(tokens_cache_read);
        let ws = web_search_requests;
        print!(
            "{}{} {}{} {}{}",
            muted_label("tok:", tc),
            tokens::PRIMARY_DIM.paint(&format!("{}/{}", ti, to), tc),
            muted_label("cache:", tc),
            tokens::PRIMARY_DIM.paint(&format!("{}/{}", tcc, tcr), tc),
            muted_label("ws:", tc),
            tokens::PRIMARY_DIM.paint(&ws.to_string(), tc)
        );
        print!("{}", separator(tc, compact));
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // CONTEXT SECTION: tokens/limit (%)
    // ═══════════════════════════════════════════════════════════════════════════

    let ctx_label = match term_width {
        TerminalWidth::Narrow => "ctx:",
        _ => "context:",
    };
    print!("{}", muted_label(ctx_label, tc));

    if let Some((ctx_tokens, pct)) = context {
        let pct_token = tokens::gradient(pct as f64, 100.0);
        let pct_colored = if pct >= 80 {
            pct_token.bold(&format!("{}%", pct), tc)
        } else {
            pct_token.paint(&format!("{}%", pct), tc)
        };

        let ctx_limit_full = context_limit_override
            .unwrap_or_else(|| context_limit_for_model_display(model_id, model_display_name));
        let ctx_limit_usable =
            ctx_limit_full.saturating_sub(reserved_output_tokens_for_model(model_id));
        let output_reserve = reserved_output_tokens_for_model(model_id);
        let overhead = system_overhead_tokens();
        let raw_tokens = ctx_tokens.saturating_sub(overhead);

        // Check if we're eating into the output reserve
        let over_usable = if ctx_tokens > ctx_limit_usable {
            let reserve_used = ctx_tokens - ctx_limit_usable;
            let reserve_remaining = output_reserve.saturating_sub(reserve_used);
            Some((reserve_used, reserve_remaining))
        } else {
            None
        };

        // Display context usage
        if overhead > 0 {
            print!(
                "{} {}{}{}",
                tokens::PRIMARY_DIM.paint(&format_tokens(raw_tokens), tc),
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
            );
        } else {
            print!(
                "{}/{} {}",
                tokens::PRIMARY_DIM.paint(&format_tokens(ctx_tokens), tc),
                muted_label(&format_tokens(ctx_limit_full), tc),
                pct_colored
            );
        }

        // Compact reserve usage indicator when eating into output reserve
        if let Some((used, _remaining)) = over_usable {
            print!(
                " {}{}{}",
                muted_label("rsv:", tc),
                tokens::ERROR.paint(&format_tokens(used), tc),
                muted_label(&format!("/{}", format_tokens(output_reserve)), tc),
            );
        }

        // Auto-compact hint
        if args.hints && pct >= 40 && crate::utils::auto_compact_enabled() {
            let usable = ctx_limit_full.saturating_sub(reserved_output_tokens_for_model(model_id));
            let cushion = crate::utils::auto_compact_headroom_tokens();
            let compact_trigger = usable.saturating_sub(cushion) as f64;
            let headroom_to_compact = (compact_trigger - ctx_tokens as f64).max(0.0);

            if tpm_indicator > 0.0 && headroom_to_compact > 0.0 {
                let eta_min = headroom_to_compact / tpm_indicator;
                let eta_min_i = eta_min.round() as i64;
                let eta_disp = if eta_min_i >= 120 {
                    format!("~{}h", eta_min_i / 60)
                } else if eta_min_i >= 60 {
                    format!("~{}h{}m", eta_min_i / 60, eta_min_i % 60)
                } else {
                    format!("~{}m", eta_min_i)
                };
                let compact_text = tokens::WARNING.paint(
                    &format!(
                        "{}@{}K {}",
                        muted_label("compact:", tc),
                        compact_trigger as u64 / 1000,
                        eta_disp
                    ),
                    tc,
                );
                print!("{}{}", separator(tc, compact), compact_text);
            } else {
                let compact_text = tokens::WARNING.paint(
                    &format!(
                        "{}@{}K",
                        muted_label("compact:", tc),
                        compact_trigger as u64 / 1000
                    ),
                    tc,
                );
                print!("{}{}", separator(tc, compact), compact_text);
            }
        }
    } else {
        print!("{}", muted_label("N/A", tc));
    }

    println!();
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
    let (ctx_tokens, ctx_pct) = context
        .map(|(t, p)| (Some(t), Some(p)))
        .unwrap_or((None, None));
    // Use hook-provided context limit if available, otherwise fall back to model detection
    let ctx_limit = context_limit_override.unwrap_or_else(|| {
        context_limit_for_model_display(&hook.model.id, &hook.model.display_name)
    });
    let overhead_value = system_overhead_tokens();
    let overhead_display = if ctx_tokens.is_some() && overhead_value > 0 {
        Some(overhead_value)
    } else {
        None
    };
    let ctx_tokens_raw = ctx_tokens.map(|t| t.saturating_sub(overhead_value));
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
        "reset_anchor_epoch": latest_reset.map(|d| d.timestamp()),
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

    // Augment session info with Claude-provided cost fields when present
    let (sess_duration_ms, sess_api_ms, sess_lines_added, sess_lines_removed, sess_cph_json) =
        if let Some(ref c) = hook.cost {
            let dur = c.total_duration_ms;
            let api = c.total_api_duration_ms;
            let la = c.total_lines_added;
            let lr = c.total_lines_removed;
            let cph = if let Some(ms) = dur {
                if ms > 0 {
                    let hrs = (ms as f64) / 3_600_000.0;
                    Some(((session_cost / hrs) * 100.0).round() / 100.0)
                } else {
                    None
                }
            } else {
                None
            };
            (dur, api, la, lr, cph)
        } else {
            (None, None, None, None, None)
        };

    let usage_limits_value = usage_limits.map(|summary| {
        serde_json::json!({
            "five_hour": usage_limit_json(&summary.window),
            "seven_day": usage_limit_json(&summary.seven_day),
            "seven_day_opus": usage_limit_json(&summary.seven_day_opus),
            "seven_day_sonnet": usage_limit_json(&summary.seven_day_sonnet),
            "seven_day_oauth_apps": usage_limit_json(&summary.seven_day_oauth_apps),
            "seven_day_cowork": usage_limit_json(&summary.seven_day_cowork),
            "extra_usage": summary.extra_usage.as_ref().map(|e| serde_json::json!({
                "is_enabled": e.is_enabled,
                "monthly_limit": e.monthly_limit,
                "used_credits": e.used_credits,
                "utilization": e.utilization
            }))
        })
    });

    let mut json = serde_json::json!({
        "model": {
            "id": hook.model.id.clone(),
            "display_name": hook.model.display_name.clone(),
            "fast_mode": is_fast_mode,
        },
        "cwd": hook.workspace.current_dir.clone(),
        "project_dir": hook.workspace.project_dir.clone(),
        "version": hook.version.clone(),
        "output_style": hook.output_style.as_ref().map(|s| serde_json::json!({"name": s.name.clone()})),
        "effort": env::var("CLAUDE_CODE_EFFORT_LEVEL").ok().and_then(|e| {
            let lower = e.to_lowercase();
            if lower == "unset" { None } else { Some(lower) }
        }),
        "provider": {"apiKeySource": api_key_source, "env": provider_final},
        "oauth_profile": {
            "organization_type": oauth_org_type,
            "rate_limit_tier": oauth_rate_tier
        },
        "reset_at": reset_iso,
        "session": {
            "cost_usd": (session_cost * 100.0).round() / 100.0,
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
            "sessions_count": sessions_count
        },
        "block": block_json.clone(),
        "window": block_json,
        "context": {
            "tokens": ctx_tokens,
            "tokens_raw": ctx_tokens_raw,
            "system_overhead_tokens": overhead_display,
            "percent": ctx_pct,
            "limit": ctx_limit,
            "limit_full": ctx_limit, // Same as limit, uses hook override when available
            "output_reserve": reserved_output_tokens_for_model(&hook.model.id),
            "output_reserve_used": ctx_tokens.map(|t| t.saturating_sub(ctx_limit)),
            "source": context_source,
            "headroom_tokens": context_headroom,
            "eta_minutes": context_eta_minutes
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
#[allow(clippy::too_many_arguments)]
pub fn print_json_output(
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
) -> anyhow::Result<()> {
    let json = build_json_output(
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
    );
    println!("{}", serde_json::to_string(&json)?);
    Ok(())
}
