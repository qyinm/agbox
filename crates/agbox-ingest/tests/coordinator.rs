#![allow(clippy::unwrap_used)]

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use agbox_ingest::IngestError;
use agbox_ingest::test_support::FixtureRuntime;
use agbox_ingest::{RetryClass, RetryClock, RetryPolicy, SourceHealth};
use tokio::sync::Semaphore;

#[derive(Debug)]
struct ScriptClock {
    faults: Mutex<VecDeque<RetryClass>>,
    sleeps: Mutex<Vec<Duration>>,
    submitted: Arc<Semaphore>,
    release: Arc<Semaphore>,
    block_submission_once: AtomicBool,
    block_before_submit_once: AtomicBool,
    before_submit: Arc<Semaphore>,
    continue_submit: Arc<Semaphore>,
    block_backoff_once: AtomicBool,
    backoff: Arc<Semaphore>,
    continue_backoff: Arc<Semaphore>,
}

impl ScriptClock {
    fn new(faults: impl IntoIterator<Item = RetryClass>, block_submission_once: bool) -> Self {
        Self {
            faults: Mutex::new(faults.into_iter().collect()),
            sleeps: Mutex::new(Vec::new()),
            submitted: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
            block_submission_once: AtomicBool::new(block_submission_once),
            block_before_submit_once: AtomicBool::new(false),
            before_submit: Arc::new(Semaphore::new(0)),
            continue_submit: Arc::new(Semaphore::new(0)),
            block_backoff_once: AtomicBool::new(false),
            backoff: Arc::new(Semaphore::new(0)),
            continue_backoff: Arc::new(Semaphore::new(0)),
        }
    }

    fn commit_barrier() -> Self {
        Self {
            block_before_submit_once: AtomicBool::new(true),
            block_backoff_once: AtomicBool::new(true),
            ..Self::new([], false)
        }
    }

    async fn wait_before_submit(&self) {
        self.before_submit.acquire().await.unwrap().forget();
    }

    fn continue_submit(&self) {
        self.continue_submit.add_permits(1);
    }

    async fn wait_backoff(&self) {
        self.backoff.acquire().await.unwrap().forget();
    }

    fn continue_backoff(&self) {
        self.continue_backoff.add_permits(1);
    }

    async fn wait_submitted(&self) {
        self.submitted.acquire().await.unwrap().forget();
    }

    fn release_submission(&self) {
        self.release.add_permits(1);
    }

    fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.lock().unwrap().clone()
    }
}

impl RetryClock for ScriptClock {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.sleeps.lock().unwrap().push(duration);
        if self.block_backoff_once.swap(false, Ordering::AcqRel) {
            Box::pin(async move {
                self.backoff.add_permits(1);
                self.continue_backoff.acquire().await.unwrap().forget();
            })
        } else {
            Box::pin(std::future::ready(()))
        }
    }

    fn before_attempt(&self) -> Pin<Box<dyn Future<Output = Option<RetryClass>> + Send + '_>> {
        Box::pin(std::future::ready(self.faults.lock().unwrap().pop_front()))
    }

    fn writer_submitted(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if self.block_submission_once.swap(false, Ordering::AcqRel) {
                self.submitted.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
        })
    }

    fn before_writer_submit(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if self.block_before_submit_once.swap(false, Ordering::AcqRel) {
                self.before_submit.add_permits(1);
                self.continue_submit.acquire().await.unwrap().forget();
            }
        })
    }
}

async fn hold_sqlite_lock_for_first_attempt(clock: Arc<ScriptClock>, database: std::path::PathBuf) {
    clock.wait_before_submit().await;
    let (ready_send, ready_receive) = std::sync::mpsc::sync_channel(0);
    let (release_send, release_receive) = std::sync::mpsc::sync_channel(0);
    let blocker = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(database).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        ready_send.send(()).unwrap();
        release_receive.recv().unwrap();
        connection.execute_batch("ROLLBACK").unwrap();
    });
    ready_receive.recv().unwrap();
    clock.continue_submit();
    clock.wait_backoff().await;
    release_send.send(()).unwrap();
    blocker.join().unwrap();
    clock.continue_backoff();
}

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
async fn first_oversized_complete_record_commits_one_fault_then_neighbor() {
    let changes = (0..=agbox_adapters::MAX_EVENTS_PER_RECORD)
        .map(|index| format!(r#"{{"path":"src/{index}.rs","kind":"update"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let oversized = format!(
        r#"{{"type":"event_msg","ordinal":3,"payload":{{"type":"item_completed","item":{{"type":"file_change","call_id":"call-1","status":"completed","changes":[{changes}]}}}}}}"#
    );
    let runtime = FixtureRuntime::records([
        r#"{"type":"session_meta","ordinal":1,"payload":{"history_mode":"paginated"}}"#.to_owned(),
        r#"{"type":"response_item","ordinal":2,"payload":{"type":"custom_tool_call","name":"apply_patch","input":"patch","call_id":"call-1"}}"#.to_owned(),
        oversized,
        r#"{"type":"event_msg","ordinal":4,"payload":{"type":"user_message","message":"neighbor"}}"#
            .to_owned(),
    ])
    .await;
    runtime.drain().await.unwrap();
    assert_eq!(runtime.read().fault_count().await.unwrap(), 1);
    assert!(runtime.read().event_count().await.unwrap() >= 1);
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
async fn injected_retry_schedule_reaches_real_exact_once_commit() {
    let clock = Arc::new(ScriptClock::new(
        [
            RetryClass::CursorConflict,
            RetryClass::Busy,
            RetryClass::StoreFailure,
        ],
        false,
    ));
    let policy = RetryPolicy::new(vec![
        Duration::from_millis(11),
        Duration::from_millis(23),
        Duration::from_millis(47),
    ])
    .unwrap();
    let runtime = FixtureRuntime::try_records_with_retry(
        [
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"first"}}"#,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"second"}}"#,
        ],
        4,
        policy,
        clock.clone(),
    )
    .await
    .unwrap();

    runtime
        .run_signals(vec![runtime.source_size(); 4])
        .await
        .unwrap();
    assert_eq!(
        clock.sleeps(),
        vec![
            Duration::from_millis(11),
            Duration::from_millis(23),
            Duration::from_millis(47)
        ]
    );
    assert_eq!(runtime.read().event_count().await.unwrap(), 2);
    assert_eq!(runtime.health().unwrap(), SourceHealth::Healthy);
}

#[tokio::test]
async fn production_four_worker_runtime_isolates_item_error_and_drains_peer() {
    assert_eq!(agbox_ingest::DECODER_WORKERS, 4);
    let runtime = FixtureRuntime::codex_records(3).await;
    runtime.run_with_unregistered_signal().await.unwrap();
    assert_eq!(runtime.read().event_count().await.unwrap(), 3);
    assert_eq!(
        runtime.read().cursor_offset().await.unwrap(),
        runtime.source_size()
    );
}

#[tokio::test]
async fn source_replacement_aborts_without_advancing_cursor() {
    let clock = Arc::new(ScriptClock::new([], false));
    let runtime = FixtureRuntime::try_records_with_retry(
        [
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"first"}}"#,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"second"}}"#,
        ],
        4,
        RetryPolicy::new(vec![Duration::from_millis(1), Duration::from_millis(2)]).unwrap(),
        clock.clone(),
    )
    .await
    .unwrap();
    runtime
        .replace_source(
            br#"{"type":"event_msg","payload":{"type":"user_message","message":"replacement"}}
"#,
        )
        .unwrap();

    let error = runtime.process_one().await.unwrap_err();
    assert!(matches!(error, IngestError::IdentityChanged));
    assert_eq!(clock.sleeps().len(), 2);
    assert_eq!(
        runtime.health().unwrap(),
        SourceHealth::Exhausted {
            attempts: 2,
            class: RetryClass::IdentityChanged
        }
    );
    assert_eq!(runtime.read().cursor_offset().await.unwrap(), 0);
    assert_eq!(runtime.read().event_count().await.unwrap(), 0);
}

#[tokio::test]
async fn cancellation_after_submission_requeues_committed_remainder() {
    let clock = Arc::new(ScriptClock::new([], true));
    let runtime = FixtureRuntime::try_records_with_retry(
        (0..1_250).map(|index| {
            format!(
                r#"{{"type":"event_msg","ordinal":{},"payload":{{"type":"user_message","message":"message-{}"}}}}"#,
                index + 1,
                index + 1
            )
        }),
        1,
        RetryPolicy::default(),
        clock.clone(),
    )
    .await
    .unwrap();
    {
        let processing = runtime.process_one();
        tokio::pin!(processing);
        tokio::select! {
            () = clock.wait_submitted() => {}
            result = &mut processing => panic!("process unexpectedly completed: {result:?}"),
        }
    }
    for _ in 0..3_000 {
        if runtime.read().cursor_offset().await.unwrap() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(runtime.read().event_count().await.unwrap(), 1_000);
    let remainder = runtime.process_one().await.unwrap();
    assert_eq!(remainder.committed_records, 250);
    assert_eq!(runtime.read().event_count().await.unwrap(), 1_250);
}

#[tokio::test]
async fn reserved_capacity_one_accepts_same_key_signal_after_partial_commit() {
    let clock = Arc::new(ScriptClock::new([], true));
    let runtime = FixtureRuntime::try_records_with_retry(
        (0..1_250).map(|index| {
            format!(
                r#"{{"type":"event_msg","ordinal":{},"payload":{{"type":"user_message","message":"message-{}"}}}}"#,
                index + 1,
                index + 1
            )
        }),
        1,
        RetryPolicy::default(),
        clock.clone(),
    )
    .await
    .unwrap();
    let process = runtime.process_one();
    let signal = async {
        clock.wait_submitted().await;
        runtime.enqueue().unwrap();
        clock.release_submission();
    };
    let (first, ()) = tokio::join!(process, signal);
    assert!(first.unwrap().requeued);
    assert_eq!(runtime.process_one().await.unwrap().committed_records, 250);
    assert_eq!(runtime.read().event_count().await.unwrap(), 1_250);
}

#[tokio::test]
async fn sqlite_busy_is_retried_without_duplicate_progress() {
    let clock = Arc::new(ScriptClock::commit_barrier());
    let runtime = FixtureRuntime::try_records_with_retry(
        [r#"{"type":"event_msg","payload":{"type":"user_message","message":"busy"}}"#],
        4,
        RetryPolicy::new(vec![
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(40),
        ])
        .unwrap(),
        clock.clone(),
    )
    .await
    .unwrap();
    let process = runtime.process_one();
    let lock =
        hold_sqlite_lock_for_first_attempt(clock.clone(), runtime.database_path().to_path_buf());
    let (report, ()) = tokio::join!(process, lock);
    let report = report.unwrap();
    assert!(
        !clock.sleeps().is_empty(),
        "prompt SQLite Busy must enter coordinator backoff"
    );
    assert_eq!(report.committed_records, 1);
    assert_eq!(runtime.read().event_count().await.unwrap(), 1);
    assert_eq!(
        runtime.read().cursor_offset().await.unwrap(),
        runtime.source_size()
    );
}

#[tokio::test]
async fn continuation_state_is_rebuilt_after_busy_retry_without_duplicates() {
    let mut records = vec![
        r#"{"type":"session_meta","ordinal":0,"payload":{"history_mode":"paginated"}}"#.to_owned(),
    ];
    for index in 0_u64..65 {
        let call_id = format!("call-{index:0>20}");
        let request_ordinal = 1 + index * 2;
        let result_ordinal = request_ordinal + 1;
        records.push(format!(
            r#"{{"type":"response_item","ordinal":{request_ordinal},"payload":{{"type":"custom_tool_call","name":"apply_patch","input":"patch","call_id":"{call_id}"}}}}"#
        ));
        records.push(format!(
            r#"{{"type":"event_msg","ordinal":{result_ordinal},"payload":{{"type":"patch_apply_end","call_id":"{call_id}","status":"completed","output":"done","path":"src/{index}.rs"}}}}"#
        ));
    }
    records.push(
        r#"{"type":"event_msg","ordinal":131,"payload":{"type":"task_complete"}}"#.to_owned(),
    );
    let clock = Arc::new(ScriptClock::commit_barrier());
    let runtime = FixtureRuntime::try_records_with_retry(
        records,
        4,
        RetryPolicy::new(vec![Duration::from_millis(13)]).unwrap(),
        clock.clone(),
    )
    .await
    .unwrap();
    let process = runtime.drain();
    let lock =
        hold_sqlite_lock_for_first_attempt(clock.clone(), runtime.database_path().to_path_buf());
    let (result, ()) = tokio::join!(process, lock);
    assert!(
        result.is_ok(),
        "continuation retry failed with {result:?}, health {:?}",
        runtime.health()
    );
    assert_eq!(clock.sleeps(), vec![Duration::from_millis(13)]);
    let event_count = runtime.read().event_count().await.unwrap();
    assert!(event_count > 130);
    let event_ids = runtime.read().event_ids().await.unwrap();
    let parser_state = runtime.read().parser_state().await.unwrap();
    assert_eq!(
        runtime.read().cursor_offset().await.unwrap(),
        runtime.source_size()
    );
    runtime.enqueue().unwrap();
    assert_eq!(runtime.process_one().await.unwrap().committed_records, 0);
    assert_eq!(runtime.read().event_count().await.unwrap(), event_count);
    assert_eq!(runtime.read().event_ids().await.unwrap(), event_ids);
    assert_eq!(runtime.read().parser_state().await.unwrap(), parser_state);
}
