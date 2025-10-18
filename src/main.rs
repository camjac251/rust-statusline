use anyhow::{Context, Result};
use chrono::Utc;
#[cfg(feature = "colors")]
use owo_colors::OwoColorize;
use std::path::Path;

use claude_statusline::cli::{Args, BurnScopeArg, WindowAnchorArg, WindowScopeArg};
#[cfg(not(feature = "colors"))]
use claude_statusline::display::color_shim::ColorizeShim;
use claude_statusline::display::{print_header, print_json_output, print_text_output};
use claude_statusline::models::HookJson;
use claude_statusline::usage::{
    calc_context_from_any, calc_context_from_entries, calc_context_from_transcript, scan_usage,
};
use claude_statusline::usage_api::{get_usage_summary, UsageSummary};
use claude_statusline::utils::{
    auto_detect_plan_tier, claude_paths, read_stdin, resolve_plan_config,
};
use claude_statusline::window::{calculate_window_metrics, BurnScope, WindowScope};

fn main() -> Result<()> {
    let args = Args::parse();
    let stdin = read_stdin()?;
    if stdin.is_empty() {
        println!(
            "Claude Code\n{} {}",
            "❯".cyan(),
            "[waiting for valid input]".dimmed()
        );
        return Ok(());
    }
    let hook: HookJson = serde_json::from_slice(&stdin).context("parse hook json")?;

    // Compute metrics (from logs)
    let paths = claude_paths(args.claude_config_dir.as_deref());
    let (mut session_cost, today_cost, entries, latest_reset, api_key_source, rate_limit_info) =
        scan_usage(
            &paths,
            &hook.session_id,
            hook.workspace.project_dir.as_deref(),
            Some(&hook.model.id),
        )
        .unwrap_or((0.0, 0.0, Vec::new(), None, None, None));

    // By default prefer log-derived session cost for Pro/Max/Team usage; allow opting into
    // hook-provided totals via CLAUDE_SESSION_COST_SOURCE=hook
    if std::env::var("CLAUDE_SESSION_COST_SOURCE")
        .map(|s| s.eq_ignore_ascii_case("hook"))
        .unwrap_or(false)
    {
        if let Some(ref c) = hook.cost {
            if let Some(v) = c.total_cost_usd {
                session_cost = v;
            }
        }
    }
    let mut context = calc_context_from_transcript(
        Path::new(&hook.transcript_path),
        &hook.model.id,
        &hook.model.display_name,
    );
    let mut context_source: Option<&'static str> = None;
    if context.is_some() {
        context_source = Some("transcript");
    }

    // Git info from project_dir or current_dir (feature-gated)
    let git_info = {
        #[cfg(feature = "git")]
        {
            let git_dir = hook
                .workspace
                .project_dir
                .as_deref()
                .unwrap_or(&hook.workspace.current_dir);
            claude_statusline::git::read_git_info(Path::new(git_dir))
        }
        #[cfg(not(feature = "git"))]
        {
            None
        }
    };

    // cc-sessions integration (detects sessions state if present)
    let sessions_info = hook
        .workspace
        .project_dir
        .as_deref()
        .and_then(|p| claude_statusline::sessions::gather_sessions_info(Some(Path::new(p))));

    if !args.json {
        print_header(
            &hook,
            git_info.as_ref(),
            &args,
            api_key_source.as_deref(),
            sessions_info.as_ref(),
        );
    }

    // Plan resolution: CLI args override env; max_tokens overrides tier. Offline-only: no API calls.
    let (mut plan_tier_final, mut plan_max) = resolve_plan_config(&args);
    let plan_source = "inferred".to_string();
    let oauth_org_type: Option<String> = None;
    let oauth_rate_tier: Option<String> = None;

    // Calculate window metrics
    let now_utc = Utc::now();
    // Honor window anchor preference: set env consumed by window.rs
    match args.window_anchor {
        WindowAnchorArg::Provider => std::env::set_var("CLAUDE_WINDOW_ANCHOR", "provider"),
        WindowAnchorArg::Log => std::env::set_var("CLAUDE_WINDOW_ANCHOR", "log"),
    }
    let window_scope = match args.window_scope {
        WindowScopeArg::Global => WindowScope::Global,
        WindowScopeArg::Project => WindowScope::Project,
    };
    let burn_scope = match args.burn_scope {
        BurnScopeArg::Session => BurnScope::Session,
        BurnScopeArg::Global => BurnScope::Global,
    };

    let mut metrics = calculate_window_metrics(
        &entries,
        &hook.session_id,
        hook.workspace.project_dir.as_deref(),
        now_utc,
        latest_reset,
        window_scope,
        burn_scope,
        plan_max,
    );

    // Auto-detect plan tier if not configured (and not set via OAuth), based on actual usage
    if plan_tier_final.is_none() && metrics.noncache_tokens > 0.0 {
        if let Some(detected_tier) = auto_detect_plan_tier(metrics.noncache_tokens) {
            plan_tier_final = Some(detected_tier.clone());
            // Set appropriate max tokens for detected tier
            plan_max = match detected_tier.as_str() {
                "max20x" => Some(claude_statusline::utils::five_hour_base_tokens() * 20.0),
                "max5x" => Some(claude_statusline::utils::five_hour_base_tokens() * 5.0),
                "pro" => Some(claude_statusline::utils::five_hour_base_tokens()),
                _ => None,
            };

            // Recalculate metrics with the detected plan_max to get correct usage percentages
            metrics = calculate_window_metrics(
                &entries,
                &hook.session_id,
                hook.workspace.project_dir.as_deref(),
                now_utc,
                latest_reset,
                window_scope,
                burn_scope,
                plan_max,
            );
        }
    }

    let usage_summary: Option<UsageSummary> = get_usage_summary();
    let mut usage_percent_display = metrics.usage_percent;
    let projected_percent_display = metrics.projected_percent;
    let mut remaining_minutes_display = metrics.remaining_minutes;
    let mut latest_reset_effective = latest_reset;

    if let Some(summary) = usage_summary.as_ref() {
        if let Some(pct) = summary.window.utilization {
            usage_percent_display = Some(pct);
        }
        if let Some(reset) = summary.window.resets_at {
            latest_reset_effective = Some(
                reset - chrono::TimeDelta::hours(claude_statusline::utils::WINDOW_DURATION_HOURS),
            );
            let remaining_secs = (reset - now_utc).num_seconds();
            remaining_minutes_display = if remaining_secs > 0 {
                remaining_secs as f64 / 60.0
            } else {
                0.0
            };
        }
    }

    // Fallback context from entries if transcript lacked usage
    if context.is_none() {
        context = calc_context_from_entries(
            &entries,
            &hook.session_id,
            &hook.model.id,
            &hook.model.display_name,
        );
        if context.is_some() {
            context_source = Some("entries");
        }
    }
    if context.is_none() {
        context = calc_context_from_any(&entries, &hook.model.id, &hook.model.display_name);
        if context.is_some() {
            context_source = Some("latest");
        }
    }

    if args.json {
        // Machine-readable output for statusline consumption
        // Use the resolved/detected plan configuration
        let plan_tier_json = plan_tier_final.clone();
        let plan_max_json = plan_max;

        // Construct an active block descriptor for JSON start/end fields
        let (wb_start, wb_end) =
            claude_statusline::window::window_bounds(now_utc, latest_reset_effective);
        let active_block = claude_statusline::models::Block {
            start: wb_start,
            end: wb_end,
            actual_end: wb_end,
            is_active: true,
            is_gap: false,
            entries: Vec::new(),
            tokens: claude_statusline::models::TokenCounts::default(),
            cost: metrics.total_cost,
        };

        print_json_output(
            &hook,
            session_cost,
            today_cost,
            metrics.total_cost,
            metrics.total_tokens,
            metrics.noncache_tokens,
            metrics.tokens_input,
            metrics.tokens_output,
            metrics.tokens_cache_create,
            metrics.tokens_cache_read,
            metrics.session_tokens_input,
            metrics.session_tokens_output,
            metrics.session_tokens_cache_create,
            metrics.session_tokens_cache_read,
            metrics.web_search_requests,
            metrics.service_tier,
            usage_percent_display,
            projected_percent_display,
            remaining_minutes_display,
            Some(&active_block),
            latest_reset_effective,
            metrics.tpm,
            metrics.tpm_indicator,
            metrics.session_nc_tpm,
            metrics.global_nc_tpm,
            metrics.cost_per_hour,
            context,
            context_source,
            api_key_source,
            plan_tier_json,
            plan_max_json,
            git_info,
            rate_limit_info.as_ref(),
            oauth_org_type,
            oauth_rate_tier,
            plan_source,
            usage_summary.as_ref(),
            sessions_info.as_ref(),
        )?;
    } else {
        // Compute session-level cost per hour and line deltas from Claude's provided cost
        let (session_cph_opt, lines_delta_opt) = if let Some(ref c) = hook.cost {
            let sess_cph = c
                .total_duration_ms
                .and_then(|ms| {
                    if ms > 0 {
                        Some((ms as f64) / 3_600_000.0)
                    } else {
                        None
                    }
                })
                .and_then(|hrs| {
                    if hrs > 0.0 {
                        Some(session_cost / hrs)
                    } else {
                        None
                    }
                });
            let la = c.total_lines_added.unwrap_or(0);
            let lr = c.total_lines_removed.unwrap_or(0);
            (sess_cph, Some((la, lr)))
        } else {
            (None, None)
        };

        print_text_output(
            &args,
            &hook.model.id,
            &hook.model.display_name,
            session_cost,
            today_cost,
            metrics.total_cost,
            usage_percent_display,
            projected_percent_display,
            remaining_minutes_display,
            None,
            latest_reset_effective,
            metrics.tpm,
            metrics.tpm_indicator,
            metrics.cost_per_hour,
            context,
            metrics.tokens_input,
            metrics.tokens_output,
            metrics.tokens_cache_create,
            metrics.tokens_cache_read,
            metrics.session_tokens_input,
            metrics.session_tokens_output,
            metrics.session_tokens_cache_create,
            metrics.session_tokens_cache_read,
            metrics.web_search_requests,
            session_cph_opt,
            lines_delta_opt,
            rate_limit_info.as_ref(),
            usage_summary.as_ref(),
        );

        // Debug output if requested
        if args.debug {
            eprintln!();
            eprintln!("{}", "=== Debug Information ===".bright_black());
            eprintln!(
                "Session: ${:.2} (from: {})",
                session_cost,
                if hook.cost.is_some() {
                    "hook"
                } else {
                    "calculated"
                }
            );
            eprintln!(
                "Today: ${:.2} ({} entries scanned)",
                today_cost,
                entries.len()
            );
            eprintln!(
                "Window: ${:.2} (reset: {:?}, window_entries: {})",
                metrics.total_cost,
                latest_reset_effective.map(|r| r.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
                entries
                    .iter()
                    .filter(|e| {
                        let (start, end) = claude_statusline::window::window_bounds(
                            now_utc,
                            latest_reset_effective,
                        );
                        e.ts >= start && e.ts < end
                    })
                    .count()
            );
            if let Some(ctx) = context {
                eprintln!(
                    "Context: {} tokens ({}% of limit, source: {})",
                    ctx.0,
                    ctx.1,
                    context_source.unwrap_or("unknown")
                );
            }
            eprintln!(
                "Burn rates: session={:.1}/m, global={:.1}/m",
                metrics.session_nc_tpm, metrics.global_nc_tpm
            );
            eprintln!("Plan: tier={:?}, max={:?} tokens", args.plan_tier, plan_max);
            eprintln!("Files scanned: cutoff=48h (env: CLAUDE_SCAN_LOOKBACK_HOURS)");
            #[cfg(feature = "git")]
            if let Some(ref git) = git_info {
                eprintln!(
                    "Git: branch={}, clean={}, ahead={}, behind={}",
                    git.branch.as_deref().unwrap_or("detached"),
                    git.is_clean
                        .map(|c| if c { "yes" } else { "no" })
                        .unwrap_or("unknown"),
                    git.ahead.unwrap_or(0),
                    git.behind.unwrap_or(0)
                );
            }
            eprintln!(
                "Window scope: {:?}, Burn scope: {:?}",
                args.window_scope, args.burn_scope
            );
            eprintln!("{}", "========================".bright_black());
        }
    }
    Ok(())
}
