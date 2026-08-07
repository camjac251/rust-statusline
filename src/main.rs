// Edition 2024 migration: allow collapsible_if for now, can refactor incrementally
#![allow(clippy::collapsible_if)]

use anyhow::{Context, Result};
use chrono::{Local, Utc};
use std::path::Path;

use claude_statusline::beads::get_beads_info;
use claude_statusline::cli::{Args, BurnScopeArg, WindowAnchorArg, WindowScopeArg};
use claude_statusline::display::{print_header, print_json_output, print_text_output};
use claude_statusline::gastown::get_gastown_info;
use claude_statusline::models::{Entry, HookJson, RateLimitInfo};
use claude_statusline::provenance::{CostProvenance, SessionCostSource, TodayCostSource};
use claude_statusline::subagent_statusline::{
    SubagentStatusInput, render_subagent_statusline, subagent_status_json,
};
use claude_statusline::usage::{
    DEFAULT_SCAN_LOOKBACK_HOURS, calc_context_from_entries, parse_active_transcript_state,
    scan_usage,
};
use claude_statusline::usage_api::{UsageSummary, get_usage_summary, resolve_usage_egress};
use claude_statusline::utils::{claude_paths, friendly_model_name, read_stdin};
use claude_statusline::window::{BurnScope, WindowScope, calculate_window_metrics};
use claude_statusline::workflow::get_session_activity;

const DB_SCAN_REFRESH_KEY: &str = "usage_scan:last_full_scan_at";
const DB_SCAN_LOCK_KEY: &str = "usage_scan:full_scan_lock";
const DB_SCAN_REFRESH_INTERVAL_SECONDS: i64 = 60;
const DB_SCAN_LOCK_TTL_SECONDS: i64 = 20;

fn db_scan_recently_refreshed(now_ts: i64) -> bool {
    claude_statusline::db::load_metadata(DB_SCAN_REFRESH_KEY)
        .ok()
        .flatten()
        .and_then(|entry| entry.value.parse::<i64>().ok())
        .is_some_and(|last_ts| {
            let age = now_ts.saturating_sub(last_ts);
            (0..DB_SCAN_REFRESH_INTERVAL_SECONDS).contains(&age)
        })
}

fn mark_db_scan_refreshed(now_ts: i64) {
    let _ = claude_statusline::db::store_metadata(DB_SCAN_REFRESH_KEY, &now_ts.to_string());
}

fn try_acquire_db_scan_lock() -> bool {
    claude_statusline::db::try_set_api_cache(DB_SCAN_LOCK_KEY, "1", DB_SCAN_LOCK_TTL_SECONDS)
        .unwrap_or(false)
}

fn release_db_scan_lock() {
    let _ = claude_statusline::db::set_api_cache(DB_SCAN_LOCK_KEY, "", 0);
}

fn merge_hook_usage_with_api(mut hook: UsageSummary, mut api: UsageSummary) -> UsageSummary {
    if api.stale {
        return hook;
    }

    hook.window.fill_missing_from(&api.window);
    hook.seven_day.fill_missing_from(&api.seven_day);
    api.window = hook.window;
    api.seven_day = hook.seven_day;
    api
}

fn session_today_cost_for_db(
    session_id: &str,
    scan_session_today_cost: f64,
    transcript_session_cost: Option<f64>,
    hook_session_cost: Option<f64>,
    entries: &[Entry],
) -> f64 {
    if scan_session_today_cost > 0.0 {
        return scan_session_today_cost;
    }

    let today = Local::now().date_naive();
    let has_non_today_session_entries = entries.iter().any(|entry| {
        entry.session_id.as_deref() == Some(session_id)
            && entry.ts.with_timezone(&Local).date_naive() != today
    });
    if has_non_today_session_entries {
        return scan_session_today_cost;
    }

    transcript_session_cost
        .filter(|cost| *cost > 0.0)
        .or_else(|| hook_session_cost.filter(|cost| *cost > 0.0))
        .unwrap_or(scan_session_today_cost)
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(ref command) = args.command {
        return claude_statusline::doctor::run_command(&args, command);
    }

    let stdin = read_stdin()?;
    if stdin.is_empty() {
        println!(
            "Claude Code\n{} {}",
            claude_statusline::tokens::ACCENT.paint("❯", false),
            claude_statusline::tokens::MUTED.dim("[waiting for valid input]", false)
        );
        return Ok(());
    }

    let hook_value: serde_json::Value =
        serde_json::from_slice(&stdin).context("parse hook json")?;
    if hook_value
        .get("tasks")
        .is_some_and(serde_json::Value::is_array)
    {
        let subagent_status: SubagentStatusInput =
            serde_json::from_value(hook_value).context("parse subagent statusline json")?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string(&subagent_status_json(&subagent_status))
                    .context("serialize subagent tasks json")?
            );
            return Ok(());
        }
        let now_ms = Utc::now().timestamp_millis().max(0) as u64;
        // Same detection as the footer: the agent panel and the status line are
        // one binary in one terminal, so they must not disagree on the palette.
        let truecolor = claude_statusline::display::is_truecolor_enabled(&args);
        for row in render_subagent_statusline(&subagent_status, now_ms, truecolor) {
            println!(
                "{}",
                serde_json::to_string(&row).context("serialize subagent statusline row")?
            );
        }
        return Ok(());
    }

    // Diagnostic: replace the footer with a column ruler so the width Claude Code
    // actually grants can be read off the screen. Placed after the agent-panel
    // branch because that surface answers in JSONL and gets its budget from the
    // payload rather than from the terminal.
    let ruler = claude_statusline::display::ruler_requested();
    if !matches!(ruler, claude_statusline::display::RulerRequest::Off) {
        claude_statusline::display::print_width_ruler(&ruler);
        return Ok(());
    }

    let mut hook: HookJson = serde_json::from_value(hook_value).context("parse hook json")?;

    // Normalize display_name: when Claude Code sends the raw model ID as the
    // display name (e.g. "claude-opus-4-6"), convert it to a friendly form
    // ("Opus 4.6") so every downstream consumer gets the right label.
    hook.model.display_name = friendly_model_name(&hook.model.id, &hook.model.display_name);

    let paths = claude_paths(args.claude_config_dir.as_deref());
    let transcript_path = Path::new(&hook.transcript_path);

    // Parse THIS session's transcript directly for authoritative session state.
    // This reads the specific transcript file (not the global scan) for:
    // - actual model in use (may differ from hook if /model was used)
    // - fast mode status (speed field from most recent API call)
    // - session cost (from SDK result messages)
    let active_transcript = parse_active_transcript_state(transcript_path);
    let session_state = &active_transcript.session;
    let prompt_cache_info = if !args.no_integrations_prompt_cache {
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

    // The modern hook schema ships the authoritative fast_mode flag; OR in the
    // transcript signal as a defensive fallback for any mid-turn skew.
    let is_fast_mode = hook.fast_mode || session_state.speed.as_deref() == Some("fast");
    let window_scope = match args.window_scope {
        WindowScopeArg::Global => WindowScope::Global,
        WindowScopeArg::Project => WindowScope::Project,
    };
    let burn_scope = match args.burn_scope {
        BurnScopeArg::Session => BurnScope::Session,
        BurnScopeArg::Global => BurnScope::Global,
    };
    let anchor_strategy = match args.window_anchor {
        WindowAnchorArg::Provider => claude_statusline::window::WindowAnchor::Provider,
        WindowAnchorArg::Log => claude_statusline::window::WindowAnchor::Log,
    };

    let mut session_cost = 0.0f64;
    let mut today_cost = 0.0f64;
    let mut entries: Vec<Entry> = Vec::new();
    let mut latest_reset: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut api_key_source: Option<String> = None;
    let mut rate_limit_info: Option<RateLimitInfo> = None;
    let mut sessions_count = 1;
    let mut today_cost_source = TodayCostSource::ScanFallback;
    let mut usage_entry_source = "scan";
    let db_fast_path_allowed =
        !args.no_subsystem_db_cache && !args.json && !args.provider_key_source;
    let scan_refresh_now = Utc::now().timestamp();
    let can_use_db_fast_path = db_fast_path_allowed && db_scan_recently_refreshed(scan_refresh_now);
    let mut needs_scan = !can_use_db_fast_path;
    let mut scan_lock_acquired = false;

    if can_use_db_fast_path {
        match claude_statusline::db::get_global_usage(
            &hook.session_id,
            &hook.workspace.project_dir,
            transcript_path,
            None,
            None,
        ) {
            Ok(global_usage) => {
                session_cost = global_usage.session_cost;
                today_cost = global_usage.global_today;
                sessions_count = global_usage.sessions_count;
                today_cost_source = TodayCostSource::DbGlobalUsage;
                entries = global_usage.entries;
                usage_entry_source = "db_cache";
            }
            Err(e) => {
                eprintln!("DB cache error (using scan_usage fallback): {}", e);
                needs_scan = true;
            }
        }
    }

    if needs_scan && db_fast_path_allowed {
        scan_lock_acquired = try_acquire_db_scan_lock();
        if !scan_lock_acquired {
            match claude_statusline::db::get_global_usage(
                &hook.session_id,
                &hook.workspace.project_dir,
                transcript_path,
                None,
                None,
            ) {
                Ok(global_usage) => {
                    session_cost = global_usage.session_cost;
                    today_cost = global_usage.global_today;
                    sessions_count = global_usage.sessions_count;
                    today_cost_source = TodayCostSource::DbGlobalUsage;
                    entries = global_usage.entries;
                    usage_entry_source = "db_cache";
                    needs_scan = false;
                }
                Err(e) => {
                    eprintln!("DB cache error while scan lock is held (using scan fallback): {e}");
                }
            }
        }
    }

    if needs_scan {
        let scan_result = scan_usage(
            &paths,
            &hook.session_id,
            Some(hook.workspace.project_dir.as_str()),
            Some(&hook.model.id),
        );
        let scan_succeeded = scan_result.is_ok();
        let (
            scan_session_cost,
            session_today_cost,
            scan_today_cost,
            scan_entries,
            scan_latest_reset,
            scan_api_key_source,
            scan_rate_limit_info,
        ) = scan_result.unwrap_or((0.0, 0.0, 0.0, Vec::new(), None, None, None));
        session_cost = scan_session_cost;
        today_cost = scan_today_cost;
        entries = scan_entries;
        latest_reset = scan_latest_reset;
        api_key_source = scan_api_key_source;
        rate_limit_info = scan_rate_limit_info;

        let db_session_today_cost = session_today_cost_for_db(
            &hook.session_id,
            session_today_cost,
            session_state.session_cost,
            Some(hook.cost.total_cost_usd),
            &entries,
        );

        // Global usage tracking: SQLite-based aggregation across all sessions.
        // Pass the best current-session today cost available so DB totals don't
        // lag behind Claude Code's live hook when transcript usage is sparse.
        if !args.no_subsystem_db_cache {
            match claude_statusline::db::get_global_usage(
                &hook.session_id,
                &hook.workspace.project_dir,
                transcript_path,
                Some(db_session_today_cost),
                scan_succeeded.then_some(entries.as_slice()),
            ) {
                Ok(global_usage) => {
                    today_cost = global_usage.global_today;
                    sessions_count = global_usage.sessions_count;
                    today_cost_source = TodayCostSource::DbGlobalUsage;
                    usage_entry_source = "scan";
                    if scan_succeeded {
                        mark_db_scan_refreshed(scan_refresh_now);
                    }
                }
                Err(e) => {
                    eprintln!("DB cache error (using scan_usage fallback): {}", e);
                }
            }
        }
        if scan_lock_acquired {
            release_db_scan_lock();
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
    } else if hook.cost.total_cost_usd > 0.0 {
        session_cost = hook.cost.total_cost_usd;
        session_cost_source = SessionCostSource::HookCost;
    }
    // Context window: prefer hook data except when the transcript proves a compact
    // boundary has reset the visible context and hook usage is still pre-compact.
    // If modern hook data says current_usage is null, avoid transcript/global
    // fallbacks unless a compact boundary gives us a fresh post-compact estimate.
    let mut context: Option<(u64, u32)> = None;
    let mut context_source: Option<&'static str> = None;
    let hook_has_live_context_usage = hook.context_window.current_usage.is_some();
    let transcript_context_detail =
        active_transcript.context_for(&hook.model.id, &hook.model.display_name);
    let transcript_context = transcript_context_detail.map(|detail| detail.as_tuple());

    if let Some(detail) = transcript_context_detail.filter(|detail| {
        detail.source == claude_statusline::usage::TranscriptContextSource::CompactEstimate
    }) {
        context = Some(detail.as_tuple());
        context_source = Some("transcript_compact");
    }

    // Priority 1: Use context_window from the modern hook schema unless a
    // compact boundary made the hook's last usage sample stale.
    if context.is_none() {
        let ctx_win = &hook.context_window;
        if let Some(ref usage) = ctx_win.current_usage {
            // Context tokens: input-side only (matches CLI calculation).
            // Output tokens don't count against the input context window.
            let total_tokens = usage.input_tokens
                + usage.cache_creation_input_tokens
                + usage.cache_read_input_tokens;
            if total_tokens > 0 {
                context = Some((total_tokens, ctx_win.used_percentage.min(100)));
                context_source = Some("hook");
            }
        }
    }

    // Priority 2: Parse transcript for usage only when the hook still has a
    // live usage-bearing message. If the modern hook says current_usage is null,
    // stale transcript samples should not resurrect pre-clear/pre-rewind context.
    if context.is_none() && hook_has_live_context_usage {
        context = transcript_context;
        if context.is_some() {
            context_source = Some(
                match transcript_context_detail.map(|detail| detail.source) {
                    Some(claude_statusline::usage::TranscriptContextSource::CompactEstimate) => {
                        "transcript_compact"
                    }
                    Some(claude_statusline::usage::TranscriptContextSource::ContextWarning) => {
                        "transcript_warning"
                    }
                    _ => "transcript",
                },
            );
        }
    }

    // Git info from project_dir or current_dir (feature-gated + runtime toggle)
    let git_info = {
        #[cfg(feature = "git")]
        {
            if args.no_subsystem_git {
                None
            } else {
                let git_dir = hook.workspace.project_dir.as_str();
                claude_statusline::git::read_git_info(Path::new(git_dir))
            }
        }
        #[cfg(not(feature = "git"))]
        {
            None
        }
    };

    // Beads issue tracker info (unless --no-subsystem-beads is set)
    let beads_info = if args.no_subsystem_beads {
        None
    } else {
        let beads_dir = hook.workspace.project_dir.as_str();
        get_beads_info(Path::new(beads_dir))
    };

    // Gas Town multi-agent info (unless --no-subsystem-gastown is set)
    let gastown_info = if args.no_subsystem_gastown {
        None
    } else {
        let gt_dir = hook.workspace.project_dir.as_str();
        get_gastown_info(Path::new(gt_dir))
    };

    // Workflow-progress and remote-agent tasks for this hook session, discovered
    // from the session dir beside the transcript.
    let session_activity = get_session_activity(transcript_path);

    // The modern hook schema ships context_window_size and lines added/removed.
    let context_limit_override =
        Some(hook.context_window.context_window_size).filter(|&size| size > 0);

    let la = hook.cost.total_lines_added;
    let lr = hook.cost.total_lines_removed;
    let lines_delta = if la != 0 || lr != 0 {
        Some((la, lr))
    } else {
        None
    };

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
            session_activity.as_ref(),
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

    // Usage + reset data priority:
    //   1. Hook rate_limits (from subscribers, no network call)
    //   2. OAuth API (cached, with negative cache on 429s)
    //   3. Transcript heuristic (scan_usage: "limit reached... resets 5am")
    let mut usage_summary: Option<UsageSummary> = None;
    let mut usage_percent_display = None;
    let projected_percent_display = None;
    let mut authoritative_remaining_minutes = None;
    // Start with None -- only fall back to scan heuristic if nothing authoritative
    let mut reset_at_display: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut window_anchor: Option<chrono::DateTime<chrono::Utc>> = None;

    /// Apply reset time from an authoritative source (hook or API)
    fn apply_reset(
        reset_dt: chrono::DateTime<chrono::Utc>,
        now: chrono::DateTime<chrono::Utc>,
        reset_at_out: &mut Option<chrono::DateTime<chrono::Utc>>,
        window_anchor_out: &mut Option<chrono::DateTime<chrono::Utc>>,
        remaining_minutes_out: &mut Option<f64>,
    ) {
        let normalized = claude_statusline::usage::normalize_reset_time(reset_dt);
        *reset_at_out = Some(normalized);
        *window_anchor_out = Some(
            normalized - chrono::TimeDelta::hours(claude_statusline::utils::WINDOW_DURATION_HOURS),
        );
        let remaining_secs = (normalized - now).num_seconds();
        *remaining_minutes_out = Some(if remaining_secs > 0 {
            remaining_secs as f64 / 60.0
        } else {
            0.0
        });
    }

    // Priority 1: Hook-provided rate_limits (from subscribers, no network call)
    // Only use if at least five_hour is present (empty rate_limits falls through to OAuth)
    //
    // Claude Code keeps resending the last message's rate_limits snapshot while a
    // session sits idle, so once the five-hour window rolls over the hook's
    // percentage and reset describe an expired window. Detect that and defer the
    // five-hour display to the live OAuth value instead of pinning stale numbers.
    let mut hook_five_hour_stale = false;
    if let Some(ref rl) = hook.rate_limits {
        if let Some(ref five) = rl.five_hour {
            let five_reset = five
                .resets_at
                .filter(|e| e.is_finite() && *e > 0.0)
                .and_then(|e| chrono::DateTime::from_timestamp(e as i64, 0))
                .map(claude_statusline::usage::normalize_reset_time);
            hook_five_hour_stale = five_reset.is_some_and(|reset| now_utc >= reset);

            if !hook_five_hour_stale {
                usage_percent_display = five.used_percentage;
                if let Some(reset) = five_reset {
                    apply_reset(
                        reset,
                        now_utc,
                        &mut reset_at_display,
                        &mut window_anchor,
                        &mut authoritative_remaining_minutes,
                    );
                }
            }

            // Build UsageSummary from hook data for display consumers. Leave the
            // five-hour window empty when the hook snapshot is stale so the OAuth
            // enrichment below can fill it with the live value.
            let mut summary = UsageSummary::default();
            if !hook_five_hour_stale {
                summary.window.utilization = five.used_percentage;
                summary.window.resets_at = five_reset;
            }
            if let Some(ref seven) = rl.seven_day {
                let seven_reset = seven
                    .resets_at
                    .filter(|e| e.is_finite() && *e > 0.0)
                    .and_then(|e| chrono::DateTime::from_timestamp(e as i64, 0));
                // The weekly window suffers the same idle-snapshot staleness as
                // five_hour: once it rolls over, the hook keeps resending the
                // expired percentage. Leave it empty so the OAuth enrichment
                // below supplies the live value instead of pinning the old one.
                let seven_stale = seven_reset.is_some_and(|reset| now_utc >= reset);
                if !seven_stale {
                    summary.seven_day.utilization = seven.used_percentage;
                    summary.seven_day.resets_at = seven_reset;
                }
            }
            usage_summary = Some(summary);
        }
    }

    // Priority 2: OAuth API
    // When hook provided rate_limits, we still call the API to get extra_usage
    // and model-specific breakdowns that the hook doesn't include.
    if !args.no_subsystem_usage_api {
        if usage_summary.is_none() {
            // No hook data at all; API is the primary source
            usage_summary = get_usage_summary(&paths, Some(&hook.model.id));
            if let Some(summary) = usage_summary.as_ref() {
                usage_percent_display = summary.window.utilization;
                if let Some(reset) = summary.window.resets_at {
                    apply_reset(
                        reset,
                        now_utc,
                        &mut reset_at_display,
                        &mut window_anchor,
                        &mut authoritative_remaining_minutes,
                    );
                }
            }
        } else if let Some(api_summary) = get_usage_summary(&paths, Some(&hook.model.id)) {
            // Hook utilization/reset wins, while a fresh OAuth response supplies
            // all API-only fields and fetch metadata. Stale enrichment is rejected
            // so it cannot make an expired hook window look current.
            if let Some(hook_summary) = usage_summary.take() {
                usage_summary = Some(merge_hook_usage_with_api(hook_summary, api_summary));
            }
        }

        // Fresh OAuth data fills any five-hour field the hook omitted. Adopt
        // each recovered value independently for the primary display.
        if let Some(summary) = usage_summary.as_ref() {
            if usage_percent_display.is_none() {
                usage_percent_display = summary.window.utilization;
            }
            if reset_at_display.is_none()
                && let Some(reset) = summary.window.resets_at
            {
                apply_reset(
                    reset,
                    now_utc,
                    &mut reset_at_display,
                    &mut window_anchor,
                    &mut authoritative_remaining_minutes,
                );
            }
        }
    }

    // Degraded fallback: the hook's five-hour window is stale and no live source
    // replaced it. Keep the last known percentage but mark it stale so the label
    // renders with a "~" prefix, and leave the reset unset so the countdown rolls
    // forward via the window anchor instead of freezing "0m" against a passed reset.
    if hook_five_hour_stale && usage_percent_display.is_none() {
        if let Some(five) = hook
            .rate_limits
            .as_ref()
            .and_then(|rl| rl.five_hour.as_ref())
        {
            usage_percent_display = five.used_percentage;
            if let Some(summary) = usage_summary.as_mut() {
                summary.window.utilization = five.used_percentage;
                summary.stale = true;
            }
        }
    }

    // Priority 3: Transcript heuristic (only if nothing authoritative above)
    if reset_at_display.is_none() {
        if let Some(reset) = latest_reset {
            let normalized = claude_statusline::usage::normalize_reset_time(reset);
            reset_at_display = Some(normalized);
            window_anchor = Some(
                normalized
                    - chrono::TimeDelta::hours(claude_statusline::utils::WINDOW_DURATION_HOURS),
            );
        }
    }

    let metrics = calculate_window_metrics(
        &entries,
        &hook.session_id,
        Some(hook.workspace.project_dir.as_str()),
        now_utc,
        window_anchor,
        window_scope,
        burn_scope,
        anchor_strategy,
    );
    let remaining_minutes_display =
        authoritative_remaining_minutes.unwrap_or(metrics.remaining_minutes);
    let active_block = claude_statusline::models::Block {
        start: metrics.start,
        end: metrics.end,
        actual_end: metrics.end,
        is_active: true,
        is_gap: false,
        entries: Vec::new(),
        tokens: claude_statusline::models::TokenCounts::default(),
        cost: metrics.total_cost,
    };

    // Fallback context from entries only when hook still has live usage; otherwise
    // modern null current_usage means the visible message list has no usage sample.
    if context.is_none() && hook_has_live_context_usage {
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
    if args.json {
        // Machine-readable output for statusline consumption.
        // Per-subagent cost breakdown for this session, enriched from the
        // agent-<id>.meta.json sidecars beside each subagent transcript.
        let subagent_breakdown = claude_statusline::models::build_subagent_breakdown(
            &entries,
            &hook.session_id,
            &hook.transcript_path,
        );

        print_json_output(
            &args,
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
            reset_at_display,
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
            session_activity.as_ref(),
        )?;
    } else {
        // Compute session-level cost per hour from Claude's provided cost
        let session_cph_opt = {
            let ms = hook.cost.total_duration_ms;
            if ms > 0 {
                let hrs = (ms as f64) / 3_600_000.0;
                if hrs > 0.0 {
                    Some(session_cost / hrs)
                } else {
                    None
                }
            } else {
                None
            }
        };

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
            Some(&active_block),
            window_anchor,
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
            session_activity.as_ref(),
        );

        // Debug output if requested
        if args.debug {
            eprintln!();
            eprintln!(
                "{}",
                claude_statusline::tokens::MUTED.dim("=== Debug Information ===", false)
            );
            eprintln!(
                "Session: ${:.2} (from: {})",
                session_cost,
                session_cost_source.as_str()
            );
            eprintln!(
                "Today: ${:.2} ({} entries from {})",
                today_cost,
                entries.len(),
                usage_entry_source
            );
            eprintln!(
                "Window: ${:.2} (reset: {:?}, window_entries: {})",
                metrics.total_cost,
                reset_at_display.map(|r| r.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
                entries
                    .iter()
                    .filter(|e| e.ts >= metrics.start && e.ts < metrics.end)
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
            if usage_entry_source == "scan" {
                let scan_lookback = std::env::var("CLAUDE_SCAN_LOOKBACK_HOURS")
                    .ok()
                    .filter(|value| value.parse::<i64>().is_ok())
                    .unwrap_or_else(|| DEFAULT_SCAN_LOOKBACK_HOURS.to_string());
                eprintln!(
                    "Files scanned: cutoff={}h (env: CLAUDE_SCAN_LOOKBACK_HOURS)",
                    scan_lookback
                );
            } else {
                eprintln!("Files scanned: skipped ({usage_entry_source})");
            }
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
            let usage_egress = resolve_usage_egress();
            match &usage_egress.extra_ca {
                Some(path) => {
                    eprintln!(
                        "Usage API egress: {} (extra CA: {})",
                        usage_egress.route, path
                    )
                }
                None => eprintln!("Usage API egress: {}", usage_egress.route),
            }
            eprintln!(
                "{}",
                claude_statusline::tokens::MUTED.dim("========================", false)
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(session_id: &str, ts: chrono::DateTime<Utc>) -> Entry {
        Entry {
            ts,
            input: 0,
            output: 0,
            cache_create: 0,
            cache_read: 0,
            web_search_requests: 0,
            speed: None,
            service_tier: None,
            cost: 0.0,
            model: None,
            session_id: Some(session_id.to_string()),
            msg_id: None,
            req_id: None,
            project: None,
            agent_id: None,
        }
    }

    #[test]
    fn db_today_cost_prefers_scanned_today_cost() {
        let cost = session_today_cost_for_db("session-1", 1.25, Some(2.0), Some(3.0), &[]);

        assert_eq!(cost, 1.25);
    }

    #[test]
    fn db_today_cost_uses_live_cost_when_scan_has_no_current_cost() {
        let cost = session_today_cost_for_db("session-1", 0.0, None, Some(3.0), &[]);

        assert_eq!(cost, 3.0);
    }

    #[test]
    fn db_today_cost_does_not_treat_cross_day_total_as_today_cost() {
        let yesterday = Utc::now() - chrono::TimeDelta::days(1);
        let entries = vec![test_entry("session-1", yesterday)];

        let cost = session_today_cost_for_db("session-1", 0.0, Some(2.0), Some(3.0), &entries);

        assert_eq!(cost, 0.0);
    }
}
