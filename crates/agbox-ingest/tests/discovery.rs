#![allow(clippy::expect_used, clippy::unwrap_used)]

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use agbox_adapters::{RootClass, RootSpec};
use agbox_core::Provider;
use agbox_ingest::{
    DISCOVERY_ENTRIES_PER_YIELD, DiscoveryWalker, HistoryDecision, HistoryPolicy, ProjectResolver,
    VerifiedOpenError, VerifiedSourceOpener, deduplicate_overlapping_sources,
};
use agbox_store::{CryptoError, KeyProvider, MemoryKeyProvider, SourceRegistration, StoreRuntime};
use rusqlite::Connection;
use time::OffsetDateTime;
use zeroize::Zeroizing;

fn root_spec(path: &Path) -> RootSpec {
    RootSpec {
        path: path.to_path_buf(),
        class: RootClass::Active,
        recursive: true,
    }
}

#[derive(Debug)]
struct CountingKeyProvider {
    calls: Arc<AtomicUsize>,
}

impl KeyProvider for CountingKeyProvider {
    fn master_key(&self) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Zeroizing::new([77_u8; 32]))
    }
}

#[test]
fn history_is_exactly_ninety_days_and_rejects_untrusted_dates() {
    let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    let policy = HistoryPolicy::new(Duration::from_hours(90 * 24));

    assert_eq!(
        policy.decide(Some(now - time::Duration::days(90)), now, 400),
        HistoryDecision::ReplayFrom(0)
    );
    assert_eq!(
        policy.decide(
            Some(now - time::Duration::days(90) - time::Duration::seconds(1)),
            now,
            400
        ),
        HistoryDecision::BaselineAt(400)
    );
    assert_eq!(
        policy.decide(Some(now + time::Duration::days(1)), now, 400),
        HistoryDecision::ReplayFrom(0)
    );
    assert_eq!(
        policy.decide(
            Some(now + time::Duration::days(1) + time::Duration::seconds(1)),
            now,
            400
        ),
        HistoryDecision::BaselineAt(400)
    );
    assert_eq!(
        policy.decide(None, now, 400),
        HistoryDecision::BaselineAt(400)
    );
    let attempted_override = HistoryPolicy::new(Duration::from_hours(30 * 24));
    assert_eq!(
        attempted_override.decide(Some(now - time::Duration::days(60)), now, 400),
        HistoryDecision::ReplayFrom(0)
    );
    let minimum = OffsetDateTime::from_unix_timestamp(-377_705_116_800).unwrap();
    let maximum = OffsetDateTime::from_unix_timestamp(253_402_300_799).unwrap();
    assert_eq!(
        policy.decide(Some(minimum), minimum, 400),
        HistoryDecision::BaselineAt(400)
    );
    assert_eq!(
        policy.decide(Some(maximum), maximum, 400),
        HistoryDecision::BaselineAt(400)
    );
}

#[test]
fn discovery_yields_at_256_and_resumes_deterministically_without_opening_contents() {
    let temp = tempfile::tempdir().unwrap();
    for index in (0..300).rev() {
        let path = temp.path().join(format!("{index:03}.jsonl"));
        fs::write(path, b"AGBOX_DISCOVERY_MUST_NOT_READ_CONTENT").unwrap();
    }

    let mut walker = DiscoveryWalker::new(Provider::Codex, root_spec(temp.path())).unwrap();
    let first = walker.next_batch(usize::MAX).unwrap();
    assert_eq!(first.visited_entries, DISCOVERY_ENTRIES_PER_YIELD);
    assert_eq!(first.sources.len(), DISCOVERY_ENTRIES_PER_YIELD - 2);
    assert!(first.cursor.is_some());
    let serialized_cursor = serde_json::to_vec(first.cursor.as_ref().unwrap()).unwrap();
    assert!(serialized_cursor.len() < 32 * 1024);
    assert!(
        !serialized_cursor
            .windows(temp.path().as_os_str().as_encoded_bytes().len())
            .any(|window| window == temp.path().as_os_str().as_encoded_bytes())
    );

    let second = walker.next_batch(usize::MAX).unwrap();
    assert_eq!(second.visited_entries, 46);
    assert_eq!(second.sources.len(), 46);
    assert!(second.cursor.is_none());

    let first_names = first
        .sources
        .iter()
        .map(|source| source.path.file_name().unwrap().to_owned())
        .collect::<Vec<_>>();
    let second_names = second
        .sources
        .iter()
        .map(|source| source.path.file_name().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(first_names.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(second_names.windows(2).all(|pair| pair[0] <= pair[1]));
    let mut names = first_names;
    names.extend(second_names);
    names.sort();
    let expected = (0..300)
        .map(|index| std::ffi::OsString::from(format!("{index:03}.jsonl")))
        .collect::<Vec<_>>();
    assert_eq!(names, expected);

    let mut resumed = DiscoveryWalker::from_cursor(
        Provider::Codex,
        root_spec(temp.path()),
        first.cursor.unwrap(),
    )
    .unwrap();
    let recovery = resumed.next_batch(256).unwrap();
    assert!(recovery.sources.is_empty());
    assert_eq!(recovery.visited_entries, 256);
    assert!(recovery.cursor.is_some());
    let resumed_tail = resumed.next_batch(256).unwrap();
    assert_eq!(resumed_tail.sources.len(), 46);
    assert!(resumed_tail.visited_entries <= 256);

    let mut restarted_recovery = DiscoveryWalker::from_cursor(
        Provider::Codex,
        root_spec(temp.path()),
        recovery.cursor.unwrap(),
    )
    .unwrap();
    let repeated_recovery = restarted_recovery.next_batch(128).unwrap();
    assert!(repeated_recovery.sources.is_empty());
    assert_eq!(repeated_recovery.visited_entries, 128);
    let continued_recovery = restarted_recovery.next_batch(128).unwrap();
    assert!(continued_recovery.sources.is_empty());
    assert_eq!(continued_recovery.visited_entries, 128);
    let restarted_tail = restarted_recovery.next_batch(256).unwrap();
    assert_eq!(restarted_tail.sources.len(), 46);
}

#[test]
fn worst_case_256_long_entries_finish_with_a_bounded_cursor() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..256 {
        let name = format!("{index:03}-{}.jsonl", "x".repeat(180));
        fs::write(temp.path().join(name), b"record").unwrap();
    }
    let mut walker = DiscoveryWalker::new(Provider::Codex, root_spec(temp.path())).unwrap();
    let batch = walker.next_batch(256).unwrap();
    assert_eq!(batch.visited_entries, 256);
    assert_eq!(batch.sources.len(), 254);
    assert!(batch.cursor.is_some());
    assert!(batch.faults.is_empty());
    let terminal = walker.next_batch(256).unwrap();
    assert_eq!(terminal.visited_entries, 2);
    assert_eq!(terminal.sources.len(), 2);
    assert!(terminal.cursor.is_none());
}

#[test]
fn wide_long_child_directories_make_progress_without_a_cursor_overflow() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..256 {
        let child = temp.path().join(format!("{index:03}-{}", "d".repeat(180)));
        fs::create_dir(&child).unwrap();
        fs::write(child.join("source.jsonl"), b"record").unwrap();
    }
    let mut walker = DiscoveryWalker::new(Provider::Codex, root_spec(temp.path())).unwrap();
    let mut found = 0;
    for _ in 0..1024 {
        let batch = walker.next_batch(256).unwrap();
        found += batch.sources.len();
        if batch.cursor.is_none() {
            break;
        }
    }
    assert_eq!(found, 256);
}

#[test]
fn cursor_debug_redacts_reversible_components() {
    let temp = tempfile::tempdir().unwrap();
    let secret = "SECRET_CURSOR_COMPONENT";
    fs::create_dir(temp.path().join(secret)).unwrap();
    let mut walker = DiscoveryWalker::new(Provider::Codex, root_spec(temp.path())).unwrap();
    let cursor = walker.next_batch(3).unwrap().cursor.unwrap();
    assert!(!format!("{cursor:?}").contains(secret));
}

#[test]
fn persistent_deleted_directory_is_quarantined_without_blocking_later_work() {
    let temp = tempfile::tempdir().unwrap();
    let bad = temp.path().join("bad");
    let good = temp.path().join("good");
    fs::create_dir(&bad).unwrap();
    fs::create_dir(&good).unwrap();
    fs::write(good.join("source.jsonl"), b"record").unwrap();
    let mut walker = DiscoveryWalker::new(Provider::Codex, root_spec(temp.path())).unwrap();
    let first = walker.next_batch(256).unwrap();
    fs::remove_dir_all(&bad).unwrap();

    let mut sources = first.sources;
    for _ in 0..16 {
        let batch = walker.next_batch(256).unwrap();
        sources.extend(batch.sources);
        if batch.cursor.is_none() {
            break;
        }
    }
    assert!(
        sources
            .iter()
            .any(|source| source.path.ends_with("good/source.jsonl"))
    );
}

#[test]
fn overlapping_roots_collapse_by_verified_file_identity() {
    let temp = tempfile::tempdir().unwrap();
    let nested = temp.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("session.jsonl"), b"record").unwrap();
    let mut broad = DiscoveryWalker::new(Provider::Codex, root_spec(temp.path())).unwrap();
    let mut narrow = DiscoveryWalker::new(Provider::Codex, root_spec(&nested)).unwrap();
    let broad_sources = broad.next_batch(256).unwrap().sources;
    let narrow_sources = narrow.next_batch(256).unwrap().sources;
    let collapsed = deduplicate_overlapping_sources([broad_sources, narrow_sources].concat());
    assert_eq!(collapsed.len(), 1);
    assert_eq!(collapsed[0].root, nested.canonicalize().unwrap());
}

#[test]
fn discovery_skips_symlinks_non_regular_files_and_backup_trees() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("keep.jsonl"), b"keep").unwrap();
    fs::create_dir(temp.path().join("backup")).unwrap();
    fs::write(temp.path().join("backup/hidden.jsonl"), b"hidden").unwrap();
    fs::create_dir(temp.path().join("tmp")).unwrap();
    fs::write(temp.path().join("tmp/hidden.jsonl"), b"hidden").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        temp.path().join("keep.jsonl"),
        temp.path().join("linked.jsonl"),
    )
    .unwrap();

    let mut walker = DiscoveryWalker::new(Provider::Claude, root_spec(temp.path())).unwrap();
    let batch = walker.next_batch(256).unwrap();
    assert_eq!(batch.sources.len(), 1);
    assert_eq!(batch.sources[0].path.file_name().unwrap(), "keep.jsonl");
}

#[test]
fn discovery_uses_only_the_adapter_trusted_session_date() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("2026/01/02")).unwrap();
    fs::write(temp.path().join("2026/01/02/session.jsonl"), b"record").unwrap();
    fs::write(temp.path().join("not-a-date.jsonl"), b"record").unwrap();
    let mut walker = DiscoveryWalker::new(Provider::Codex, root_spec(temp.path())).unwrap();
    let mut sources = Vec::new();
    loop {
        let batch = walker.next_batch(256).unwrap();
        sources.extend(batch.sources);
        if batch.cursor.is_none() {
            break;
        }
    }
    let dated = sources
        .iter()
        .find(|source| source.path.ends_with("2026/01/02/session.jsonl"))
        .unwrap();
    assert_eq!(
        dated.session_time,
        Some(OffsetDateTime::from_unix_timestamp(1_767_312_000).unwrap())
    );
    let invalid = sources
        .iter()
        .find(|source| source.path.ends_with("not-a-date.jsonl"))
        .unwrap();
    assert_eq!(invalid.session_time, None);
}

#[test]
fn discovery_isolates_metadata_disappearance_without_exposing_names() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("a.jsonl"), b"a").unwrap();
    fs::write(temp.path().join("z_SECRET_ATTACKER_NAME.jsonl"), b"b").unwrap();
    let mut walker = DiscoveryWalker::new(Provider::Codex, root_spec(temp.path())).unwrap();
    let first = walker.next_batch(3).unwrap();
    assert_eq!(first.sources.len(), 1);
    let remaining = if first.sources[0].path.ends_with("a.jsonl") {
        "z_SECRET_ATTACKER_NAME.jsonl"
    } else {
        "a.jsonl"
    };
    fs::remove_file(temp.path().join(remaining)).unwrap();
    let second = walker.next_batch(1).unwrap();
    assert_eq!(second.sources.len(), 0);
    assert_eq!(second.faults.len(), 1);
    let debug = format!("{:?}", second.faults);
    assert!(!debug.contains("SECRET_ATTACKER_NAME"));
    assert!(debug.len() < 128);
}

#[cfg(unix)]
#[test]
fn verified_open_rejects_symlink_components_and_replacement_races() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("source.jsonl"), b"original").unwrap();
    std::os::unix::fs::symlink(&real, temp.path().join("linked")).unwrap();

    let opener = VerifiedSourceOpener::new(temp.path()).unwrap();
    let error = opener
        .open_relative(Path::new("linked/source.jsonl"), "unix:0:0")
        .unwrap_err();
    assert_eq!(error, VerifiedOpenError::IdentityChanged);

    let mut walker = DiscoveryWalker::new(Provider::Codex, root_spec(temp.path())).unwrap();
    let batch = walker.next_batch(256).unwrap();
    let source = batch
        .sources
        .iter()
        .find(|source| source.path.ends_with("real/source.jsonl"))
        .unwrap();
    fs::write(real.join("source.jsonl"), b"original plus append").unwrap();
    assert_eq!(
        opener.open(source).unwrap_err(),
        VerifiedOpenError::IdentityChanged
    );
    fs::rename(real.join("source.jsonl"), real.join("old.jsonl")).unwrap();
    fs::write(real.join("source.jsonl"), b"replacement").unwrap();
    assert_eq!(
        opener.open(source).unwrap_err(),
        VerifiedOpenError::IdentityChanged
    );
}

#[cfg(unix)]
#[test]
fn verified_open_rejects_an_intermediate_directory_renamed_out_of_its_bound_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let nested = root.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("source.jsonl"), b"record").unwrap();
    let opener = VerifiedSourceOpener::new(root.path()).unwrap();
    let mut walker = DiscoveryWalker::new(Provider::Codex, root_spec(root.path())).unwrap();
    let source = walker
        .next_batch(256)
        .unwrap()
        .sources
        .into_iter()
        .next()
        .unwrap();
    fs::rename(&nested, outside.path().join("nested")).unwrap();
    assert!(matches!(
        opener.open(&source),
        Err(VerifiedOpenError::IdentityChanged)
    ));
}

#[test]
fn project_resolution_requires_real_git_and_domain_separates_identity() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    let nested = repository.join("a/b");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir(repository.join(".git")).unwrap();

    let resolver = ProjectResolver::new(temp.path()).unwrap();
    let project = resolver.resolve(&nested).unwrap();
    assert_eq!(project.root, repository.canonicalize().unwrap());
    assert!(project.project_id.as_str().starts_with("project_"));
    assert!(project.repository_identity.starts_with("repo-fs-v1:"));
    assert!(!project.project_id.as_str().contains("repo"));
    assert!(resolver.resolve(temp.path()).is_err());
}

#[test]
fn project_resolution_requires_a_valid_contained_gitdir_marker() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    let nested = repository.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let resolver = ProjectResolver::new(temp.path()).unwrap();

    fs::write(repository.join(".git"), b"").unwrap();
    assert!(resolver.resolve(&nested).is_err());
    fs::write(repository.join(".git"), b"gitdir: ../outside\nextra").unwrap();
    assert!(resolver.resolve(&nested).is_err());
    fs::write(repository.join(".git"), b"gitdir: ../../escape").unwrap();
    assert!(resolver.resolve(&nested).is_err());

    fs::create_dir(repository.join("actual-gitdir")).unwrap();
    fs::write(repository.join(".git"), b"gitdir: actual-gitdir\n").unwrap();
    assert_eq!(
        resolver.resolve(&nested).unwrap().root,
        repository.canonicalize().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn project_resolution_rejects_symlink_escape() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(outside.path().join(".git")).unwrap();
    std::os::unix::fs::symlink(outside.path(), allowed.path().join("escape")).unwrap();
    let resolver = ProjectResolver::new(allowed.path()).unwrap();
    assert!(resolver.resolve(allowed.path().join("escape")).is_err());
}

#[cfg(unix)]
#[test]
fn project_resolution_rejects_a_component_swapped_to_a_symlink_after_binding() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let repository = allowed.path().join("repo");
    fs::create_dir_all(repository.join("nested")).unwrap();
    fs::create_dir(repository.join(".git")).unwrap();
    fs::create_dir(outside.path().join(".git")).unwrap();
    let resolver = ProjectResolver::new(allowed.path()).unwrap();

    fs::rename(&repository, allowed.path().join("old-repo")).unwrap();
    std::os::unix::fs::symlink(outside.path(), &repository).unwrap();
    assert!(resolver.resolve(repository.join("nested")).is_err());
}

#[test]
fn project_resolution_rejects_repository_renamed_outside_the_bound_root() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let repository = allowed.path().join("repo");
    fs::create_dir_all(repository.join("nested")).unwrap();
    fs::create_dir(repository.join(".git")).unwrap();
    let resolver = ProjectResolver::new(allowed.path()).unwrap();
    fs::rename(&repository, outside.path().join("repo")).unwrap();
    assert!(resolver.resolve(repository.join("nested")).is_err());
}

#[tokio::test]
async fn store_runtime_loads_the_field_encryption_key_exactly_once() {
    let temp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = StoreRuntime::start_with_key_provider(
        temp.path().join("state-v2.sqlite3"),
        Arc::new(CountingKeyProvider {
            calls: Arc::clone(&calls),
        }),
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    runtime.shutdown().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn registration_is_atomic_idempotent_encrypted_and_exact() {
    let temp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = temp.path().join("state-v2.sqlite3");
    let runtime = StoreRuntime::start_with_key_provider(
        &database,
        Arc::new(MemoryKeyProvider::fixed([42_u8; 32])),
    )
    .await
    .unwrap();
    let project_root = b"/private/AGBOX_PROJECT_ROOT_SECRET".to_vec();
    let source_path = b"/private/AGBOX_SOURCE_PATH_SECRET/session.jsonl".to_vec();
    let registration = SourceRegistration {
        project_id: agbox_core::ProjectId::for_test("project_registration"),
        repository_identity: "repo-fs-v1:11:12".to_owned(),
        project_root: Zeroizing::new(project_root.clone()),
        source_id: "source_11111111111111111111111111111111".to_owned(),
        provider: Provider::Codex,
        root_class: "active".to_owned(),
        source_path: Zeroizing::new(source_path.clone()),
        file_identity: "unix:11:99".to_owned(),
        generation: 1,
        size_bytes: 777,
        mtime: OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap(),
        session_time: None,
        initial_cursor: 777,
    };
    let debug = format!("{registration:?}");
    assert!(!debug.contains("AGBOX_PROJECT_ROOT_SECRET"));
    assert!(!debug.contains("AGBOX_SOURCE_PATH_SECRET"));
    assert!(!debug.contains("source_11111111111111111111111111111111"));

    runtime
        .writer()
        .register_source(registration.clone())
        .await
        .unwrap();
    runtime
        .writer()
        .register_source(registration)
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();

    let connection = Connection::open(&database).unwrap();
    let generation: (i64, i64) = connection
        .query_row(
            "SELECT size_bytes, cursor_offset
             FROM source_generations
             INNER JOIN source_cursors USING (source_id, generation)
             WHERE source_id = 'source_11111111111111111111111111111111' AND generation = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(generation, (777, 777));
    let stored_text: String = connection
        .query_row(
            "SELECT projects.repository_identity || sources.source_id ||
                    sources.provider || sources.root_class || sources.file_identity ||
                    source_generations.mtime || source_generations.status
             FROM projects
             INNER JOIN sources USING (project_id)
             INNER JOIN source_generations USING (source_id)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!stored_text.contains("AGBOX_PROJECT_ROOT_SECRET"));
    assert!(!stored_text.contains("AGBOX_SOURCE_PATH_SECRET"));

    let bytes = fs::read(&database).unwrap();
    assert!(
        !bytes
            .windows(project_root.len())
            .any(|window| window == project_root)
    );
    assert!(
        !bytes
            .windows(source_path.len())
            .any(|window| window == source_path)
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn registration_conflict_rolls_back_without_partial_generation() {
    let temp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = temp.path().join("state-v2.sqlite3");
    let runtime = StoreRuntime::start_with_key_provider(
        &database,
        Arc::new(MemoryKeyProvider::fixed([43_u8; 32])),
    )
    .await
    .unwrap();
    let base = SourceRegistration {
        project_id: agbox_core::ProjectId::for_test("project_registration"),
        repository_identity: "repo-fs-v1:21:22".to_owned(),
        project_root: Zeroizing::new(b"/project".to_vec()),
        source_id: "source_11111111111111111111111111111111".to_owned(),
        provider: Provider::Claude,
        root_class: "active".to_owned(),
        source_path: Zeroizing::new(b"/project/source.jsonl".to_vec()),
        file_identity: "unix:21:29".to_owned(),
        generation: 1,
        size_bytes: 50,
        mtime: OffsetDateTime::UNIX_EPOCH,
        session_time: None,
        initial_cursor: 0,
    };
    runtime
        .writer()
        .register_source(base.clone())
        .await
        .unwrap();
    let mut conflict = base.clone();
    conflict.file_identity = "unix:21:30".to_owned();
    assert!(runtime.writer().register_source(conflict).await.is_err());

    let mut second = base.clone();
    second.generation = 2;
    second.size_bytes = 55;
    second.file_identity = "unix:21:30".to_owned();
    second.source_path = Zeroizing::new(b"/project/moved-source.jsonl".to_vec());
    runtime
        .writer()
        .register_source(second.clone())
        .await
        .unwrap();

    let mut historical_identity_reuse = base.clone();
    historical_identity_reuse.source_id = "source_33333333333333333333333333333333".to_owned();
    historical_identity_reuse.source_path = Zeroizing::new(b"/project/other-source.jsonl".to_vec());
    assert!(
        runtime
            .writer()
            .register_source(historical_identity_reuse)
            .await
            .is_err()
    );

    let mut invalid_source_id = base.clone();
    invalid_source_id.source_id = "source_ABCDEF".to_owned();
    assert!(
        runtime
            .writer()
            .register_source(invalid_source_id)
            .await
            .is_err()
    );
    let mut invalid_repository_identity = base.clone();
    invalid_repository_identity.repository_identity = "repo-fs-v1:01:22".to_owned();
    assert!(
        runtime
            .writer()
            .register_source(invalid_repository_identity)
            .await
            .is_err()
    );
    let mut invalid_file_identity = base.clone();
    invalid_file_identity.file_identity = "unix:21:029".to_owned();
    assert!(
        runtime
            .writer()
            .register_source(invalid_file_identity)
            .await
            .is_err()
    );

    assert!(
        runtime
            .writer()
            .register_source(base.clone())
            .await
            .is_err()
    );
    let mut cursor_mismatch = second.clone();
    cursor_mismatch.initial_cursor = 55;
    assert!(
        runtime
            .writer()
            .register_source(cursor_mismatch)
            .await
            .is_err()
    );
    let mut generation_gap = second.clone();
    generation_gap.generation = 4;
    assert!(
        runtime
            .writer()
            .register_source(generation_gap)
            .await
            .is_err()
    );
    let mut reassociation = second;
    reassociation.generation = 3;
    reassociation.project_id = agbox_core::ProjectId::for_test("project_other");
    assert!(
        runtime
            .writer()
            .register_source(reassociation)
            .await
            .is_err()
    );
    let maximum_width = SourceRegistration {
        project_id: agbox_core::ProjectId::parse_wire(&"p".repeat(128)).unwrap(),
        repository_identity: "repo-fs-v1:23:24".to_owned(),
        project_root: Zeroizing::new(b"/maximum/project".to_vec()),
        source_id: "source_22222222222222222222222222222222".to_owned(),
        provider: Provider::Codex,
        root_class: "archive".to_owned(),
        source_path: Zeroizing::new(b"/maximum/project/source.jsonl".to_vec()),
        file_identity: "unix:23:25".to_owned(),
        generation: 1,
        size_bytes: 0,
        mtime: OffsetDateTime::UNIX_EPOCH,
        session_time: None,
        initial_cursor: 0,
    };
    runtime
        .writer()
        .register_source(maximum_width)
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();

    let connection = Connection::open(&database).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM source_generations
             WHERE source_id = 'source_11111111111111111111111111111111'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let current_identity: String = connection
        .query_row(
            "SELECT file_identity FROM sources
             WHERE source_id = 'source_11111111111111111111111111111111'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(current_identity, "unix:21:30");
}
