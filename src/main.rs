// Edition 2024 migration: allow collapsible_if for now, can refactor incrementally
#![allow(clippy::collapsible_if)]

use anyhow::{Context, Result};
use chrono::Utc;
#[cfg(feature = "colors")]
use owo_colors::OwoColorize;
use std::path::Path;

use claude_statusline::beads::get_beads_info;
use claude_statusline::cli::{Args, BurnScopeArg, WindowAnchorArg, WindowScopeArg};
#[cfg(not(feature = "colors"))]
use claude_statusline::display::color_shim::ColorizeShim;
use claude_statusline::display::{print_header, print_json_output, print_text_output};
use claude_statusline::gastown::get_gastown_info;
use claude_statusline::models::HookJson;
use claude_statusline::provenance::{CostProvenance, SessionCostSource, TodayCostSource};
use claude_statusline::usage::{
    calc_context_from_entries, calc_context_from_transcript, parse_session_state, scan_usage,
};
use claude_statusline::usage_api::{UsageSummary, get_usage_summary};
use claude_statusline::utils::{claude_paths, friendly_model_name, read_stdin};
use claude_statusline::window::{BurnScope, WindowScope, calculate_window_metrics};

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(ref command) = args.command {
        return claude_statusline::doctor::run_command(&args, command);
    }

    let stdin = read_stdin()?;
    if stdin.is_empty() {
        println!(
            "Claude Code\n{} {}",
            "❯".cyan(),
            "[waiting for valid input]".dimmed()
        );
        return Ok(());
    }

    let mut hook: HookJson = serde_json::from_slice(&stdin).context("parse hook json")?;

    // Normalize display_name: when Claude Code sends the raw model ID as the
    // display name (e.g. "claude-opus-4-6"), convert it to a friendly form
    // ("Opus 4.6") so every downstream consumer gets the right label.
    hook.model.display_name = friendly_model_name(&hook.model.id, &hook.model.display_name);

    // Compute metrics (from logs)
    let paths = claude_paths(args.claude_config_dir.as_deref());
    let (
        mut session_cost,
        session_today_cost,
        mut today_cost,
        entries,
        latest_reset,
        api_key_source,
        rate_limit_info,
    ) = scan_usage(
        &paths,
        &hook.session_id,
        hook.workspace.project_dir.as_deref(),
        Some(&hook.model.id),
    )
    .unwrap_or((0.0, 0.0, 0.0, Vec::new(), None, None, None));

    // Parse THIS session's transcript directly for authoritative session state.
    // This reads the specific transcript file (not the global scan) for:
    // - actual model in use (may differ from hook if /model was used)
    // - fast mode status (speed field from most recent API call)
    // - session cost (from SDK result messages)
    let session_state = parse_session_state(Path::new(&hook.transcript_path));
    let prompt_cache_info = if args.prompt_cache {
        session_state.prompt_cache.clone().map(|mut info| {
            info.now = Utc::now();
            info.set_unknown_ttl_seconds(args.prompt_cache_ttl_seconds.unwrap_or(300));
            info
        })
    } else {
        None
    };

    if let Some(ref actual_model) = session_state.model {
        if *actual_model != hook.model.id {
            hook.model.id = actual_model.clone();
            hook.model.display_name = friendly_model_name(&hook.model.id, &hook.model.id);
        }
    }

    let is_fast_mode = session_state.speed.as_deref() == Some("fast");

    // Global usage tracking: SQLite-based aggregation across all sessions
    // Pass session_today_cost (this session only) for proper aggregation
    let mut sessions_count = 1;
    let mut today_cost_source = TodayCostSource::ScanFallback;
    if let Some(ref project_dir) = hook.workspace.project_dir {
        // Skip DB cache if --no-db-cache flag is set
        if !args.no_db_cache {
            match claude_statusline::db::get_global_usage(
                &hook.session_id,
                project_dir,
                Path::new(&hook.transcript_path),
                Some(session_today_cost),
                Some(&entries),
            ) {
                Ok(global_usage) => {
                    today_cost = global_usage.global_today;
                    sessions_count = global_usage.sessions_count;
                    today_cost_source = TodayCostSource::DbGlobalUsage;
                }
                Err(e) => {
                    eprintln!("DB cache error (using scan_usage fallback): {}", e);
                }
            }
        }
    }

    // Session cost priority:
    // 1. SDK result from this session's transcript (most authoritative, includes subagent costs)
    // 2. Hook-provided cost (from Claude Code's in-memory total, includes subagent costs)
    // 3. Scan-derived cost (summed from transcript entries including subagent files)
    let mut session_cost_source = SessionCostSource::TranscriptScan;
    if let Some(transcript_cost) = session_state.session_cost {
        if transcript_cost > 0.0 {
            session_cost = transcript_cost;
            session_cost_source = SessionCostSource::TranscriptResult;
        }
    } else if let Some(ref c) = hook.cost {
        if let Some(v) = c.total_cost_usd {
            if v > 0.0 {
                session_cost = v;
                session_cost_source = SessionCostSource::HookCost;
            }
        }
    }
    // Context window: prefer hook data (from Claude Code 2.0.69+), fallback to transcript parsing
    let mut context: Option<(u64, u32)> = None;
    let mut context_source: Option<&'static str> = None;

    // Priority 1: Use context_window from hook if available (most accurate)
    if let Some(ref ctx_win) = hook.context_window {
        if let Some(ref usage) = ctx_win.current_usage {
            // Context tokens: input-side only (matches CLI calculation)
            // Output tokens don't count against the input context window
            let input = usage.input_tokens.unwrap_or(0);
            let cache_create = usage.cache_creation_input_tokens.unwrap_or(0);
            let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
            let total_tokens = input + cache_create + cache_read;

            if total_tokens > 0 {
                // Prefer CLI's pre-calculated percentage (authoritative),
                // fall back to our own calculation
                let pct = if let Some(cli_pct) = ctx_win.used_percentage {
                    cli_pct
                } else {
                    let limit = ctx_win.context_window_size.unwrap_or_else(|| {
                        claude_statusline::utils::context_limit_for_model_display(
                            &hook.model.id,
                            &hook.model.display_name,
                        )
                    });
                    if limit > 0 {
                        ((total_tokens as f64 / limit as f64) * 100.0).round() as u32
                    } else {
                        0
                    }
                };
                context = Some((total_tokens, pct.min(100)));
                context_source = Some("hook");
            }
        }
    }

    // Priority 2: Parse transcript for usage (fallback for older Claude Code versions)
    if context.is_none() {
        context = calc_context_from_transcript(
            Path::new(&hook.transcript_path),
            &hook.model.id,
            &hook.model.display_name,
        );
        if context.is_some() {
            context_source = Some("transcript");
        }
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

    // Beads issue tracker info (unless --no-beads is set)
    let beads_info = if args.no_beads {
        None
    } else {
        let beads_dir = hook
            .workspace
            .project_dir
            .as_deref()
            .unwrap_or(&hook.workspace.current_dir);
        get_beads_info(Path::new(beads_dir))
    };

    // Gas Town multi-agent info (unless --no-gastown is set)
    let gastown_info = if args.no_gastown {
        None
    } else {
        let gt_dir = hook
            .workspace
            .project_dir
            .as_deref()
            .unwrap_or(&hook.workspace.current_dir);
        get_gastown_info(Path::new(gt_dir))
    };

    // Extract context_window_size from hook if available (for custom proxy models)
    // This is used in header, JSON, and text output paths
    let context_limit_override = hook
        .context_window
        .as_ref()
        .and_then(|cw| cw.context_window_size);

    // Extract lines delta from hook.cost for header display
    let lines_delta = hook.cost.as_ref().and_then(|c| {
        let la = c.total_lines_added.unwrap_or(0);
        let lr = c.total_lines_removed.unwrap_or(0);
        if la != 0 || lr != 0 {
            Some((la, lr))
        } else {
            None
        }
    });

    if !args.json {
        print_header(
            &hook,
            git_info.as_ref(),
            &args,
            api_key_source.as_deref(),
            lines_delta,
            beads_info.as_ref(),
            gastown_info.as_ref(),
            context_limit_override,
            is_fast_mode,
        );
    }

    let oauth_org_type: Option<String> = None;
    let oauth_rate_tier: Option<String> = None;
    let cost_provenance = CostProvenance {
        session_cost: session_cost_source,
        today_cost: today_cost_source,
        pricing: claude_statusline::pricing::pricing_source_for_model(&hook.model.id),
    };

    // Calculate window metrics
    let now_utc = Utc::now();
    // Honor window anchor preference: set env consumed by window.rs
    // SAFETY: We're in single-threaded startup code before any concurrent access
    match args.window_anchor {
        WindowAnchorArg::Provider => unsafe {
            std::env::set_var("CLAUDE_WINDOW_ANCHOR", "provider")
        },
        WindowAnchorArg::Log => unsafe { std::env::set_var("CLAUDE_WINDOW_ANCHOR", "log") },
    }
    let window_scope = match args.window_scope {
        WindowScopeArg::Global => WindowScope::Global,
        WindowScopeArg::Project => WindowScope::Project,
    };
    let burn_scope = match args.burn_scope {
        BurnScopeArg::Session => BurnScope::Session,
        BurnScopeArg::Global => BurnScope::Global,
    };

    let metrics = calculate_window_metrics(
        &entries,
        &hook.session_id,
        hook.workspace.project_dir.as_deref(),
        now_utc,
        latest_reset,
        window_scope,
        burn_scope,
    );

    // Usage + reset data priority:
    //   1. Hook rate_limits (from subscribers, no network call)
    //   2. OAuth API (cached, with negative cache on 429s)
    //   3. Transcript heuristic (scan_usage: "limit reached... resets 5am")
    let mut usage_summary: Option<UsageSummary> = None;
    let mut usage_percent_display = None;
    let projected_percent_display = None;
    let mut remaining_minutes_display = metrics.remaining_minutes;
    // Start with None -- only fall back to scan heuristic if nothing authoritative
    let mut latest_reset_effective: Option<chrono::DateTime<chrono::Utc>> = None;

    /// Apply reset time from an authoritative source (hook or API)
    fn apply_reset(
        reset_dt: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        latest_reset_out: &mut Option<chrono::DateTime<chrono::Utc>>,
        remaining_minutes_out: &mut f64,
    ) {
        let normalized = claude_statusline::usage::normalize_reset_time(reset_dt);
        *latest_reset_out = Some(
            normalized - chrono::TimeDelta::hours(claude_statusline::utils::WINDOW_DURATION_HOURS),
        );
        let remaining_secs = (normalized - now).num_seconds();
        *remaining_minutes_out = if remaining_secs > 0 {
            remaining_secs as f64 / 60.0
        } else {
            0.0
        };
    }

    // Priority 1: Hook-provided rate_limits (from subscribers, no network call)
    // Only use if at least five_hour is present (empty rate_limits falls through to OAuth)
    if let Some(ref rl) = hook.rate_limits {
        if let Some(ref five) = rl.five_hour {
            usage_percent_display = five.used_percentage;
            if let Some(epoch) = five.resets_at {
                if epoch.is_finite() && epoch > 0.0 {
                    if let Some(reset) = chrono::DateTime::from_timestamp(epoch as i64, 0) {
                        apply_reset(
                            reset,
                            now_utc,
                            &mut latest_reset_effective,
                            &mut remaining_minutes_display,
                        );
                    }
                }
            }

            // Build UsageSummary from hook data for display consumers
            let mut summary = UsageSummary::default();
            summary.window.utilization = five.used_percentage;
            summary.window.resets_at = five
                .resets_at
                .filter(|e| e.is_finite() && *e > 0.0)
                .and_then(|e| chrono::DateTime::from_timestamp(e as i64, 0));
            if let Some(ref seven) = rl.seven_day {
                summary.seven_day.utilization = seven.used_percentage;
                summary.seven_day.resets_at = seven
                    .resets_at
                    .filter(|e| e.is_finite() && *e > 0.0)
                    .and_then(|e| chrono::DateTime::from_timestamp(e as i64, 0));
            }
            usage_summary = Some(summary);
        }
    }

    // Priority 2: OAuth API
    // When hook provided rate_limits, we still call the API to get extra_usage
    // and model-specific breakdowns that the hook doesn't include.
    if usage_summary.is_none() {
        // No hook data at all; API is the primary source
        usage_summary = get_usage_summary(&paths, Some(&hook.model.id));
        if let Some(summary) = usage_summary.as_ref() {
            usage_percent_display = summary.window.utilization;
            if let Some(reset) = summary.window.resets_at {
                apply_reset(
                    reset,
                    now_utc,
                    &mut latest_reset_effective,
                    &mut remaining_minutes_display,
                );
            }
        }
    } else if let Some(api_summary) = get_usage_summary(&paths, Some(&hook.model.id)) {
        // Hook provided utilization/reset; enrich with API-only fields
        if let Some(ref mut summary) = usage_summary {
            if summary.extra_usage.is_none() {
                summary.extra_usage = api_summary.extra_usage;
            }
            if summary.seven_day_sonnet.utilization.is_none() {
                summary.seven_day_sonnet = api_summary.seven_day_sonnet;
            }
            if summary.seven_day_opus.utilization.is_none() {
                summary.seven_day_opus = api_summary.seven_day_opus;
            }
        }
    }

    // Priority 3: Transcript heuristic (only if nothing authoritative above)
    if latest_reset_effective.is_none() {
        latest_reset_effective = latest_reset;
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
    // Note: Removed calc_context_from_any fallback - it returned stale data from
    // previous sessions when starting a new session. Better to show no context
    // than misleading data from a different session.

    if args.json {
        // Machine-readable output for statusline consumption
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

        // Compute per-subagent cost breakdown for this session
        let subagent_breakdown = {
            let mut by_agent: std::collections::HashMap<String, (f64, u64, u64)> =
                std::collections::HashMap::new();
            for e in &entries {
                if e.session_id.as_deref() == Some(&hook.session_id) {
                    if let Some(ref aid) = e.agent_id {
                        let entry = by_agent.entry(aid.clone()).or_insert((0.0, 0, 0));
                        entry.0 += e.cost;
                        entry.1 += e.input + e.cache_create + e.cache_read;
                        entry.2 += e.output;
                    }
                }
            }
            if by_agent.is_empty() {
                None
            } else {
                let arr: Vec<serde_json::Value> = by_agent
                    .into_iter()
                    .map(|(aid, (cost, input, output))| {
                        serde_json::json!({
                            "agent_id": aid,
                            "cost_usd": (cost * 10000.0).round() / 10000.0,
                            "input_tokens": input,
                            "output_tokens": output,
                        })
                    })
                    .collect();
                Some(serde_json::Value::Array(arr))
            }
        };

        print_json_output(
            &hook,
            session_cost,
            today_cost,
            sessions_count,
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
            git_info,
            rate_limit_info.as_ref(),
            oauth_org_type,
            oauth_rate_tier,
            usage_summary.as_ref(),
            context_limit_override,
            beads_info.as_ref(),
            gastown_info.as_ref(),
            is_fast_mode,
            subagent_breakdown,
            Some(&cost_provenance),
            prompt_cache_info.as_ref(),
        )?;
    } else {
        // Compute session-level cost per hour from Claude's provided cost
        let session_cph_opt = hook.cost.as_ref().and_then(|c| {
            c.total_duration_ms
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
                })
        });

        print_text_output(
            &hook,
            git_info.as_ref(),
            &args,
            is_fast_mode,
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
            lines_delta,
            rate_limit_info.as_ref(),
            usage_summary.as_ref(),
            context_limit_override,
            Some(&cost_provenance),
            prompt_cache_info.as_ref(),
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
