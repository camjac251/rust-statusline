use anyhow::{Context, Result};
use chrono::Utc;
#[cfg(feature = "colors")]
use owo_colors::OwoColorize;
use std::path::Path;

use claude_statusline::cli::{Args, BurnScopeArg, WindowScopeArg};
#[cfg(not(feature = "colors"))]
use claude_statusline::display::color_shim::ColorizeShim;
use claude_statusline::display::{print_header, print_json_output, print_text_output};
use claude_statusline::models::HookJson;
use claude_statusline::usage::{
    calc_context_from_any, calc_context_from_entries, calc_context_from_transcript, scan_usage,
};
use claude_statusline::utils::{claude_paths, read_stdin, resolve_plan_config};
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

    // Compute metrics
    let paths = claude_paths(args.claude_config_dir.as_deref());
    let (session_cost, today_cost, entries, latest_reset, api_key_source) = scan_usage(
        &paths,
        &hook.session_id,
        hook.workspace.project_dir.as_deref(),
    )
    .unwrap_or((0.0, 0.0, Vec::new(), None, None));
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

    if !args.json {
        print_header(
            &hook,
            git_info.as_ref(),
            &args,
            api_key_source.as_deref(),
        );
    }

    // Plan resolution: CLI args override env; max_tokens overrides tier.
    let (_plan_tier_final, plan_max) = resolve_plan_config(&args);

    // Calculate window metrics
    let now_utc = Utc::now();
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
        plan_max,
    );

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
        // Use same plan configuration resolution
        let (plan_tier_json, plan_max_json) = resolve_plan_config(&args);

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
            metrics.web_search_requests,
            metrics.service_tier,
            metrics.usage_percent,
            metrics.projected_percent,
            metrics.remaining_minutes,
            None,
            latest_reset,
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
        )?;
    } else {
        print_text_output(
            &args,
            session_cost,
            today_cost,
            metrics.total_cost,
            metrics.usage_percent,
            metrics.projected_percent,
            metrics.remaining_minutes,
            None,
            latest_reset,
            metrics.tpm,
            metrics.tpm_indicator,
            metrics.cost_per_hour,
            context,
            metrics.tokens_input,
            metrics.tokens_output,
            metrics.tokens_cache_create,
            metrics.tokens_cache_read,
            metrics.web_search_requests,
        );
    }
    Ok(())
}
