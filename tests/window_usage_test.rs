use chrono::{TimeDelta, Timelike, Utc};

use claude_statusline::models::Entry;
use claude_statusline::window::{calculate_window_metrics, BurnScope, WindowScope};

#[test]
fn usage_percent_uses_noncache_tokens_and_projection_uses_global_nc_tpm() {
    let now = Utc::now()
        .with_nanosecond(0)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_minute(0)
        .unwrap();
    // Anchor 1 hour ago so window is [anchor, anchor+5h]
    let anchor = now - TimeDelta::hours(1);

    // Two entries inside the active window
    let e1 = Entry {
        ts: now - TimeDelta::minutes(30),
        input: 6_000,
        output: 4_000,
        cache_create: 20_000,
        cache_read: 10_000,
        web_search_requests: 0,
        service_tier: None,
        cost: 0.0,
        model: None,
        session_id: Some("s1".into()),
        msg_id: None,
        req_id: None,
        project: None,
    };
    let e2 = Entry {
        ts: now - TimeDelta::minutes(10),
        ..e1.clone()
    };
    let entries = vec![e1, e2];

    // Plan max 200k
    let plan_max = Some(200_000.0);
    let metrics = calculate_window_metrics(
        &entries,
        "s1",
        None,
        now,
        Some(anchor),
        WindowScope::Global,
        BurnScope::Global,
        plan_max,
    );

    // Non-cache = (input+output) summed over entries
    let noncache = (6_000 + 4_000) as f64 * 2.0; // 20_000
    let expected_usage = noncache * 100.0 / plan_max.unwrap();
    assert!(metrics.usage_percent.is_some());
    let up = metrics.usage_percent.unwrap();
    assert!((up - expected_usage).abs() < 1e-6);

    // Projected percent uses global_nc_tpm and remaining minutes
    let projected_nc = noncache + metrics.global_nc_tpm * metrics.remaining_minutes;
    let expected_proj = projected_nc * 100.0 / plan_max.unwrap();
    let pp = metrics.projected_percent.unwrap();
    assert!((pp - expected_proj).abs() < 1e-6);
}

#[test]
fn heuristic_block_start_is_floored_to_hour_when_no_anchor() {
    let now = Utc::now()
        .with_nanosecond(0)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_minute(0)
        .unwrap();
    // Two entries with no >=5h gap; earliest at an odd minute second
    let base = now - TimeDelta::hours(3);
    let e1 = Entry {
        ts: base.with_minute(17).unwrap().with_second(23).unwrap(),
        input: 1000,
        output: 1000,
        cache_create: 0,
        cache_read: 0,
        web_search_requests: 0,
        service_tier: None,
        cost: 0.0,
        model: None,
        session_id: Some("s1".into()),
        msg_id: None,
        req_id: None,
        project: None,
    };
    let e2 = Entry {
        ts: now - TimeDelta::hours(2),
        ..e1.clone()
    };
    let entries = vec![e1, e2];

    let metrics = calculate_window_metrics(
        &entries,
        "s1",
        None,
        now,
        None, // no anchor
        WindowScope::Global,
        BurnScope::Global,
        Some(200_000.0),
    );

    // Infer window start from remaining_minutes
    let inferred_start =
        now - (TimeDelta::hours(5) - TimeDelta::minutes(metrics.remaining_minutes as i64));
    assert_eq!(inferred_start.minute(), 0);
    assert_eq!(inferred_start.second(), 0);
}
