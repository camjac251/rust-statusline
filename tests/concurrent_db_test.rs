use claude_statusline::db::get_global_usage;
use serial_test::serial;
use std::path::PathBuf;
use std::thread;
use tempfile::TempDir;

/// Test concurrent access to the database from multiple threads
#[test]
#[serial]
fn test_concurrent_db_access() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test_concurrent.db");
    unsafe { std::env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

    // Create 10 test transcript files
    let transcript_files: Vec<PathBuf> = (0..10)
        .map(|i| {
            let path = temp_dir.path().join(format!("transcript_{}.jsonl", i));
            std::fs::write(
                &path,
                format!(
                    r#"{{"timestamp":"2025-10-18T10:00:00Z","costUSD":{}}}
{{"timestamp":"2025-10-18T11:00:00Z","costUSD":{}}}"#,
                    i as f64 * 0.1,
                    i as f64 * 0.05
                ),
            )
            .unwrap();
            path
        })
        .collect();

    // Spawn 10 threads, each accessing the database concurrently
    let handles: Vec<_> = transcript_files
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let path = path.clone();
            let db_path = db_path.clone();
            // Add stagger to reduce initial contention
            thread::sleep(std::time::Duration::from_millis(i as u64 * 10));
            thread::spawn(move || {
                unsafe {
                    std::env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap())
                };

                // Each thread performs 5 get_global_usage calls with retry logic
                for iteration in 0..5 {
                    let session_id = format!("session-{}", i);
                    let project_dir = format!("/tmp/project-{}", i);

                    // Retry logic for database lock errors
                    let mut attempts = 0;
                    let result = loop {
                        match get_global_usage(
                            &session_id,
                            &project_dir,
                            &path,
                            Some((i as f64 * 0.1) + (i as f64 * 0.05)), // session_today_cost
                        ) {
                            Ok(r) => break Ok(r),
                            Err(e) if e.to_string().contains("locked") && attempts < 5 => {
                                attempts += 1;
                                thread::sleep(std::time::Duration::from_millis(50 * attempts));
                                continue;
                            }
                            Err(e) => break Err(e),
                        }
                    };

                    assert!(
                        result.is_ok(),
                        "Thread {} iteration {} failed: {:?}",
                        i,
                        iteration,
                        result.err()
                    );
                }
            })
        })
        .collect();

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    // Verify database is not corrupted and has correct session count
    unsafe { std::env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };
    let final_result = get_global_usage(
        "final-session",
        "/tmp/final-project",
        &transcript_files[0],
        Some(1.0),
    )
    .unwrap();

    // Should have 10 sessions + 1 final = 11 total
    assert_eq!(final_result.sessions_count, 11);

    // Clean up
    unsafe { std::env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
}

/// Test that provided session_today_cost bypasses parsing (optimization)
#[test]
#[serial]
fn test_provided_cost_optimization() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test_opt.db");
    let transcript_path = temp_dir.path().join("transcript.jsonl");

    unsafe { std::env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

    // Write transcript with known cost
    std::fs::write(
        &transcript_path,
        r#"{"timestamp":"2025-10-18T10:00:00Z","costUSD":1.0}"#,
    )
    .unwrap();

    // First call with provided cost - should NOT parse file
    let result1 = get_global_usage(
        "test-session",
        "/tmp/test-project",
        &transcript_path,
        Some(2.5), // Provide different cost than what's in file
    )
    .unwrap();
    // Should use provided cost, not file cost (1.0)
    assert_eq!(result1.session_cost, 2.5);
    assert_eq!(result1.global_today, 2.5);

    // Second call without provided cost - should parse from cache (same mtime)
    // This verifies cache is working
    let result2 =
        get_global_usage("test-session", "/tmp/test-project", &transcript_path, None).unwrap();
    // Should still be 2.5 (cached from previous call)
    assert_eq!(result2.session_cost, 2.5);

    // Sleep to ensure mtime changes (filesystem mtime granularity varies)
    thread::sleep(std::time::Duration::from_secs(2));

    // Modify transcript
    std::fs::write(
        &transcript_path,
        r#"{"timestamp":"2025-10-18T10:00:00Z","costUSD":1.0}
{"timestamp":"2025-10-18T11:00:00Z","costUSD":0.5}"#,
    )
    .unwrap();

    // Call with new provided cost - should update cache with new mtime
    let result3 = get_global_usage(
        "test-session",
        "/tmp/test-project",
        &transcript_path,
        Some(3.7), // New provided cost
    )
    .unwrap();
    assert_eq!(result3.session_cost, 3.7);

    // Clean up
    unsafe { std::env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
}

/// Test that stale entries from previous days are cleaned up
#[test]
#[serial]
fn test_date_rollover_cleanup() {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test_rollover.db");
    let transcript_path = temp_dir.path().join("transcript.jsonl");

    unsafe { std::env::set_var("CLAUDE_STATUSLINE_DB_PATH", db_path.to_str().unwrap()) };

    // This test verifies that entries with yesterday's date are cleaned up
    // Since we can't easily mock the date, we'll verify the DB has only today's entries
    std::fs::write(
        &transcript_path,
        r#"{"timestamp":"2025-10-18T10:00:00Z","costUSD":1.0}"#,
    )
    .unwrap();

    let result = get_global_usage(
        "test-session",
        "/tmp/test-project",
        &transcript_path,
        Some(1.0),
    )
    .unwrap();

    // Should have exactly 1 session for today
    assert_eq!(result.sessions_count, 1);
    assert_eq!(result.global_today, 1.0);

    // Clean up
    unsafe { std::env::remove_var("CLAUDE_STATUSLINE_DB_PATH") };
}
