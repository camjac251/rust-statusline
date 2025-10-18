use chrono::{TimeDelta, Timelike, Utc};

use claude_statusline::models::Entry;
use claude_statusline::window::{calculate_window_metrics, BurnScope, WindowScope};

#[test]
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
    );

    // Infer window start from remaining_minutes
    let inferred_start =
        now - (TimeDelta::hours(5) - TimeDelta::minutes(metrics.remaining_minutes as i64));
    assert_eq!(inferred_start.minute(), 0);
    assert_eq!(inferred_start.second(), 0);
}
