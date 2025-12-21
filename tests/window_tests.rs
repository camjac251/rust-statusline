use chrono::{DateTime, Utc};
use claude_statusline::models::Entry;
use claude_statusline::window::{BurnScope, WindowScope, calculate_window_metrics};

fn create_test_entry(
    ts: DateTime<Utc>,
    input: u64,
    output: u64,
    cost: f64,
    session_id: &str,
) -> Entry {
    Entry {
        ts,
        input,
        output,
        cache_create: 0,
        cache_read: 0,
        web_search_requests: 0,
        service_tier: None,
        cost,
        model: Some("test-model".to_string()),
        session_id: Some(session_id.to_string()),
        msg_id: None,
        req_id: None,
        project: Some("test-project".to_string()),
    }
}

#[test]
fn test_window_metrics_calculation() {
    let now = Utc::now();
    let entries = vec![
        create_test_entry(now - chrono::Duration::hours(4), 1000, 500, 0.1, "session1"),
        create_test_entry(
            now - chrono::Duration::hours(3),
            2000,
            1000,
            0.2,
            "session1",
        ),
        create_test_entry(
            now - chrono::Duration::hours(2),
            1500,
            750,
            0.15,
            "session2",
        ),
        create_test_entry(
            now - chrono::Duration::hours(1),
            3000,
            1500,
            0.3,
            "session1",
        ),
    ];

    let metrics = calculate_window_metrics(
        &entries,
        "session1",
        Some("test-project"),
        now,
        None,
        WindowScope::Global,
        BurnScope::Session,
    );

    assert_eq!(metrics.tokens_input, 7500);
    assert_eq!(metrics.tokens_output, 3750);
    assert_eq!(metrics.total_cost, 0.75);
}

#[test]
fn test_window_scope_project_filtering() {
    let now = Utc::now();
    let mut entries = vec![create_test_entry(
        now - chrono::Duration::hours(2),
        1000,
        500,
        0.1,
        "session1",
    )];

    // Add entry with different project
    let mut other_entry = create_test_entry(
        now - chrono::Duration::hours(1),
        2000,
        1000,
        0.2,
        "session1",
    );
    other_entry.project = Some("other-project".to_string());
    entries.push(other_entry);

    let metrics = calculate_window_metrics(
        &entries,
        "session1",
        Some("test-project"),
        now,
        None,
        WindowScope::Project,
        BurnScope::Session,
    );

    // Should only include first entry
    assert_eq!(metrics.tokens_input, 1000);
    assert_eq!(metrics.tokens_output, 500);
}

#[test]
fn test_burn_scope_session_vs_global() {
    let now = Utc::now();
    let entries = vec![
        create_test_entry(now - chrono::Duration::hours(3), 1000, 500, 0.1, "session1"),
        create_test_entry(
            now - chrono::Duration::hours(2),
            2000,
            1000,
            0.2,
            "session2",
        ),
        create_test_entry(
            now - chrono::Duration::minutes(30),
            1500,
            750,
            0.15,
            "session1",
        ),
    ];

    let session_metrics = calculate_window_metrics(
        &entries,
        "session1",
        None,
        now,
        None,
        WindowScope::Global,
        BurnScope::Session,
    );

    let global_metrics = calculate_window_metrics(
        &entries,
        "session1",
        None,
        now,
        None,
        WindowScope::Global,
        BurnScope::Global,
    );

    // Session burn should be different from global burn
    assert_ne!(session_metrics.tpm_indicator, global_metrics.tpm_indicator);
    // But totals should be the same
    assert_eq!(session_metrics.total_tokens, global_metrics.total_tokens);
}

#[test]
fn test_reset_anchor_window_calculation() {
    let now = Utc::now();
    let reset = now - chrono::Duration::hours(3);

    let entries = vec![
        create_test_entry(now - chrono::Duration::hours(4), 1000, 500, 0.1, "session1"),
        create_test_entry(
            now - chrono::Duration::hours(2),
            2000,
            1000,
            0.2,
            "session1",
        ),
    ];

    let metrics = calculate_window_metrics(
        &entries,
        "session1",
        None,
        now,
        Some(reset),
        WindowScope::Global,
        BurnScope::Session,
    );

    // Only the second entry should be included (after reset)
    assert_eq!(metrics.tokens_input, 2000);
    assert_eq!(metrics.tokens_output, 1000);
}

#[test]
fn test_empty_entries() {
    let now = Utc::now();
    let entries: Vec<Entry> = vec![];

    let metrics = calculate_window_metrics(
        &entries,
        "session1",
        None,
        now,
        None,
        WindowScope::Global,
        BurnScope::Session,
    );

    assert_eq!(metrics.total_cost, 0.0);
    assert_eq!(metrics.total_tokens, 0.0);
    assert_eq!(metrics.tpm, 0.0);
}
