use chrono::{DateTime, Local, Timelike};
use std::env;

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

fn colorize_percent(pct: f64) -> String {
    if pct >= 95.0 {
        format!("{pct:.1}%").red().bold().to_string()
    } else if pct >= 80.0 {
        format!("{pct:.1}%").yellow().bold().to_string()
    } else {
        format!("{pct:.1}%").green().to_string()
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
    if let Ok(v) = env::var("CLAUDE_TRUECOLOR") {
        if v.trim() == "1" {
            return true;
        }
    }
    args.truecolor
}

pub fn model_colored_name(model_id: &str, display: &str, args: &Args) -> String {
    // Respect NO_COLOR if set: return plain string
    if env::var("NO_COLOR").is_ok() {
        return display.to_string();
    }
    let lower = model_id.to_lowercase();
    let use_true = is_truecolor_enabled(args);
    if lower.contains("opus") {
        if use_true {
            format!("{}", display.truecolor(168, 85, 247))
        } else {
            format!("{}", display.bright_magenta())
        }
    } else if lower.contains("sonnet") {
        if use_true {
            format!("{}", display.truecolor(245, 158, 11))
        } else {
            format!("{}", display.bright_yellow())
        }
    } else if lower.contains("haiku") {
        if use_true {
            format!("{}", display.truecolor(6, 182, 212))
        } else {
            format!("{}", display.bright_cyan())
        }
    } else {
        format!("{}", display.bright_white())
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

    // Build header segments: git (minimal) + model + output_style + optional provider hints
    let mut header_parts: Vec<String> = Vec::new();

    // Git info from project_dir or current_dir
    if let Some(gi) = git_info {
        let mut git_seg = String::new();
        // worktree indicator
        if gi.is_linked_worktree == Some(true) {
            git_seg.push_str("wt ");
        }
        if let (Some(br), Some(sc)) = (gi.branch.as_ref(), gi.short_commit.as_ref()) {
            // branch and short sha
            git_seg.push_str("⎇ ");
            git_seg.push_str(&format!("{}@{}", br, sc));
        } else if let Some(sc) = gi.short_commit.as_ref() {
            git_seg.push_str(&format!("(detached@{})", sc));
        }
        // dirty marker
        if gi.is_clean == Some(false) {
            git_seg.push('*');
        }
        // ahead/behind
        if let (Some(a), Some(b)) = (gi.ahead, gi.behind) {
            if a > 0 {
                git_seg.push(' ');
                git_seg.push_str(&format!("↑{}", a));
            }
            if b > 0 {
                if a == 0 {
                    git_seg.push(' ');
                }
                git_seg.push_str(&format!("↓{}", b));
            }
        }
        // lines delta (working tree changes)
        if let Some((added, removed)) = lines_delta {
            if added != 0 || removed != 0 {
                if !git_seg.is_empty() {
                    git_seg.push_str(" ");
                }
                git_seg.push_str(&format!("+{}", added).green().to_string());
                git_seg.push_str(&format!("-{}", removed.abs()).red().to_string());
            }
        }
        if !git_seg.is_empty() {
            header_parts.push(format!(
                "{}{}{}",
                "[".bright_black(),
                git_seg.bright_white(),
                "]".bright_black()
            ));
        }
    }

    // Model segment
    header_parts.push(format!(
        "{}{}{}",
        "[".bright_black(),
        mdisp,
        "]".bright_black(),
    ));

    // Output style segment (if present)
    if let Some(ref output_style) = hook.output_style {
        header_parts.push(format!(
            "{}{}{}{}",
            "[".bright_black(),
            "style:".bright_black().dimmed(),
            output_style.name.bright_blue(),
            "]".bright_black(),
        ));
    }

    // Sessions segment (if detected)
    if let Some(si) = sessions_info {
        let mut sess_parts: Vec<String> = Vec::new();

        // Task
        if let Some(ref task) = si.current_task {
            sess_parts.push(format!(
                "{}{}",
                "task:".bright_black().dimmed(),
                task.cyan()
            ));
        }

        // Mode (lowercase to match existing style)
        if let Some(ref mode) = si.mode {
            let mode_text = match mode.as_str() {
                "Implementation" => "implement",
                _ => "discuss",
            };
            let mode_colored = match mode.as_str() {
                "Implementation" => mode_text.yellow().to_string(),
                _ => mode_text.white().to_string(),
            };
            sess_parts.push(format!(
                "{}{}",
                "mode:".bright_black().dimmed(),
                mode_colored
            ));
        }

        // Edited files count
        if si.edited_files > 0 {
            sess_parts.push(format!(
                "{}{}",
                "files:".bright_black().dimmed(),
                si.edited_files.to_string().yellow()
            ));
        }

        // Upstream (ahead/behind) - keep arrows as they're standard
        if let Some(ref upstream) = si.upstream {
            if upstream.ahead > 0 || upstream.behind > 0 {
                let mut up_parts = Vec::new();
                if upstream.ahead > 0 {
                    up_parts.push(format!("↑{}", upstream.ahead).green().to_string());
                }
                if upstream.behind > 0 {
                    up_parts.push(format!("↓{}", upstream.behind).red().to_string());
                }
                sess_parts.push(up_parts.join(" "));
            }
        }

        // Open tasks
        if si.open_tasks > 0 {
            sess_parts.push(format!(
                "{}{}",
                "tasks:".bright_black().dimmed(),
                si.open_tasks.to_string().cyan()
            ));
        }

        if !sess_parts.is_empty() {
            header_parts.push(format!(
                "{}{}{}",
                "[".bright_black(),
                sess_parts.join(" "),
                "]".bright_black(),
            ));
        }
    }

    // Optional provider hints grouped (only when --show-provider is set)
    if args.show_provider {
        let mut prov_hint_parts: Vec<String> = Vec::new();
        if let Some(src) = api_key_source {
            prov_hint_parts.push(format!("{}{}", "key:".bright_black().dimmed(), src.white()));
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
            "prov:".bright_black().dimmed(),
            prov_disp.white()
        ));
        if !prov_hint_parts.is_empty() {
            header_parts.push(format!(
                "{}{}{}",
                "[".bright_black(),
                prov_hint_parts.join(" "),
                "]".bright_black()
            ));
        }
    }

    // Print header line: cwd then segments
    println!("{} {}", dir_fmt.bright_blue(), header_parts.join(" "));
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
    cost_per_hour: f64,
    context: Option<(u64, u32)>,
    tokens_input: u64,
    tokens_output: u64,
    tokens_cache_create: u64,
    tokens_cache_read: u64,
    // session-scoped tokens within the current window
    sess_tokens_input: u64,
    sess_tokens_output: u64,
    sess_tokens_cache_create: u64,
    sess_tokens_cache_read: u64,
    web_search_requests: u64,
    // Optional enrichments from Claude's provided cost block
    session_cost_per_hour: Option<f64>,
    _lines_delta: Option<(i64, i64)>,
    rate_limit: Option<&RateLimitInfo>,
    usage_limits: Option<&UsageSummary>,
) {
    // Line 2
    print!("{} ", "❯".bright_cyan());

    // Labels preference
    let long_labels = matches!(args.labels, LabelsArg::Long);

    // session
    let session_label = "session:";
    print!(
        "{}{}{} ",
        session_label.bright_black().dimmed(),
        "$".bold().bright_white(),
        format_currency(session_cost).bold().bright_white()
    );
    print!("{} ", "·".bright_black().dimmed());

    // today
    let today_label = "today:";
    let today_cost_color = if today_cost >= 100.0 {
        format_currency(today_cost).bold().red().to_string()
    } else if today_cost >= 50.0 {
        format_currency(today_cost).bold().yellow().to_string()
    } else if today_cost >= 20.0 {
        format_currency(today_cost).yellow().to_string()
    } else {
        format_currency(today_cost).white().to_string()
    };
    print!(
        "{}{}{} ",
        today_label.bright_black().dimmed(),
        "$".white(),
        today_cost_color
    );
    print!("{} ", "·".bright_black().dimmed());

    // window (formerly block)
    let window_label = if long_labels {
        "current window:"
    } else {
        "window:"
    };
    let window_cost_color = if total_cost >= 50.0 {
        format_currency(total_cost).bold().red().to_string()
    } else if total_cost >= 20.0 {
        format_currency(total_cost).bold().yellow().to_string()
    } else if total_cost >= 10.0 {
        format_currency(total_cost).yellow().to_string()
    } else {
        format_currency(total_cost).bright_white().to_string()
    };
    print!(
        "{}{}{} ",
        window_label.bright_black().dimmed(),
        "$".bright_white(),
        window_cost_color
    );
    print!("{} ", "·".bright_black().dimmed());

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

    // usage (only if a plan/window max is configured)
    if let Some(usage_value) = usage_percent {
        let usage_colored = colorize_percent(usage_value);
        // Also show remaining percentage for clarity (what's left in window)
        let left = (100.0 - usage_value).max(0.0);
        let left_colored = if left <= 5.0 {
            format!("{left:.1}%").red().bold().to_string()
        } else if left <= 20.0 {
            format!("{left:.1}%").yellow().bold().to_string()
        } else {
            format!("{left:.1}%").green().to_string()
        };

        if let Some(projected_value) = projected_percent {
            let proj_colored = colorize_percent(projected_value);
            print!(
                "{}{}{}{} {}{} ",
                "usage:".bright_black().dimmed(),
                usage_colored,
                "→".bright_black().dimmed(),
                proj_colored,
                "left:".bright_black().dimmed(),
                left_colored
            );
        } else {
            print!(
                "{}{} {}{} ",
                "usage:".bright_black().dimmed(),
                usage_colored,
                "left:".bright_black().dimmed(),
                left_colored
            );
        }

        if let Some(summary) = usage_limits {
            let mut segments: Vec<String> = Vec::new();
            if let Some(pct) = summary.seven_day.utilization {
                let label = if long_labels { "weekly:" } else { "7d:" };
                segments.push(format!(
                    "{}{}",
                    label.bright_black().dimmed(),
                    colorize_percent(pct)
                ));
            }
            if let Some(pct) = summary.seven_day_opus.utilization {
                segments.push(format!(
                    "{}{}",
                    "opus:".bright_black().dimmed(),
                    colorize_percent(pct)
                ));
            }
            if !segments.is_empty() {
                print!("{} ", "·".bright_black().dimmed());
                let separator = format!(" {} ", "·".bright_black().dimmed());
                let joined = segments.join(&separator);
                print!("{} ", joined);
            }

            if args.hints {
                if let Some(reset) = summary.seven_day.resets_at {
                    let local = reset.with_timezone(&Local);
                    let fmt = if use_12h { "%a %-I:%M %p" } else { "%a %H:%M" };
                    print!(
                        "{}{} ",
                        "7d↻:".bright_black().dimmed(),
                        local.format(fmt).to_string().white()
                    );
                    print!("{} ", "·".bright_black().dimmed());
                }
                if let Some(reset) = summary.seven_day_opus.resets_at {
                    let local = reset.with_timezone(&Local);
                    let fmt = if use_12h { "%a %-I:%M %p" } else { "%a %H:%M" };
                    print!(
                        "{}{} ",
                        "opus↻:".bright_black().dimmed(),
                        local.format(fmt).to_string().white()
                    );
                    print!("{} ", "·".bright_black().dimmed());
                }
            }
        }

        print!("{} ", "·".bright_black().dimmed());

        if args.hints {
            // Approaching limit hint 
            // Show a friendly warning and a nudge to try /model when near cap
            let is_opus = model_id.to_lowercase().contains("opus");
            if usage_value >= 95.0 {
                let label = if is_opus {
                    "Opus usage limit"
                } else {
                    "usage limit"
                };
                print!(
                    "{}{} {} ",
                    "warn:".bright_black().dimmed(),
                    format!("{} nearly reached", label).red().bold(),
                    "/model best".bright_white().bold()
                );
                print!("{} ", "·".bright_black().dimmed());
            } else if usage_value >= 80.0 {
                let label = if is_opus {
                    "Opus usage limit"
                } else {
                    "usage limit"
                };
                print!(
                    "{}{} {} ",
                    "warn:".bright_black().dimmed(),
                    format!("Approaching {}", label).yellow().bold(),
                    "/model best".white()
                );
                print!("{} ", "·".bright_black().dimmed());
            }
        }
    }

    // countdown and reset time
    let rem_h = (remaining_minutes as i64) / 60;
    let rem_m = (remaining_minutes as i64) % 60;
    let countdown = if rem_h > 0 {
        format!("{}h {}m left", rem_h, rem_m)
    } else {
        format!("{}m left", rem_m)
    };
    // Emphasize as we get closer to the reset time
    let countdown_colored = if remaining_minutes < 30.0 {
        countdown.red().bold().to_string()
    } else if remaining_minutes < 60.0 {
        countdown.yellow().bold().to_string()
    } else if remaining_minutes < 180.0 {
        countdown.yellow().to_string()
    } else {
        countdown.white().to_string()
    };
    print!("{}{} ", "time:".bright_black().dimmed(), countdown_colored);
    print!("{} ", "·".bright_black().dimmed());

    // Rate limit indicators
    if let Some(rl) = rate_limit {
        if let Some(ref s) = rl.status {
            let s_col = match s.as_str() {
                "allowed_warning" => "warn".yellow().bold().to_string(),
                "rejected" => "rejected".red().bold().to_string(),
                _ => s.to_string().green().to_string(),
            };
            print!("{}{} ", "rl:".bright_black().dimmed(), s_col);
            print!("{} ", "·".bright_black().dimmed());
        }
        if rl.is_using_overage.unwrap_or(false) {
            let over = rl
                .overage_resets_at
                .map(|d| d.with_timezone(&Local))
                .map(|d| {
                    if matches!(args.time_fmt, TimeFormatArg::H12) {
                        d.format("%I:%M %p").to_string().trim().to_string()
                    } else {
                        d.format("%H:%M").to_string()
                    }
                })
                .unwrap_or_else(|| "n/a".to_string());
            print!(
                "{}{}{}{} ",
                "overage:".bright_black().dimmed(),
                "on ".red().bold(),
                "until ".bright_black().dimmed(),
                over.white()
            );
            print!("{} ", "·".bright_black().dimmed());
        }
        if rl.fallback_available.unwrap_or(false) {
            let pct = rl
                .fallback_percentage
                .map(|p| format!("{:.0}%", p * 100.0))
                .unwrap_or_else(|| "".to_string());
            print!(
                "{}{}{}{} ",
                "fallback:".bright_black().dimmed(),
                "available".green(),
                if pct.is_empty() {
                    "".to_string()
                } else {
                    " ".to_string()
                },
                pct
            );
            print!("{} ", "·".bright_black().dimmed());
        }
    }

    // Reset clock at window end (active end if available; else computed using shared window_bounds)
    let window_end_local = if let Some(b) = active_block {
        b.end.with_timezone(&Local)
    } else {
        // Use shared window_bounds function for consistent window calculation
        let now_utc = chrono::Utc::now();
        let (_start, end) = window_bounds(now_utc, latest_reset);
        end.with_timezone(&Local)
    };

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

    let fmt = if use_12h { "%-I:%M %p" } else { "%H:%M" };
    let reset_disp = window_end_local.format(fmt).to_string();
    let midnight = window_end_local.hour() == 0 && window_end_local.minute() == 0;
    // Prefer a more explicit "resets@" once close to window end (hints only)
    if args.hints && remaining_minutes <= 60.0 {
        print!(
            "{}{} ",
            "resets@".bright_black().dimmed(),
            reset_disp.white()
        );
    } else if !use_12h && midnight {
        // 24h mode hint for next day
        print!(
            "{}{}{} ",
            "↻ ".bright_black(),
            reset_disp.white(),
            " (+1d)".bright_black()
        );
    } else {
        print!(
            "{}{} ",
            "reset:".bright_black().dimmed(),
            reset_disp.white()
        );
    }
    print!("{} ", "·".bright_black().dimmed());

    // burn: show non-cache tokens per minute (indicator); usage% now based on TOTAL tokens
    let burn_val = format!("{}/m", format_tokens(tpm_indicator.round() as u64));
    let burn_colored = if tpm_indicator >= 5000.0 {
        format!("{}", burn_val.red().bold())
    } else if tpm_indicator >= 2000.0 {
        format!("{}", burn_val.yellow())
    } else {
        format!("{}", burn_val.green())
    };
    // Color cost/hour: green < $1/h, yellow $1–$5/h, red ≥ $5/h
    let cph_str = format!("${}/h", format_currency(cost_per_hour));
    let cph_colored = if cost_per_hour >= 5.0 {
        cph_str.red().bold().to_string()
    } else if cost_per_hour >= 1.0 {
        cph_str.yellow().to_string()
    } else {
        cph_str.green().to_string()
    };
    print!(
        "{}{} {} ",
        "burn:".bright_black().dimmed(),
        burn_colored,
        cph_colored
    );
    // (Usage percent already printed earlier as "usage (nc)" when plan caps are configured.)
    if let Some(sess_cph) = session_cost_per_hour {
        let sess_str = format!("${}/h", format_currency(sess_cph));
        let sess_colored = if sess_cph >= 5.0 {
            sess_str.red().bold().to_string()
        } else if sess_cph >= 1.0 {
            sess_str.yellow().to_string()
        } else {
            sess_str.green().to_string()
        };
        print!(" {}{} ", "sess:".bright_black().dimmed(), sess_colored);
    }
    print!("{} ", "·".bright_black().dimmed());

    // tokens breakdown (optional)
    if args.show_breakdown {
        let ti = format_tokens(tokens_input);
        let to = format_tokens(tokens_output);
        let tcc = format_tokens(tokens_cache_create);
        let tcr = format_tokens(tokens_cache_read);
        let ws = web_search_requests;
        print!(
            "{}{} {}{} {}{} ",
            "tok:".bright_black().dimmed(),
            format!("{}/{}", ti, to).white(),
            "cache:".bright_black().dimmed(),
            format!("{}/{}", tcc, tcr).white(),
            "ws:".bright_black().dimmed(),
            ws.to_string().white()
        );
        // Also show session-scoped breakdown for clarity
        let sti = format_tokens(sess_tokens_input);
        let sto = format_tokens(sess_tokens_output);
        let stcc = format_tokens(sess_tokens_cache_create);
        let stcr = format_tokens(sess_tokens_cache_read);
        print!(
            " {}{}{} {}{} ",
            "·".bright_black().dimmed(),
            "sess:".bright_black().dimmed(),
            format!("{}/{}", sti, sto).white(),
            "cache:".bright_black().dimmed(),
            format!("{}/{}", stcc, stcr).white()
        );
        print!("{} ", "·".bright_black().dimmed());
    }

    // context
    print!("{}", "context:".bright_black().dimmed());
    if let Some((tokens, pct)) = context {
        let pct_colored = if pct as f64 >= 80.0 {
            format!("{}%", pct).red().bold().to_string()
        } else if pct as f64 >= 50.0 {
            format!("{}%", pct).yellow().to_string()
        } else {
            format!("{}%", pct).green().to_string()
        };
        let ctx_limit_usable = context_limit_for_model_display(model_id, model_display_name)
            .saturating_sub(reserved_output_tokens_for_model(model_id));
        let ctx_limit_full = context_limit_for_model_display(model_id, model_display_name);
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

        if overhead > 0 {
            print!(
                "{} +{} sys = {}/{} ({})",
                format_tokens(raw_tokens),
                format_tokens(overhead),
                format_tokens(tokens),
                format_tokens(ctx_limit_full),
                pct_colored
            );
        } else {
            print!(
                "{}/{} ({})",
                format_tokens(tokens),
                format_tokens(ctx_limit_full),
                pct_colored
            );
        }

        // Show output reserve usage if we're over the usable limit
        if let Some((used, remaining)) = over_usable {
            print!(
                " {}",
                format!(
                    "⚠ using {} of {} output reserve ({} left, {} max)",
                    format_tokens(used),
                    format_tokens(output_reserve),
                    format_tokens(remaining),
                    format_tokens(ctx_limit_full)
                )
                .yellow()
                .bold()
            );
        }

        if args.hints {
            // Auto-compact hint: when context usage >= 40%, show headroom and ETA to full
            // Only show if auto-compact is actually enabled
            if pct >= 40 && crate::utils::auto_compact_enabled() {
                let ctx_limit = context_limit_for_model_display(model_id, model_display_name) as f64;
                let headroom_tokens = (ctx_limit - tokens as f64).max(0.0);
                // Use tpm_indicator (non-cache) to estimate time until context fills
                if tpm_indicator > 0.0 && headroom_tokens > 0.0 {
                    let eta_min = headroom_tokens / tpm_indicator;
                    let eta_min_i = eta_min.round() as i64;
                    let eta_disp = if eta_min_i >= 120 {
                        format!("~{}h", eta_min_i / 60)
                    } else if eta_min_i >= 60 {
                        format!("~{}h{}m", eta_min_i / 60, eta_min_i % 60)
                    } else {
                        format!("~{}m", eta_min_i)
                    };
                    print!(
                        " {}{}{}{}",
                        "·".bright_black().dimmed(),
                        "compact:".bright_black().dimmed(),
                        "≥40% ".yellow(),
                        eta_disp.yellow()
                    );
                } else {
                    // Show a simple hint if we cannot estimate time
                    print!(
                        " {}{}{}",
                        "·".bright_black().dimmed(),
                        "compact:".bright_black().dimmed(),
                        "≥40%".yellow()
                    );
                }
            }
        }
    } else {
        print!(
            "{}{} ",
            "usage:".bright_black().dimmed(),
            "N/A".bright_black().dimmed()
        );
        print!("{} ", "·".bright_black().dimmed());
    }
    println!();
}

#[allow(clippy::too_many_arguments)]
pub fn build_json_output(
    hook: &HookJson,
    session_cost: f64,
    today_cost: f64,
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
    let ctx_limit = context_limit_for_model_display(&hook.model.id, &hook.model.display_name);
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
            "seven_day_oauth_apps": usage_limit_json(&summary.seven_day_oauth_apps),
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
        "today": {"cost_usd": (today_cost * 100.0).round() / 100.0},
        "block": block_json.clone(),
        "window": block_json,
        "context": {
            "tokens": ctx_tokens,
            "tokens_raw": ctx_tokens_raw,
            "system_overhead_tokens": overhead_display,
            "percent": ctx_pct,
            "limit": ctx_limit,
            "limit_full": context_limit_for_model_display(&hook.model.id, &hook.model.display_name),
            "output_reserve": reserved_output_tokens_for_model(&hook.model.id),
            "output_reserve_used": ctx_tokens.map(|t| if t > ctx_limit { t - ctx_limit } else { 0 }),
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
) -> anyhow::Result<()> {
    let json = build_json_output(
        hook,
        session_cost,
        today_cost,
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
    );
    println!("{}", serde_json::to_string(&json)?);
    Ok(())
}
