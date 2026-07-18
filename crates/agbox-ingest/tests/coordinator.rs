#![allow(clippy::unwrap_used)]

use agbox_ingest::IngestError;
use agbox_ingest::test_support::FixtureRuntime;
use agbox_store::StoreError;

#[tokio::test]
async fn coordinator_commits_at_most_one_thousand_records_per_chunk() {
    let runtime = FixtureRuntime::codex_records(1_250).await;
    let first = runtime.process_one().await.unwrap();
    assert_eq!(first.committed_records, 1_000);
    assert!(first.requeued);
    let second = runtime.process_one().await.unwrap();
    assert_eq!(second.committed_records, 250);
    assert!(!second.requeued);
    assert_eq!(runtime.read().event_count().await.unwrap(), 1_250);
}

#[tokio::test]
async fn one_malformed_record_is_quarantined_without_losing_neighbors() {
    let runtime = FixtureRuntime::records([
        r#"{"type":"event_msg","payload":{"type":"user_message","message":"first"}}"#,
        r#"{"type":"event_msg","payload":"#,
        r#"{"type":"event_msg","payload":{"type":"user_message","message":"third"}}"#,
    ])
    .await;
    runtime.drain().await.unwrap();
    assert_eq!(runtime.read().event_count().await.unwrap(), 2);
    assert_eq!(runtime.read().fault_count().await.unwrap(), 1);
    assert_eq!(
        runtime.read().cursor_offset().await.unwrap(),
        runtime.source_size()
    );
}

#[tokio::test]
async fn retry_after_commit_is_exactly_once_with_stable_event_ids() {
    let runtime = FixtureRuntime::codex_records(3).await;
    runtime.drain().await.unwrap();
    let before_ids = runtime.read().event_ids().await.unwrap();
    assert_eq!(before_ids.len(), 3);

    runtime.enqueue().unwrap();
    let retry = runtime.process_one().await.unwrap();
    assert_eq!(retry.committed_records, 0);
    assert_eq!(runtime.read().event_count().await.unwrap(), 3);
    assert_eq!(runtime.read().event_ids().await.unwrap(), before_ids);
}

#[tokio::test]
async fn conflicting_slices_abort_one_cursor_advance() {
    let runtime = FixtureRuntime::codex_records(2).await;
    let first_end = runtime.first_record_end().unwrap();
    let (short, full) = tokio::join!(
        runtime.process_target(first_end),
        runtime.process_target(runtime.source_size())
    );
    let conflicts = [&short, &full]
        .into_iter()
        .filter(|result| matches!(result, Err(IngestError::Store(StoreError::CursorConflict))))
        .count();
    assert_eq!(conflicts, 1);
    assert!(runtime.read().event_count().await.unwrap() <= 2);
}

#[tokio::test]
async fn source_replacement_aborts_without_advancing_cursor() {
    let runtime = FixtureRuntime::codex_records(2).await;
    runtime
        .replace_source(
            br#"{"type":"event_msg","payload":{"type":"user_message","message":"replacement"}}
"#,
        )
        .unwrap();

    let error = runtime.process_one().await.unwrap_err();
    assert!(matches!(error, IngestError::IdentityChanged));
    assert_eq!(runtime.read().cursor_offset().await.unwrap(), 0);
    assert_eq!(runtime.read().event_count().await.unwrap(), 0);
}

#[tokio::test]
async fn cancellation_before_writer_commit_leaves_cursor_unchanged() {
    let runtime = FixtureRuntime::codex_records(250).await;
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(1),
        runtime.process_target(runtime.source_size()),
    )
    .await;
    assert!(
        result.is_err(),
        "fixture decode must cross the cancellation point"
    );
    assert_eq!(runtime.read().cursor_offset().await.unwrap(), 0);
    assert_eq!(runtime.read().event_count().await.unwrap(), 0);

    runtime.drain().await.unwrap();
    assert_eq!(runtime.read().event_count().await.unwrap(), 250);
}

#[tokio::test]
async fn sqlite_busy_is_retried_without_duplicate_progress() {
    let runtime = FixtureRuntime::codex_records(1).await;
    let database = runtime.database_path().to_path_buf();
    let (ready_send, ready_receive) = std::sync::mpsc::sync_channel(0);
    let blocker = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(database).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        ready_send.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        connection.execute_batch("ROLLBACK").unwrap();
    });
    ready_receive.recv().unwrap();

    let report = runtime.process_one().await.unwrap();
    blocker.join().unwrap();
    assert_eq!(report.committed_records, 1);
    assert_eq!(runtime.read().event_count().await.unwrap(), 1);
    assert_eq!(
        runtime.read().cursor_offset().await.unwrap(),
        runtime.source_size()
    );
}
