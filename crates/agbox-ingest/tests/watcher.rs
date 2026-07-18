#![allow(clippy::unwrap_used)]

use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::symlink,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use agbox_core::Provider;
use agbox_ingest::{
    SourceKey, WatchEventKind, WatchRoot, WatchSignalBridge, WatchedSource, WatcherCatalog,
    WatcherRuntime, WorkPriority, test_support::FixtureRuntime,
};
use time::OffsetDateTime;
use tokio::sync::watch;

#[derive(Debug)]
struct StaticCatalog {
    sources: Vec<WatchedSource>,
    visits: AtomicUsize,
}

impl WatcherCatalog for StaticCatalog {
    fn visit(
        &self,
        root_id: Option<&str>,
        relative_path: Option<&std::path::Path>,
        visitor: &mut dyn FnMut(&WatchedSource),
    ) {
        self.visits.fetch_add(1, Ordering::Relaxed);
        for source in &self.sources {
            if root_id.is_none_or(|id| id == source.root_id())
                && relative_path.is_none_or(|path| path == source.relative_path())
            {
                visitor(source);
            }
        }
    }
}

fn source_id(byte: u8) -> String {
    format!("source_{byte:032x}")
}

#[tokio::test]
async fn append_between_snapshot_and_watch_registration_is_captured_once() {
    let directory = tempfile::tempdir().unwrap();
    let root = WatchRoot::new("active", Provider::Codex, directory.path()).unwrap();
    let relative = std::path::PathBuf::from("session.jsonl");
    std::fs::write(directory.path().join(&relative), b"first\n").unwrap();
    let source = WatchedSource::new(
        SourceKey::new(source_id(1), 1).unwrap(),
        root.id(),
        relative,
        WorkPriority::Live,
    )
    .unwrap();
    let catalog = Arc::new(StaticCatalog {
        sources: vec![source],
        visits: AtomicUsize::new(0),
    });
    let (pause_tx, pause_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let startup = WatcherRuntime::new(vec![root], catalog)
        .unwrap()
        .start_with_registration_barrier(shutdown_rx, pause_tx, resume_rx);
    pause_rx.await.unwrap();
    OpenOptions::new()
        .append(true)
        .open(directory.path().join("session.jsonl"))
        .unwrap()
        .write_all(b"second\n")
        .unwrap();
    resume_tx.send(()).unwrap();
    let mut handle = startup.await.unwrap().unwrap();
    let item = handle.recv().await.unwrap();
    assert_eq!(item.target_offset, 13);
    assert_eq!(item.priority, WorkPriority::Live);
    assert!(handle.try_recv().is_err());
    shutdown_tx.send(true).unwrap();
    handle.join().await.unwrap();
}

#[tokio::test]
async fn one_thousand_duplicate_notifications_coalesce_to_one_exact_target() {
    let directory = tempfile::tempdir().unwrap();
    let root = WatchRoot::new("active", Provider::Codex, directory.path()).unwrap();
    let relative = std::path::PathBuf::from("session.jsonl");
    std::fs::write(directory.path().join(&relative), b"one\n").unwrap();
    let key = SourceKey::new(source_id(2), 1).unwrap();
    let source =
        WatchedSource::new(key.clone(), root.id(), relative.clone(), WorkPriority::Live).unwrap();
    let catalog = Arc::new(StaticCatalog {
        sources: vec![source],
        visits: AtomicUsize::new(0),
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut handle = WatcherRuntime::new(vec![root.clone()], catalog)
        .unwrap()
        .start_for_test(shutdown_rx)
        .await
        .unwrap();
    std::fs::write(directory.path().join(&relative), b"one\ntwo\n").unwrap();
    for _ in 0..1_000 {
        handle
            .inject(WatchEventKind::Write, [directory.path().join(&relative)])
            .unwrap();
    }
    let items = handle.reconcile_pending_for_test().await.unwrap();
    let mut queue = agbox_ingest::KeyedQueue::new(4);
    for item in items {
        queue
            .try_enqueue(item.key, item.target_offset, item.priority)
            .unwrap();
    }
    assert_eq!(queue.len(), 1);
    let item = queue.pop().unwrap();
    assert_eq!(item.key, key);
    assert_eq!(item.target_offset, 8);
    assert_eq!(item.priority, WorkPriority::Live);
    shutdown_tx.send(true).unwrap();
    handle.join().await.unwrap();
}

#[test]
fn watcher_bridge_is_bounded_and_coalesces_overflow() {
    let directory = tempfile::tempdir().unwrap();
    let root = WatchRoot::new("active", Provider::Codex, directory.path()).unwrap();
    let (bridge, mut receiver) = WatchSignalBridge::new(vec![root]);
    let paths = (0..17)
        .map(|index| directory.path().join(format!("{index}.jsonl")))
        .collect::<Vec<_>>();
    bridge.push_paths(WatchEventKind::Create, paths);
    assert_eq!(receiver.len(), 16);
    assert!(bridge.take_overflow());
    assert!(!bridge.take_overflow());
    for index in 0..256 {
        bridge.push_paths(
            WatchEventKind::Write,
            [directory.path().join(format!("full-{index}.jsonl"))],
        );
    }
    bridge.push_paths(
        WatchEventKind::Write,
        [directory.path().join("overflow.jsonl")],
    );
    assert!(bridge.take_overflow());
    while receiver.try_recv().is_ok() {}
}

#[test]
fn archive_move_targets_only_affected_verified_roots() {
    let directory = tempfile::tempdir().unwrap();
    let active_path = directory.path().join("active");
    let archive_path = directory.path().join("archive");
    std::fs::create_dir_all(&active_path).unwrap();
    std::fs::create_dir_all(&archive_path).unwrap();
    let active = WatchRoot::new("active", Provider::Codex, &active_path).unwrap();
    let archive = WatchRoot::new("archive", Provider::Codex, &archive_path).unwrap();
    let (bridge, mut receiver) = WatchSignalBridge::new(vec![active, archive]);
    bridge.push_paths(
        WatchEventKind::Rename,
        [
            active_path.join("old.jsonl"),
            archive_path.join("old.jsonl"),
        ],
    );
    let first = receiver.try_recv().unwrap();
    let second = receiver.try_recv().unwrap();
    assert_ne!(first.root_id(), second.root_id());
    assert_eq!(first.kind(), WatchEventKind::Rename);
    assert_eq!(second.kind(), WatchEventKind::Rename);
}

#[tokio::test]
async fn cancellation_is_graceful_and_old_source_append_remains_live() {
    let directory = tempfile::tempdir().unwrap();
    let root = WatchRoot::new("active", Provider::Codex, directory.path()).unwrap();
    let relative = std::path::PathBuf::from("2020/01/01/session.jsonl");
    std::fs::create_dir_all(directory.path().join("2020/01/01")).unwrap();
    std::fs::write(directory.path().join(&relative), b"old\n").unwrap();
    let source = WatchedSource::new(
        SourceKey::new(source_id(3), 1).unwrap(),
        root.id(),
        relative.clone(),
        WorkPriority::Archive,
    )
    .unwrap();
    let catalog = Arc::new(StaticCatalog {
        sources: vec![source],
        visits: AtomicUsize::new(0),
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut handle = WatcherRuntime::new(vec![root], catalog)
        .unwrap()
        .start_for_test(shutdown_rx)
        .await
        .unwrap();
    std::fs::write(directory.path().join(&relative), b"old\nappend\n").unwrap();
    handle
        .inject(WatchEventKind::Write, [directory.path().join(&relative)])
        .unwrap();
    let item = handle.recv_reconciled_for_test().await.unwrap();
    assert_eq!(item.priority, WorkPriority::Live);
    assert!(OffsetDateTime::now_utc().year() > 2020);
    shutdown_tx.send(true).unwrap();
    handle.join().await.unwrap();
}

#[tokio::test]
async fn duplicate_signals_commit_exactly_one_event_through_the_keyed_queue() {
    let runtime = FixtureRuntime::codex_records(1).await;
    runtime
        .run_signals(vec![runtime.source_size(); 1_000])
        .await
        .unwrap();
    assert_eq!(runtime.read().event_count().await.unwrap(), 1);
}

#[tokio::test]
async fn runtime_cancellation_drains_existing_work_after_watcher_closes_admission() {
    let fixture = FixtureRuntime::codex_records(1).await;
    let directory = tempfile::tempdir().unwrap();
    let root = WatchRoot::new("empty", Provider::Codex, directory.path()).unwrap();
    let catalog = Arc::new(StaticCatalog {
        sources: Vec::new(),
        visits: AtomicUsize::new(0),
    });
    let watcher = WatcherRuntime::new(vec![root], catalog).unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    shutdown_tx.send(true).unwrap();
    fixture
        .production_runtime()
        .run_watcher_until(watcher, shutdown_rx)
        .await
        .unwrap();
    assert_eq!(fixture.read().event_count().await.unwrap(), 1);
}

#[tokio::test]
async fn source_metadata_walk_rejects_symlink_escape() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), b"outside\n").unwrap();
    symlink(outside.path(), directory.path().join("session.jsonl")).unwrap();
    let root = WatchRoot::new("active", Provider::Codex, directory.path()).unwrap();
    let source = WatchedSource::new(
        SourceKey::new(source_id(4), 1).unwrap(),
        root.id(),
        std::path::PathBuf::from("session.jsonl"),
        WorkPriority::Live,
    )
    .unwrap();
    let catalog = Arc::new(StaticCatalog {
        sources: vec![source],
        visits: AtomicUsize::new(0),
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut handle = WatcherRuntime::new(vec![root], catalog)
        .unwrap()
        .start_for_test(shutdown_rx)
        .await
        .unwrap();
    handle
        .inject(
            WatchEventKind::Write,
            [directory.path().join("session.jsonl")],
        )
        .unwrap();
    assert!(
        handle
            .reconcile_pending_for_test()
            .await
            .unwrap()
            .is_empty()
    );
    shutdown_tx.send(true).unwrap();
    handle.join().await.unwrap();
}
