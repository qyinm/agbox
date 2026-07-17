#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Barrier};
use std::thread;

use agbox_core::{EvidenceId, ProjectId, WorkId};
use agbox_store::{EvidenceContext, EvidenceOwnerRef, EvidenceVault, MemoryKeyProvider};

fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

#[test]
fn evidence_is_encrypted_and_bound_to_its_project() {
    let dir = private_tempdir();
    let vault = EvidenceVault::open(
        dir.path().to_path_buf(),
        Arc::new(MemoryKeyProvider::fixed([7_u8; 32])),
    )
    .unwrap();
    let id = EvidenceId::for_test("ev_1");
    let project = ProjectId::for_test("project_a");
    let work = WorkId::for_test("work_a");
    let secret = b"AGBOX_FIXTURE_SECRET_9271";

    vault
        .put(
            &id,
            EvidenceContext {
                project_id: &project,
                owner: EvidenceOwnerRef::Work(&work),
            },
            secret,
        )
        .unwrap();

    let stored = std::fs::read(dir.path().join("ev_1.agbx")).unwrap();
    assert!(stored.starts_with(b"AGBX\x01"));
    assert!(!stored.windows(secret.len()).any(|window| window == secret));
    assert_eq!(
        vault
            .get(
                &id,
                EvidenceContext {
                    project_id: &project,
                    owner: EvidenceOwnerRef::Work(&work),
                },
            )
            .unwrap()
            .as_slice(),
        secret
    );

    let other_project = ProjectId::for_test("project_b");
    assert!(
        vault
            .get(
                &id,
                EvidenceContext {
                    project_id: &other_project,
                    owner: EvidenceOwnerRef::Work(&work),
                },
            )
            .is_err()
    );
}

fn race_puts(
    vault: &Arc<EvidenceVault>,
    id: EvidenceId,
    context: EvidenceContext<'static>,
    first: &'static [u8],
    second: &'static [u8],
) -> (
    Result<(), agbox_store::EvidenceError>,
    Result<(), agbox_store::EvidenceError>,
) {
    let barrier = Arc::new(Barrier::new(2));
    thread::scope(|scope| {
        let first_id = id.clone();
        let first_vault = Arc::clone(vault);
        let first_barrier = Arc::clone(&barrier);
        let first_thread = scope.spawn(move || {
            first_barrier.wait();
            first_vault.put(&first_id, context, first)
        });

        let second_id = id;
        let second_vault = Arc::clone(vault);
        let second_barrier = Arc::clone(&barrier);
        let second_thread = scope.spawn(move || {
            second_barrier.wait();
            second_vault.put(&second_id, context, second)
        });

        (first_thread.join().unwrap(), second_thread.join().unwrap())
    })
}

#[test]
fn concurrent_writers_publish_once_and_leave_no_temporary_files() {
    let same_dir = private_tempdir();
    let same_vault = Arc::new(
        EvidenceVault::open(
            same_dir.path().to_path_buf(),
            Arc::new(MemoryKeyProvider::fixed([8_u8; 32])),
        )
        .unwrap(),
    );
    let same_id = EvidenceId::for_test("ev_same");
    let same_project = ProjectId::for_test("project_same");
    let same_work = WorkId::for_test("work_same");
    let same_context = EvidenceContext {
        project_id: Box::leak(Box::new(same_project)),
        owner: EvidenceOwnerRef::Work(Box::leak(Box::new(same_work))),
    };
    let same_results = race_puts(
        &same_vault,
        same_id.clone(),
        same_context,
        b"same plaintext",
        b"same plaintext",
    );
    assert!(same_results.0.is_ok());
    assert!(same_results.1.is_ok());
    assert_eq!(
        same_vault.get(&same_id, same_context).unwrap().as_slice(),
        b"same plaintext"
    );
    assert!(same_dir.path().read_dir().unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));

    let different_dir = private_tempdir();
    let different_vault = Arc::new(
        EvidenceVault::open(
            different_dir.path().to_path_buf(),
            Arc::new(MemoryKeyProvider::fixed([9_u8; 32])),
        )
        .unwrap(),
    );
    let different_id = EvidenceId::for_test("ev_different");
    let different_project = ProjectId::for_test("project_different");
    let different_work = WorkId::for_test("work_different");
    let different_context = EvidenceContext {
        project_id: Box::leak(Box::new(different_project)),
        owner: EvidenceOwnerRef::Work(Box::leak(Box::new(different_work))),
    };
    let different_results = race_puts(
        &different_vault,
        different_id.clone(),
        different_context,
        b"first plaintext",
        b"second plaintext",
    );
    assert!(different_results.0.is_ok() ^ different_results.1.is_ok());
    assert!(
        matches!(
            different_results.0,
            Err(agbox_store::EvidenceError::Conflict)
        ) || matches!(
            different_results.1,
            Err(agbox_store::EvidenceError::Conflict)
        )
    );
    let final_plaintext = different_vault
        .get(&different_id, different_context)
        .unwrap();
    assert!(
        final_plaintext.as_slice() == b"first plaintext"
            || final_plaintext.as_slice() == b"second plaintext"
    );
    assert!(different_dir.path().read_dir().unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn evidence_ids_longer_than_task_one_wire_limit_are_rejected() {
    let dir = private_tempdir();
    let vault = EvidenceVault::open(
        dir.path().to_path_buf(),
        Arc::new(MemoryKeyProvider::fixed([10_u8; 32])),
    )
    .unwrap();
    let id = EvidenceId::for_test(&"e".repeat(129));
    let project = ProjectId::for_test("project_limit");
    let work = WorkId::for_test("work_limit");

    assert!(
        vault
            .put(
                &id,
                EvidenceContext {
                    project_id: &project,
                    owner: EvidenceOwnerRef::Work(&work),
                },
                b"bounded",
            )
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn owner_directory_rejects_user_owned_intermediate_symlinks() {
    use std::os::unix::fs::symlink;

    let target = private_tempdir();
    let parent = private_tempdir();
    let linked = parent.path().join("linked");
    symlink(target.path(), &linked).unwrap();
    let root = linked.join("evidence");

    assert!(EvidenceVault::open(root, Arc::new(MemoryKeyProvider::fixed([11_u8; 32])),).is_err());
}

#[cfg(unix)]
#[test]
fn replacing_the_named_root_does_not_redirect_a_vault() {
    use std::os::unix::fs::PermissionsExt;

    let container = private_tempdir();
    let root = container.path().join("root");
    let old_root = container.path().join("old-root");
    let replacement = container.path().join("replacement");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&replacement).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o700)).unwrap();
    let vault = EvidenceVault::open(
        root.clone(),
        Arc::new(MemoryKeyProvider::fixed([12_u8; 32])),
    )
    .unwrap();
    std::fs::rename(&root, &old_root).unwrap();
    std::fs::rename(&replacement, &root).unwrap();

    let id = EvidenceId::for_test("ev_root_swap");
    let project = ProjectId::for_test("project_root_swap");
    let work = WorkId::for_test("work_root_swap");
    let context = EvidenceContext {
        project_id: &project,
        owner: EvidenceOwnerRef::Work(&work),
    };
    let put_result = vault.put(&id, context, b"old-root-only");
    assert!(put_result.is_ok(), "put failed: {put_result:?}");
    assert!(!root.join("ev_root_swap.agbx").exists());
    assert!(old_root.join("ev_root_swap.agbx").exists());
    assert_eq!(
        vault.get(&id, context).unwrap().as_slice(),
        b"old-root-only"
    );
}
