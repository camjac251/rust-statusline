use chrono::{DateTime, Local, Timelike};

use crate::usage_api::is_direct_claude_api;
use std::env;

// ═══════════════════════════════════════════════════════════════════════════════
// COLOR PALETTE - Matching Claude Code's aesthetic
// ═══════════════════════════════════════════════════════════════════════════════

// Model-specific colors (truecolor)
const COLOR_OPUS: (u8, u8, u8) = (200, 160, 255); // Purple - Opus
const COLOR_SONNET: (u8, u8, u8) = (255, 200, 100); // Amber - Sonnet
const COLOR_HAIKU: (u8, u8, u8) = (100, 220, 255); // Cyan - Haiku

// Semantic colors matching Claude Code theme
const COLOR_SUCCESS: (u8, u8, u8) = (134, 239, 172); // Green - good states
const COLOR_WARNING: (u8, u8, u8) = (253, 224, 71); // Yellow - caution
const COLOR_ERROR: (u8, u8, u8) = (248, 113, 113); // Red - alerts
const COLOR_MUTED: (u8, u8, u8) = (148, 163, 184); // Slate - secondary text
const COLOR_ACCENT: (u8, u8, u8) = (96, 165, 250); // Blue - links/actions

// Gradient colors for usage visualization
const COLOR_GRADIENT_LOW: (u8, u8, u8) = (134, 239, 172); // Green
const COLOR_GRADIENT_MID: (u8, u8, u8) = (253, 224, 71); // Yellow
const COLOR_GRADIENT_HIGH: (u8, u8, u8) = (248, 113, 113); // Red

// ═══════════════════════════════════════════════════════════════════════════════
// UNICODE SYMBOLS - Matching Claude Code's icon set
// ═══════════════════════════════════════════════════════════════════════════════

const SYM_PROMPT: &str = "❯"; // Command prompt
const SYM_SEPARATOR: &str = "│"; // Vertical bar separator
const SYM_DOT: &str = "·"; // Dot separator (compact)
const SYM_ARROW_RIGHT: &str = "→"; // Projection arrow
const SYM_ARROW_UP: &str = "↑"; // Ahead indicator
const SYM_ARROW_DOWN: &str = "↓"; // Behind indicator
const SYM_WARNING: &str = "⚠"; // Warning indicator
const SYM_DOLLAR: &str = "$"; // Cost indicator

// Terminal width thresholds for responsive formatting
const WIDTH_NARROW: u16 = 140;
const WIDTH_MEDIUM: u16 = 200;
// Account for Claude CLI padding/margins (status line container has padding)
const TERMINAL_MARGIN: u16 = 15;

#[cfg(feature = "colors")]
use owo_colors::OwoColorize;

// Provide a no-op color shim when "colors" feature is disabled
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
        fn bright_blue(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn bright_cyan(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn bright_magenta(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn bright_yellow(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn bright_red(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn bright_green(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn red(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn yellow(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn green(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn white(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn bold(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn dimmed(&self) -> Plain {
            Plain(self.as_str().to_string())
        }
        fn truecolor(&self, _r: u8, _g: u8, _b: u8) -> Plain {
            // No-op truecolor in shim; returns plain string
            Plain(self.as_str().to_string())
        }
        fn cyan(&self) -> Plain {
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

#[cfg(not(feature = "colors"))]
use color_shim::ColorizeShim as OwoColorize;

use crate::cli::{Args, LabelsArg, TimeFormatArg};
use crate::models::{Block, GitInfo, HookJson, RateLimitInfo};
use crate::usage_api::{UsageLimit, UsageSummary};
use crate::utils::{
    context_limit_for_model_display, deduce_provider_from_model, format_currency, format_path,
    format_tokens, reserved_output_tokens_for_model, system_overhead_tokens,
};
use crate::window::window_bounds;

fn format_pct(pct: f64) -> String {
    if pct.fract() == 0.0 {
        format!("{:.0}%", pct)
    } else {
        format!("{:.1}%", pct)
    }
}

// Interpolate between semantic gradient colors: green → yellow → red
fn color_scale_rgb(value: f64, max: f64) -> (u8, u8, u8) {
    let ratio = (value / max).clamp(0.0, 1.0);

    if ratio < 0.5 {
        // Green to Yellow
        let t = ratio * 2.0;
        let r = (COLOR_GRADIENT_LOW.0 as f64
            + (COLOR_GRADIENT_MID.0 as f64 - COLOR_GRADIENT_LOW.0 as f64) * t)
            as u8;
        let g = (COLOR_GRADIENT_LOW.1 as f64
            + (COLOR_GRADIENT_MID.1 as f64 - COLOR_GRADIENT_LOW.1 as f64) * t)
            as u8;
        let b = (COLOR_GRADIENT_LOW.2 as f64
            + (COLOR_GRADIENT_MID.2 as f64 - COLOR_GRADIENT_LOW.2 as f64) * t)
            as u8;
        (r, g, b)
    } else {
        // Yellow to Red
        let t = (ratio - 0.5) * 2.0;
        let r = (COLOR_GRADIENT_MID.0 as f64
            + (COLOR_GRADIENT_HIGH.0 as f64 - COLOR_GRADIENT_MID.0 as f64) * t)
            as u8;
        let g = (COLOR_GRADIENT_MID.1 as f64
            + (COLOR_GRADIENT_HIGH.1 as f64 - COLOR_GRADIENT_MID.1 as f64) * t)
            as u8;
        let b = (COLOR_GRADIENT_MID.2 as f64
            + (COLOR_GRADIENT_HIGH.2 as f64 - COLOR_GRADIENT_MID.2 as f64) * t)
            as u8;
        (r, g, b)
    }
}

// Helper: format with muted color for labels
fn muted_label(text: &str, use_true: bool) -> String {
    if use_true {
        text.truecolor(COLOR_MUTED.0, COLOR_MUTED.1, COLOR_MUTED.2)
            .to_string()
    } else {
        text.bright_black().dimmed().to_string()
    }
}

// Helper: format separator
fn separator(use_true: bool, compact: bool) -> String {
    let sym = if compact { SYM_DOT } else { SYM_SEPARATOR };
    if use_true {
        format!(
            " {} ",
            sym.truecolor(COLOR_MUTED.0, COLOR_MUTED.1, COLOR_MUTED.2)
        )
    } else {
        format!(" {} ", sym.bright_black().dimmed())
    }
}

fn colorize_percent(pct: f64, args: &Args) -> String {
    let formatted = format_pct(pct);
    if is_truecolor_enabled(args) {
        // Gradient: 0% -> 100%
        let (r, g, b) = color_scale_rgb(pct, 100.0);
        formatted.truecolor(r, g, b).to_string()
    } else if pct >= 95.0 {
        formatted.red().bold().to_string()
    } else if pct >= 80.0 {
        formatted.yellow().bold().to_string()
    } else {
        formatted.green().to_string()
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
    let use_true = is_truecolor_enabled(args);

    // Opus family (and Claude 2 legacy) -> Purple
    if lower_id.contains("opus") || lower_disp.contains("opus") || lower_id.contains("claude-2") {
        if use_true {
            display
                .truecolor(COLOR_OPUS.0, COLOR_OPUS.1, COLOR_OPUS.2)
                .to_string()
        } else {
            display.bright_magenta().to_string()
        }
    }
    // Sonnet family -> Amber/Yellow
    else if lower_id.contains("sonnet") || lower_disp.contains("sonnet") {
        if use_true {
            display
                .truecolor(COLOR_SONNET.0, COLOR_SONNET.1, COLOR_SONNET.2)
                .to_string()
        } else {
            display.bright_yellow().to_string()
        }
    }
    // Haiku family (and Instant legacy) -> Cyan/Blue
    else if lower_id.contains("haiku")
        || lower_disp.contains("haiku")
        || lower_id.contains("claude-instant")
    {
        if use_true {
            display
                .truecolor(COLOR_HAIKU.0, COLOR_HAIKU.1, COLOR_HAIKU.2)
                .to_string()
        } else {
            display.bright_cyan().to_string()
        }
    } else {
        // Unknown/Other -> White
        display.bright_white().to_string()
    }
}

pub fn print_header(
    hook: &HookJson,
    git_info: Option<&GitInfo>,
    args: &Args,
    api_key_source: Option<&str>,
    sessions_info: Option<&crate::models::SessionsInfo>,
    lines_delta: Option<(i64, i64)>,
) {
    let dir_fmt = format_path(&hook.workspace.current_dir);
    let mdisp = model_colored_name(&hook.model.id, &hook.model.display_name, args);
    let use_true = is_truecolor_enabled(args);

    // Build header segments: git (minimal) + model + output_style + optional provider hints
    let mut header_parts: Vec<String> = Vec::new();

    // Helper for bracket styling
    let bracket = |open: bool| -> String {
        let ch = if open { "[" } else { "]" };
        if use_true {
            ch.truecolor(COLOR_MUTED.0, COLOR_MUTED.1, COLOR_MUTED.2)
                .to_string()
        } else {
            ch.bright_black().to_string()
        }
    };

    // Git info from project_dir or current_dir
    if let Some(gi) = git_info {
        let mut git_seg = String::new();
        // worktree indicator
        if gi.is_linked_worktree == Some(true) {
            git_seg.push_str(&muted_label("wt ", use_true));
        }
        if let (Some(br), Some(sc)) = (gi.branch.as_ref(), gi.short_commit.as_ref()) {
            // branch and short sha
            git_seg.push_str(&br.bright_white().to_string());
            git_seg.push_str(&muted_label("@", use_true));
            git_seg.push_str(&sc.bright_white().to_string());
        } else if let Some(sc) = gi.short_commit.as_ref() {
            git_seg.push_str(&muted_label("detached@", use_true));
            git_seg.push_str(&sc.bright_white().to_string());
        }
        // dirty marker
        if gi.is_clean == Some(false) {
            if use_true {
                git_seg.push_str(
                    &"*".truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                        .to_string(),
                );
            } else {
                git_seg.push_str(&"*".yellow().to_string());
            }
        }
        // ahead/behind
        if let (Some(a), Some(b)) = (gi.ahead, gi.behind) {
            if a > 0 {
                git_seg.push(' ');
                if use_true {
                    git_seg.push_str(
                        &format!("{}{}", SYM_ARROW_UP, a)
                            .truecolor(COLOR_SUCCESS.0, COLOR_SUCCESS.1, COLOR_SUCCESS.2)
                            .to_string(),
                    );
                } else {
                    git_seg.push_str(&format!("{}{}", SYM_ARROW_UP, a).green().to_string());
                }
            }
            if b > 0 {
                if a == 0 {
                    git_seg.push(' ');
                }
                if use_true {
                    git_seg.push_str(
                        &format!("{}{}", SYM_ARROW_DOWN, b)
                            .truecolor(COLOR_ERROR.0, COLOR_ERROR.1, COLOR_ERROR.2)
                            .to_string(),
                    );
                } else {
                    git_seg.push_str(&format!("{}{}", SYM_ARROW_DOWN, b).red().to_string());
                }
            }
        }
        // lines delta (working tree changes)
        if let Some((added, removed)) = lines_delta {
            if added != 0 || removed != 0 {
                if !git_seg.is_empty() {
                    git_seg.push(' ');
                }
                if use_true {
                    git_seg.push_str(
                        &format!("+{}", added)
                            .truecolor(COLOR_SUCCESS.0, COLOR_SUCCESS.1, COLOR_SUCCESS.2)
                            .to_string(),
                    );
                    git_seg.push_str(
                        &format!("-{}", removed.abs())
                            .truecolor(COLOR_ERROR.0, COLOR_ERROR.1, COLOR_ERROR.2)
                            .to_string(),
                    );
                } else {
                    git_seg.push_str(&format!("+{}", added).green().to_string());
                    git_seg.push_str(&format!("-{}", removed.abs()).red().to_string());
                }
            }
        }
        if !git_seg.is_empty() {
            header_parts.push(format!("{}{}{}", bracket(true), git_seg, bracket(false)));
        }
    }

    // Model segment
    header_parts.push(format!("{}{}{}", bracket(true), mdisp, bracket(false)));

    // Output style segment (if present)
    if let Some(ref output_style) = hook.output_style {
        let style_colored = if use_true {
            output_style
                .name
                .truecolor(COLOR_ACCENT.0, COLOR_ACCENT.1, COLOR_ACCENT.2)
                .to_string()
        } else {
            output_style.name.bright_blue().to_string()
        };
        header_parts.push(format!(
            "{}{}{}{}",
            bracket(true),
            muted_label("style:", use_true),
            style_colored,
            bracket(false),
        ));
    }

    // Sessions segment (if detected)
    if let Some(si) = sessions_info {
        let mut sess_parts: Vec<String> = Vec::new();

        // Task
        if let Some(ref task) = si.current_task {
            sess_parts.push(format!("{}{}", muted_label("task:", use_true), task.cyan()));
        }

        // Mode (lowercase to match existing style)
        if let Some(ref mode) = si.mode {
            let mode_text = match mode.as_str() {
                "Implementation" => "implement",
                _ => "discuss",
            };
            let mode_colored = if use_true {
                match mode.as_str() {
                    "Implementation" => mode_text
                        .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                        .to_string(),
                    _ => mode_text.white().to_string(),
                }
            } else {
                match mode.as_str() {
                    "Implementation" => mode_text.yellow().to_string(),
                    _ => mode_text.white().to_string(),
                }
            };
            sess_parts.push(format!(
                "{}{}",
                muted_label("mode:", use_true),
                mode_colored
            ));
        }

        // Edited files count
        if si.edited_files > 0 {
            let files_colored = if use_true {
                si.edited_files
                    .to_string()
                    .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                    .to_string()
            } else {
                si.edited_files.to_string().yellow().to_string()
            };
            sess_parts.push(format!(
                "{}{}",
                muted_label("files:", use_true),
                files_colored
            ));
        }

        // Upstream (ahead/behind)
        if let Some(ref upstream) = si.upstream {
            if upstream.ahead > 0 || upstream.behind > 0 {
                let mut up_parts = Vec::new();
                if upstream.ahead > 0 {
                    if use_true {
                        up_parts.push(
                            format!("{}{}", SYM_ARROW_UP, upstream.ahead)
                                .truecolor(COLOR_SUCCESS.0, COLOR_SUCCESS.1, COLOR_SUCCESS.2)
                                .to_string(),
                        );
                    } else {
                        up_parts.push(
                            format!("{}{}", SYM_ARROW_UP, upstream.ahead)
                                .green()
                                .to_string(),
                        );
                    }
                }
                if upstream.behind > 0 {
                    if use_true {
                        up_parts.push(
                            format!("{}{}", SYM_ARROW_DOWN, upstream.behind)
                                .truecolor(COLOR_ERROR.0, COLOR_ERROR.1, COLOR_ERROR.2)
                                .to_string(),
                        );
                    } else {
                        up_parts.push(
                            format!("{}{}", SYM_ARROW_DOWN, upstream.behind)
                                .red()
                                .to_string(),
                        );
                    }
                }
                sess_parts.push(up_parts.join(" "));
            }
        }

        // Open tasks
        if si.open_tasks > 0 {
            sess_parts.push(format!(
                "{}{}",
                muted_label("tasks:", use_true),
                si.open_tasks.to_string().cyan()
            ));
        }

        if !sess_parts.is_empty() {
            header_parts.push(format!(
                "{}{}{}",
                bracket(true),
                sess_parts.join(" "),
                bracket(false),
            ));
        }
    }

    // Optional provider hints grouped (only when --show-provider is set)
    if args.show_provider {
        let mut prov_hint_parts: Vec<String> = Vec::new();
        if let Some(src) = api_key_source {
            prov_hint_parts.push(format!("{}{}", muted_label("key:", use_true), src.white()));
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
            muted_label("prov:", use_true),
            prov_disp.white()
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
    let dir_colored = if use_true {
        dir_fmt
            .truecolor(COLOR_ACCENT.0, COLOR_ACCENT.1, COLOR_ACCENT.2)
            .to_string()
    } else {
        dir_fmt.bright_blue().to_string()
    };
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
    // Optional enrichments from Claude's provided cost block
    _session_cost_per_hour: Option<f64>,
    _lines_delta: Option<(i64, i64)>,
    _rate_limit: Option<&RateLimitInfo>,
    usage_limits: Option<&UsageSummary>,
    // Override context limit from hook.context_window.context_window_size
    context_limit_override: Option<u64>,
) {
    // Detect terminal width for responsive formatting
    let term_width = get_terminal_width();
    let use_true = is_truecolor_enabled(args);
    let compact = term_width == TerminalWidth::Narrow;

    // Prompt symbol
    let prompt = if use_true {
        SYM_PROMPT
            .truecolor(COLOR_ACCENT.0, COLOR_ACCENT.1, COLOR_ACCENT.2)
            .to_string()
    } else {
        SYM_PROMPT.bright_cyan().to_string()
    };
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
        if use_true {
            SYM_DOLLAR
                .truecolor(COLOR_MUTED.0, COLOR_MUTED.1, COLOR_MUTED.2)
                .to_string()
        } else {
            SYM_DOLLAR.bright_white().bold().to_string()
        },
        session_cost_str.bold().bright_white()
    );
    print!(
        "{}{}",
        muted_label(session_label, use_true),
        session_colored
    );
    print!("{}", separator(use_true, compact));

    // Today cost
    let today_label = match term_width {
        TerminalWidth::Narrow => "t:",
        _ => "today:",
    };
    let today_cost_str = format_currency(today_cost);
    let dollar_muted = if use_true {
        SYM_DOLLAR
            .truecolor(COLOR_MUTED.0, COLOR_MUTED.1, COLOR_MUTED.2)
            .to_string()
    } else {
        SYM_DOLLAR.white().to_string()
    };
    let today_colored = if use_true {
        let (r, g, b) = color_scale_rgb(today_cost, 10.0);
        format!("{}{}", dollar_muted, today_cost_str.truecolor(r, g, b))
    } else if today_cost >= 100.0 {
        format!("{}{}", dollar_muted, today_cost_str.bold().red())
    } else if today_cost >= 50.0 {
        format!("{}{}", dollar_muted, today_cost_str.bold().yellow())
    } else if today_cost >= 20.0 {
        format!("{}{}", dollar_muted, today_cost_str.yellow())
    } else {
        format!("{}{}", dollar_muted, today_cost_str.white())
    };
    print!("{}{}", muted_label(today_label, use_true), today_colored);
    print!("{}", separator(use_true, compact));

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
        let dollar_win = if use_true {
            SYM_DOLLAR
                .truecolor(COLOR_MUTED.0, COLOR_MUTED.1, COLOR_MUTED.2)
                .to_string()
        } else {
            SYM_DOLLAR.bright_white().to_string()
        };
        let window_colored = if use_true {
            let (r, g, b) = color_scale_rgb(total_cost, 5.0);
            format!(
                "{}{}",
                dollar_win,
                window_cost_str.truecolor(r, g, b).bold()
            )
        } else if total_cost >= 50.0 {
            format!("{}{}", dollar_win, window_cost_str.bold().red())
        } else if total_cost >= 20.0 {
            format!("{}{}", dollar_win, window_cost_str.bold().yellow())
        } else if total_cost >= 10.0 {
            format!("{}{}", dollar_win, window_cost_str.yellow())
        } else {
            format!("{}{}", dollar_win, window_cost_str.bright_white())
        };
        print!("{}{}", muted_label(window_label, use_true), window_colored);
        print!("{}", separator(use_true, compact));
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
    // USAGE SECTION: usage% → projected% | 7d | model-specific
    // ═══════════════════════════════════════════════════════════════════════════

    // Usage (only if a plan/window max is configured)
    if is_claude {
        if let Some(usage_value) = usage_percent {
            let usage_colored = colorize_percent(usage_value, args);

            let usage_label = match term_width {
                TerminalWidth::Narrow => "u:",
                _ => "usage:",
            };

            // Usage with optional projection arrow
            if let Some(projected_value) = projected_percent {
                let proj_colored = colorize_percent(projected_value, args);
                let arrow = if use_true {
                    SYM_ARROW_RIGHT
                        .truecolor(COLOR_MUTED.0, COLOR_MUTED.1, COLOR_MUTED.2)
                        .to_string()
                } else {
                    SYM_ARROW_RIGHT.bright_black().dimmed().to_string()
                };
                print!(
                    "{}{}{}{}",
                    muted_label(usage_label, use_true),
                    usage_colored,
                    arrow,
                    proj_colored
                );
            } else {
                print!("{}{}", muted_label(usage_label, use_true), usage_colored);
            }

            // 7-day and model-specific usage limits
            if let Some(summary) = usage_limits {
                let mut segments: Vec<String> = Vec::new();
                if let Some(pct) = summary.seven_day.utilization {
                    let label = if long_labels { "weekly:" } else { "7d:" };
                    let mut text = format!(
                        "{}{}",
                        muted_label(label, use_true),
                        colorize_percent(pct, args)
                    );
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
                            muted_label(&format!("({})", reset_fmt), use_true)
                        ));
                    }
                    segments.push(text);
                }
                if let Some(pct) = summary.seven_day_opus.utilization {
                    segments.push(format!(
                        "{}{}",
                        muted_label("opus:", use_true),
                        colorize_percent(pct, args)
                    ));
                }
                if let Some(pct) = summary.seven_day_sonnet.utilization {
                    segments.push(format!(
                        "{}{}",
                        muted_label("sonnet:", use_true),
                        colorize_percent(pct, args)
                    ));
                }
                if !segments.is_empty() {
                    print!("{}", separator(use_true, compact));
                    print!("{}", segments.join(&separator(use_true, compact)));
                }
            }

            print!("{}", separator(use_true, compact));

            // Approaching limit hints
            if args.hints {
                let is_opus = model_id.to_lowercase().contains("opus");
                if usage_value >= 95.0 {
                    let label = if is_opus { "Opus limit" } else { "limit" };
                    let warn_text = if use_true {
                        let sym = SYM_WARNING
                            .truecolor(COLOR_ERROR.0, COLOR_ERROR.1, COLOR_ERROR.2)
                            .to_string();
                        format!("{} {} nearly reached", sym, label)
                            .truecolor(COLOR_ERROR.0, COLOR_ERROR.1, COLOR_ERROR.2)
                            .bold()
                            .to_string()
                    } else {
                        format!("{} {} nearly reached", SYM_WARNING, label)
                            .red()
                            .bold()
                            .to_string()
                    };
                    print!("{}", warn_text);
                    print!("{}", separator(use_true, compact));
                } else if usage_value >= 80.0 {
                    let label = if is_opus { "Opus limit" } else { "limit" };
                    let warn_text = if use_true {
                        let sym = SYM_WARNING
                            .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                            .to_string();
                        format!("{} approaching {}", sym, label)
                            .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                            .to_string()
                    } else {
                        format!("{} approaching {}", SYM_WARNING, label)
                            .yellow()
                            .to_string()
                    };
                    print!("{}", warn_text);
                    print!("{}", separator(use_true, compact));
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // RESET SECTION: countdown (reset time)
    // ═══════════════════════════════════════════════════════════════════════════

    if is_claude {
        let rem_h = (remaining_minutes as i64) / 60;
        let rem_m = (remaining_minutes as i64) % 60;
        let countdown = if rem_h > 0 {
            format!("{}h{}m", rem_h, rem_m)
        } else {
            format!("{}m", rem_m)
        };

        // Color countdown based on urgency
        let countdown_colored = if use_true {
            if remaining_minutes < 30.0 {
                countdown
                    .truecolor(COLOR_ERROR.0, COLOR_ERROR.1, COLOR_ERROR.2)
                    .bold()
                    .to_string()
            } else if remaining_minutes < 60.0 {
                countdown
                    .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                    .bold()
                    .to_string()
            } else if remaining_minutes < 180.0 {
                countdown
                    .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                    .to_string()
            } else {
                countdown.white().to_string()
            }
        } else if remaining_minutes < 30.0 {
            countdown.red().bold().to_string()
        } else if remaining_minutes < 60.0 {
            countdown.yellow().bold().to_string()
        } else if remaining_minutes < 180.0 {
            countdown.yellow().to_string()
        } else {
            countdown.white().to_string()
        };

        // Reset clock at window end
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

        let reset_label = match term_width {
            TerminalWidth::Narrow => "r:",
            _ => "reset:",
        };

        print!(
            "{}{} {}",
            muted_label(reset_label, use_true),
            countdown_colored,
            muted_label(&format!("({})", reset_disp), use_true)
        );
        print!("{}", separator(use_true, compact));
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
            muted_label("tok:", use_true),
            format!("{}/{}", ti, to).white(),
            muted_label("cache:", use_true),
            format!("{}/{}", tcc, tcr).white(),
            muted_label("ws:", use_true),
            ws.to_string().white()
        );
        print!("{}", separator(use_true, compact));
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // CONTEXT SECTION: tokens/limit (%)
    // ═══════════════════════════════════════════════════════════════════════════

    let ctx_label = match term_width {
        TerminalWidth::Narrow => "ctx:",
        _ => "context:",
    };
    print!("{}", muted_label(ctx_label, use_true));

    if let Some((tokens, pct)) = context {
        // Color percentage based on usage
        let pct_colored = if use_true {
            if pct as f64 >= 80.0 {
                format!("{}%", pct)
                    .truecolor(COLOR_ERROR.0, COLOR_ERROR.1, COLOR_ERROR.2)
                    .bold()
                    .to_string()
            } else if pct as f64 >= 50.0 {
                format!("{}%", pct)
                    .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                    .to_string()
            } else {
                format!("{}%", pct)
                    .truecolor(COLOR_SUCCESS.0, COLOR_SUCCESS.1, COLOR_SUCCESS.2)
                    .to_string()
            }
        } else if pct as f64 >= 80.0 {
            format!("{}%", pct).red().bold().to_string()
        } else if pct as f64 >= 50.0 {
            format!("{}%", pct).yellow().to_string()
        } else {
            format!("{}%", pct).green().to_string()
        };

        let ctx_limit_full = context_limit_override
            .unwrap_or_else(|| context_limit_for_model_display(model_id, model_display_name));
        let ctx_limit_usable =
            ctx_limit_full.saturating_sub(reserved_output_tokens_for_model(model_id));
        let output_reserve = reserved_output_tokens_for_model(model_id);
        let overhead = system_overhead_tokens();
        let raw_tokens = tokens.saturating_sub(overhead);

        // Check if we're eating into the output reserve
        let over_usable = if tokens > ctx_limit_usable {
            let reserve_used = tokens - ctx_limit_usable;
            let reserve_remaining = output_reserve.saturating_sub(reserve_used);
            Some((reserve_used, reserve_remaining))
        } else {
            None
        };

        // Display context usage
        if overhead > 0 {
            print!(
                "{} {}{}{}",
                format_tokens(raw_tokens).white(),
                muted_label("+", use_true),
                muted_label(&format!("{} sys = ", format_tokens(overhead)), use_true),
                format!(
                    "{}/{} ({})",
                    format_tokens(tokens),
                    format_tokens(ctx_limit_full),
                    pct_colored
                )
                .white()
            );
        } else {
            print!(
                "{}/{} {}",
                format_tokens(tokens).white(),
                muted_label(&format_tokens(ctx_limit_full), use_true),
                pct_colored
            );
        }

        // Warnings about output reserve
        if let Some((used, remaining)) = over_usable {
            let warn = if use_true {
                let sym = SYM_WARNING
                    .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                    .to_string();
                format!(
                    " {} using {} of {} reserve ({} left)",
                    sym,
                    format_tokens(used),
                    format_tokens(output_reserve),
                    format_tokens(remaining)
                )
                .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                .bold()
                .to_string()
            } else {
                format!(
                    " {} using {} of {} reserve ({} left)",
                    SYM_WARNING,
                    format_tokens(used),
                    format_tokens(output_reserve),
                    format_tokens(remaining)
                )
                .yellow()
                .bold()
                .to_string()
            };
            print!("{}", warn);
        } else if args.hints {
            let headroom_to_usable = ctx_limit_usable.saturating_sub(tokens);
            if headroom_to_usable > 0 && headroom_to_usable <= 10_000 {
                let warn = if use_true {
                    let sym = SYM_WARNING
                        .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                        .to_string();
                    format!(
                        " {} {} until reserve",
                        sym,
                        format_tokens(headroom_to_usable)
                    )
                    .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                    .to_string()
                } else {
                    format!(
                        " {} {} until reserve",
                        SYM_WARNING,
                        format_tokens(headroom_to_usable)
                    )
                    .yellow()
                    .to_string()
                };
                print!("{}", warn);
            }
        }

        // Auto-compact hint
        if args.hints && pct >= 40 && crate::utils::auto_compact_enabled() {
            let usable = ctx_limit_full.saturating_sub(reserved_output_tokens_for_model(model_id));
            let cushion = crate::utils::auto_compact_headroom_tokens();
            let compact_trigger = usable.saturating_sub(cushion) as f64;
            let headroom_to_compact = (compact_trigger - tokens as f64).max(0.0);

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
                let compact_text = if use_true {
                    format!(
                        "{}@{}K {}",
                        muted_label("compact:", use_true),
                        compact_trigger as u64 / 1000,
                        eta_disp
                    )
                    .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                    .to_string()
                } else {
                    format!(
                        "{}@{}K {}",
                        "compact:".bright_black().dimmed(),
                        compact_trigger as u64 / 1000,
                        eta_disp
                    )
                    .yellow()
                    .to_string()
                };
                print!("{}{}", separator(use_true, compact), compact_text);
            } else {
                let compact_text = if use_true {
                    format!(
                        "{}@{}K",
                        muted_label("compact:", use_true),
                        compact_trigger as u64 / 1000
                    )
                    .truecolor(COLOR_WARNING.0, COLOR_WARNING.1, COLOR_WARNING.2)
                    .to_string()
                } else {
                    format!("compact:@{}K", compact_trigger as u64 / 1000)
                        .yellow()
                        .to_string()
                };
                print!("{}{}", separator(use_true, compact), compact_text);
            }
        }
    } else {
        print!("{}", muted_label("N/A", use_true));
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
    sessions_info: Option<&crate::models::SessionsInfo>,
    // Override context limit from hook.context_window.context_window_size
    context_limit_override: Option<u64>,
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
            "extra_usage": summary.extra_usage.as_ref().map(|e| serde_json::json!({
                "is_enabled": e.is_enabled,
                "monthly_limit": e.monthly_limit,
                "used_credits": e.used_credits,
                "utilization": e.utilization
            }))
        })
    });

    serde_json::json!({
        "model": {"id": hook.model.id.clone(), "display_name": hook.model.display_name.clone()},
        "cwd": hook.workspace.current_dir.clone(),
        "project_dir": hook.workspace.project_dir.clone(),
        "version": hook.version.clone(),
        "output_style": hook.output_style.as_ref().map(|s| serde_json::json!({"name": s.name.clone()})),
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
        "sessions": sessions_info.map(|si| serde_json::json!({
            "detected": si.detected,
            "current_task": si.current_task,
            "mode": si.mode,
            "open_tasks": si.open_tasks,
            "edited_files": si.edited_files,
            "upstream": si.upstream.as_ref().map(|u| serde_json::json!({
                "ahead": u.ahead,
                "behind": u.behind
            }))
        }))
    })
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
    sessions_info: Option<&crate::models::SessionsInfo>,
    context_limit_override: Option<u64>,
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
        sessions_info,
        context_limit_override,
    );
    println!("{}", serde_json::to_string(&json)?);
    Ok(())
}
