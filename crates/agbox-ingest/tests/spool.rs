#![allow(clippy::unwrap_used)]

use std::{
    io::Cursor,
    os::unix::fs::{PermissionsExt, symlink},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use agbox_core::Provider;
use agbox_ingest::{
    HookEventKind, HookSignal, HookSourceVerifier, HookSpool, HookSpoolLimits,
    MAX_HOOK_PAYLOAD_BYTES, MAX_SPOOL_ENTRY_BYTES, SourceKey, SpoolError,
};
use agbox_store::MemoryKeyProvider;
use time::OffsetDateTime;

#[derive(Debug)]
struct Verifier {
    key: SourceKey,
    expected: std::path::PathBuf,
}

impl HookSourceVerifier for Verifier {
    fn verify(
        &self,
        _provider: Provider,
        path: &std::path::Path,
        target_size: u64,
    ) -> Option<(SourceKey, u64)> {
        (path == self.expected).then(|| (self.key.clone(), target_size))
    }
}

fn fixture(index: u8) -> HookSignal {
    HookSignal::fixture_for_test(
        Provider::Codex,
        HookEventKind::SessionEnd,
        format!("session-{index}").as_bytes(),
        &SourceKey::new(format!("source_{index:032x}"), 1).unwrap(),
        OffsetDateTime::UNIX_EPOCH,
        u64::from(index),
    )
    .unwrap()
}

fn spool(directory: &tempfile::TempDir, limits: HookSpoolLimits) -> HookSpool {
    HookSpool::with_limits(
        directory.path(),
        Arc::new(MemoryKeyProvider::fixed([7; 32])),
        limits,
    )
    .unwrap()
}

#[test]
fn hook_payload_is_streamed_normalized_and_sensitive_fields_are_discarded() {
    let source = std::path::PathBuf::from("/verified/session.jsonl");
    let verifier = Verifier {
        key: SourceKey::new("source_00000000000000000000000000000001", 1).unwrap(),
        expected: source.clone(),
    };
    let payload = format!(
        r#"{{"provider":"codex","hook_event_name":"session_end","session_id":"native-secret","transcript_path":"{}","target_size":42,"prompt":"DO NOT RETAIN","tool_input":{{"password":"secret"}},"environment":{{"TOKEN":"value"}}}}"#,
        source.display()
    );
    let signal = HookSignal::from_reader(
        Cursor::new(payload.as_bytes()),
        &verifier,
        OffsetDateTime::UNIX_EPOCH,
    )
    .unwrap();
    let wire = serde_json::to_vec(&signal).unwrap();
    assert!(wire.len() <= MAX_SPOOL_ENTRY_BYTES);
    for secret in [
        b"native-secret".as_slice(),
        b"DO NOT RETAIN",
        b"password",
        b"TOKEN",
    ] {
        assert!(!wire.windows(secret.len()).any(|window| window == secret));
    }
}

#[test]
fn malformed_oversized_and_unverified_hook_payloads_are_rejected() {
    let verifier = Verifier {
        key: SourceKey::new("source_00000000000000000000000000000001", 1).unwrap(),
        expected: std::path::PathBuf::from("/verified/session.jsonl"),
    };
    assert!(matches!(
        HookSignal::from_reader(
            Cursor::new(b"{".as_slice()),
            &verifier,
            OffsetDateTime::UNIX_EPOCH
        ),
        Err(SpoolError::InvalidPayload)
    ));
    assert!(matches!(
        HookSignal::from_reader(
            Cursor::new(vec![b' '; MAX_HOOK_PAYLOAD_BYTES + 1]),
            &verifier,
            OffsetDateTime::UNIX_EPOCH
        ),
        Err(SpoolError::PayloadTooLarge)
    ));
    let unverified = br#"{"provider":"codex","hook_event_name":"session_end","session_id":"s","transcript_path":"/wrong/session.jsonl","target_size":1}"#;
    assert!(matches!(
        HookSignal::from_reader(
            Cursor::new(unverified),
            &verifier,
            OffsetDateTime::UNIX_EPOCH
        ),
        Err(SpoolError::UnverifiedSource)
    ));
}

#[tokio::test]
async fn spool_is_encrypted_owner_only_and_has_no_plaintext() {
    let directory = tempfile::tempdir().unwrap();
    let spool = spool(&directory, HookSpoolLimits::default());
    let signal = fixture(1);
    spool.enqueue(&signal).unwrap();
    let entries = spool.entry_paths().unwrap();
    assert_eq!(entries.len(), 1);
    let bytes = std::fs::read(&entries[0]).unwrap();
    assert!(bytes.starts_with(b"AGBX\x01"));
    assert!(!bytes.windows(7).any(|window| window == b"session"));
    assert_eq!(
        std::fs::metadata(&entries[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(directory.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn spool_preserves_existing_entries_at_count_byte_and_entry_caps() {
    for limits in [
        HookSpoolLimits {
            max_entries: 1,
            max_bytes: 1_000_000,
            max_entry_bytes: MAX_SPOOL_ENTRY_BYTES,
        },
        HookSpoolLimits {
            max_entries: 10,
            max_bytes: 1,
            max_entry_bytes: MAX_SPOOL_ENTRY_BYTES,
        },
        HookSpoolLimits {
            max_entries: 10,
            max_bytes: 1_000_000,
            max_entry_bytes: 8,
        },
    ] {
        let directory = tempfile::tempdir().unwrap();
        let spool = spool(&directory, limits);
        let first = fixture(1);
        let first_result = spool.enqueue(&first);
        if first_result.is_ok() {
            assert!(matches!(spool.enqueue(&fixture(2)), Err(SpoolError::Full)));
            assert_eq!(spool.entry_paths().unwrap().len(), 1);
        } else {
            assert!(matches!(first_result, Err(SpoolError::Full)));
            assert!(spool.entry_paths().unwrap().is_empty());
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let default_spool = spool(&directory, HookSpoolLimits::default());
    default_spool.enqueue(&fixture(7)).unwrap();
    let first_path = default_spool.entry_paths().unwrap().remove(0);
    let exact_bytes = std::fs::metadata(first_path).unwrap().len();
    let exact_spool = spool(
        &directory,
        HookSpoolLimits {
            max_entries: 2,
            max_bytes: exact_bytes,
            max_entry_bytes: MAX_SPOOL_ENTRY_BYTES,
        },
    );
    assert!(matches!(
        exact_spool.enqueue(&fixture(8)),
        Err(SpoolError::Full)
    ));
    assert_eq!(exact_spool.entry_paths().unwrap().len(), 1);
}

#[tokio::test]
async fn drain_is_lexical_and_deletes_only_after_commit() {
    let directory = tempfile::tempdir().unwrap();
    let spool = spool(&directory, HookSpoolLimits::default());
    spool.enqueue(&fixture(3)).unwrap();
    spool.enqueue(&fixture(1)).unwrap();
    spool.enqueue(&fixture(2)).unwrap();
    let committed = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&committed);
    spool
        .drain(move |signal| {
            let observed = Arc::clone(&observed);
            async move {
                observed.lock().unwrap().push(signal.target_size());
                Ok::<(), ()>(())
            }
        })
        .await
        .unwrap();
    assert_eq!(*committed.lock().unwrap(), vec![3, 1, 2]);
    assert!(spool.entry_paths().unwrap().is_empty());

    spool.enqueue(&fixture(4)).unwrap();
    spool.enqueue(&fixture(5)).unwrap();
    spool.enqueue(&fixture(6)).unwrap();
    let result = spool
        .drain(|signal| async move {
            if signal.target_size() == 5 {
                Err(())
            } else {
                Ok(())
            }
        })
        .await;
    assert!(matches!(result, Err(SpoolError::CommitFailed)));
    assert_eq!(spool.entry_paths().unwrap().len(), 2);
}

#[tokio::test]
async fn invalid_encrypted_entry_is_retained() {
    let directory = tempfile::tempdir().unwrap();
    let spool = spool(&directory, HookSpoolLimits::default());
    spool.enqueue(&fixture(1)).unwrap();
    let path = spool.entry_paths().unwrap().remove(0);
    std::fs::write(&path, b"AGBX\x01invalid").unwrap();
    assert!(matches!(
        spool.drain(|_| async { Ok::<(), ()>(()) }).await,
        Err(SpoolError::InvalidEntry)
    ));
    assert!(path.exists());
}

#[test]
fn symlink_spool_directory_is_rejected_without_touching_target() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = directory.path().join("spool");
    symlink(&target, &link).unwrap();
    assert!(matches!(
        HookSpool::new(&link, Arc::new(MemoryKeyProvider::fixed([7; 32]))),
        Err(SpoolError::InvalidEntry)
    ));
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[tokio::test]
async fn stale_partial_temp_is_cleaned_and_never_blocks_lexical_drain() {
    let directory = tempfile::tempdir().unwrap();
    let initial_spool = spool(&directory, HookSpoolLimits::default());
    initial_spool.enqueue(&fixture(1)).unwrap();
    drop(initial_spool);
    let partial = directory
        .path()
        .join(".00000000000000000000-0000000000000000.agbx.tmp");
    std::fs::write(&partial, b"AGBX\x01partial").unwrap();
    let reopened = spool(&directory, HookSpoolLimits::default());
    assert!(!partial.exists());
    assert_eq!(
        reopened
            .drain(|_| async { Ok::<(), ()>(()) })
            .await
            .unwrap(),
        1
    );
    assert!(reopened.entry_paths().unwrap().is_empty());
}

#[tokio::test]
async fn two_spool_instances_drain_each_entry_only_after_one_commit() {
    let directory = tempfile::tempdir().unwrap();
    let first = Arc::new(spool(&directory, HookSpoolLimits::default()));
    let second = Arc::new(spool(&directory, HookSpoolLimits::default()));
    for index in 1..=8 {
        first.enqueue(&fixture(index)).unwrap();
    }
    let commits = Arc::new(AtomicUsize::new(0));
    let left_commits = Arc::clone(&commits);
    let left = {
        let first = Arc::clone(&first);
        tokio::spawn(async move {
            first
                .drain(move |_| {
                    let commits = Arc::clone(&left_commits);
                    async move {
                        commits.fetch_add(1, Ordering::Relaxed);
                        Ok::<(), ()>(())
                    }
                })
                .await
        })
    };
    let right_commits = Arc::clone(&commits);
    let right = {
        let second = Arc::clone(&second);
        tokio::spawn(async move {
            second
                .drain(move |_| {
                    let commits = Arc::clone(&right_commits);
                    async move {
                        commits.fetch_add(1, Ordering::Relaxed);
                        Ok::<(), ()>(())
                    }
                })
                .await
        })
    };
    let drained = left.await.unwrap().unwrap() + right.await.unwrap().unwrap();
    assert_eq!(drained, 8);
    assert_eq!(commits.load(Ordering::Relaxed), 8);
    assert!(first.entry_paths().unwrap().is_empty());
}

#[tokio::test]
async fn directory_rebinding_cannot_redirect_descriptor_relative_spool_ops() {
    let parent = tempfile::tempdir().unwrap();
    let original = parent.path().join("spool");
    std::fs::create_dir(&original).unwrap();
    let spool = HookSpool::new(&original, Arc::new(MemoryKeyProvider::fixed([7; 32]))).unwrap();
    let moved = parent.path().join("bound-spool");
    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700)).unwrap();

    spool.enqueue(&fixture(9)).unwrap();
    assert_eq!(
        spool.drain(|_| async { Ok::<(), ()>(()) }).await.unwrap(),
        1
    );
    assert_eq!(std::fs::read_dir(&original).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_dir(&moved)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != ".spool.lock")
            .count(),
        0
    );
}
