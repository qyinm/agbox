#![allow(clippy::unwrap_used)]

use std::{
    os::unix::ffi::OsStrExt,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use agbox_adapters::{DiscoveredSource, RootClass};
use agbox_core::{
    ProjectId, Provider,
    api::{AppRequest, AppResponse},
};
use agbox_ingest::{CoordinatorSource, IngestionCoordinator, ProjectResolver, WorkPriority};
use agbox_service::{
    AppClient, ApplicationService, IpcAppClient,
    ipc::{IPC_PROTOCOL_VERSION, IpcHello, LocalIpcServer, ScopedRequestHandler, WireActor},
};
use agbox_store::{CryptoError, EvidenceVault, KeyProvider, SourceRegistration, StoreRuntime};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Debug)]
struct FixedKey;

impl KeyProvider for FixedKey {
    fn master_key(&self) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
        Ok(Zeroizing::new([0x55; 32]))
    }
}

struct E2eRuntime {
    directory: tempfile::TempDir,
    ipc_directory: tempfile::TempDir,
    project_root: PathBuf,
    project_id: ProjectId,
    repository_identity: String,
    store: StoreRuntime,
    coordinator: Arc<IngestionCoordinator>,
}

impl E2eRuntime {
    async fn start(first: Provider, first_records: &str) -> Self {
        // macOS limits Unix-domain socket paths to 104 bytes. The default
        // temporary directory includes a per-user sandbox prefix that can be
        // longer than that, so keep this integration fixture deliberately
        // short just like the service IPC tests do.
        let directory = tempfile::Builder::new()
            .prefix("a")
            .tempdir_in("/tmp")
            .unwrap();
        let ipc_directory = tempfile::Builder::new()
            .prefix("a")
            .tempdir_in("/tmp")
            .unwrap();
        // Match the local runtime directory shape used by the IPC integration
        // tests: a project-owned, owner-only directory rather than a bare
        // temporary root.
        std::fs::create_dir(ipc_directory.path().join(".git")).unwrap();
        std::fs::set_permissions(ipc_directory.path(), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(project_root.join(".git")).unwrap();
        std::fs::set_permissions(&project_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let project_root = project_root.canonicalize().unwrap();
        let resolved = ProjectResolver::new(&project_root)
            .unwrap()
            .resolve(&project_root)
            .unwrap();
        let store = StoreRuntime::start_with_key_provider(
            directory.path().join("state.db"),
            Arc::new(FixedKey),
        )
        .await
        .unwrap();
        let coordinator = Arc::new(IngestionCoordinator::new(
            store.read().clone(),
            store.writer().clone(),
            agbox_ingest::SOURCE_QUEUE_CAPACITY,
        ));
        let runtime = Self {
            directory,
            ipc_directory,
            project_root,
            project_id: resolved.project_id,
            repository_identity: resolved.repository_identity,
            store,
            coordinator,
        };
        runtime.add_source(first, first_records, 1).await;
        runtime
    }

    async fn add_source(&self, provider: Provider, records: &str, ordinal: u8) {
        let source = self
            .project_root
            .join(format!("source-{}-{ordinal}.jsonl", provider.as_str()));
        std::fs::write(&source, records).unwrap();
        let source = source.canonicalize().unwrap();
        let metadata = std::fs::metadata(&source).unwrap();
        let source_id = format!("source_{ordinal:032x}");
        let file_identity = format!("unix:{}:{}", metadata.dev(), metadata.ino());
        let mtime = file_time(metadata.mtime(), metadata.mtime_nsec());
        let ctime = file_time(metadata.ctime(), metadata.ctime_nsec());
        let now = OffsetDateTime::now_utc();
        let discovered = DiscoveredSource {
            source_id: source_id.clone(),
            provider,
            root: self.project_root.clone(),
            path: source.clone(),
            class: RootClass::Active,
            file_identity: file_identity.clone(),
            generation: 1,
            size: metadata.len(),
            mtime,
            ctime,
            session_time: None,
        };
        self.store
            .writer()
            .register_source(SourceRegistration {
                project_id: self.project_id.clone(),
                repository_identity: self.repository_identity.clone(),
                project_root: Zeroizing::new(self.project_root.as_os_str().as_bytes().to_vec()),
                source_id,
                provider,
                root_class: "active".into(),
                source_path: Zeroizing::new(source.as_os_str().as_bytes().to_vec()),
                file_identity,
                generation: 1,
                size_bytes: metadata.len(),
                mtime,
                session_time: None,
                initial_cursor: 0,
            })
            .await
            .unwrap();
        let key = self
            .coordinator
            .register_source(CoordinatorSource {
                discovered,
                project_id: self.project_id.clone(),
                project_root: Some(self.project_root.clone()),
                format: source_format(provider).into(),
                observed_at: now,
            })
            .unwrap();
        self.coordinator
            .try_enqueue(key, metadata.len(), WorkPriority::Live)
            .unwrap();
    }

    async fn publish(&self) {
        while let Some(lease) = self.coordinator.lease_one().unwrap() {
            self.coordinator.process_one(lease).await.unwrap();
        }
        while !self
            .coordinator
            .reduce_and_publish_grouped_next()
            .await
            .unwrap()
            .is_empty()
        {}
    }

    async fn start_ipc(
        &self,
    ) -> (
        Arc<LocalIpcServer>,
        CancellationToken,
        tokio::task::JoinHandle<Result<(), agbox_service::ipc::IpcError>>,
    ) {
        let vault = EvidenceVault::open(self.directory.path().join("evidence"), Arc::new(FixedKey))
            .unwrap();
        let application: Arc<dyn ScopedRequestHandler> = Arc::new(ApplicationService::new(
            self.store.read().clone(),
            self.store.writer().clone(),
            vault,
        ));
        let socket = self.ipc_directory.path().join("agbox.sock");
        let server = Arc::new(LocalIpcServer::bind(&socket, application).await.unwrap());
        let cancel = CancellationToken::new();
        let serving = {
            let server = Arc::clone(&server);
            let cancel = cancel.clone();
            tokio::spawn(async move { server.serve_until(cancel).await })
        };
        (server, cancel, serving)
    }

    async fn client(&self, socket: &Path, provider: Provider, root: PathBuf) -> IpcAppClient {
        IpcAppClient::connect(
            socket,
            IpcHello {
                protocol_version: IPC_PROTOCOL_VERSION,
                project_root: root,
                actor: WireActor::Agent { provider },
            },
        )
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn claude_and_codex_share_scoped_handoffs_in_both_directions() {
    let codex = include_str!("../../agbox-adapters/tests/fixtures/codex/subagents.jsonl");
    let claude = include_str!("../../agbox-adapters/tests/fixtures/claude/sidechain.jsonl");
    assert_shared_handoff(Provider::Claude, claude, Provider::Codex, codex).await;
    assert_shared_handoff(Provider::Codex, codex, Provider::Claude, claude).await;
}

async fn assert_shared_handoff(
    first_provider: Provider,
    first_records: &str,
    second_provider: Provider,
    second_records: &str,
) {
    let runtime = E2eRuntime::start(first_provider, first_records).await;
    runtime.add_source(second_provider, second_records, 2).await;
    let (server, cancel, serving) = runtime.start_ipc().await;
    runtime.publish().await;
    let socket = runtime.ipc_directory.path().join("agbox.sock");
    let claude_client = runtime
        .client(&socket, Provider::Claude, runtime.project_root.clone())
        .await;
    let codex_client = runtime
        .client(&socket, Provider::Codex, runtime.project_root.clone())
        .await;
    let claude_work = claude_client.call(AppRequest::CurrentWork).await.unwrap();
    let codex_work = codex_client.call(AppRequest::CurrentWork).await.unwrap();
    let (AppResponse::Work(claude_work), AppResponse::Work(codex_work)) = (claude_work, codex_work)
    else {
        panic!("both agents must receive a handoff");
    };
    assert_eq!(claude_work.work_id, codex_work.work_id);
    assert_eq!(claude_work.revision, codex_work.revision);
    let rendered = serde_json::to_vec(&claude_work).unwrap();
    for forbidden in [
        b"PRIVATE_TOKEN".as_slice(),
        b"PRIVATE_AGENT".as_slice(),
        b"PRIVATE_CREDENTIAL".as_slice(),
        b"/Users/alice".as_slice(),
        b"QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=".as_slice(),
    ] {
        assert!(
            !rendered
                .windows(forbidden.len())
                .any(|window| window == forbidden),
            "raw native transcript data must never reach the IPC work response"
        );
    }

    let foreign = tempfile::tempdir().unwrap();
    std::fs::create_dir(foreign.path().join(".git")).unwrap();
    let foreign_client = runtime
        .client(&socket, Provider::Codex, foreign.path().to_path_buf())
        .await;
    assert!(matches!(
        foreign_client.call(AppRequest::CurrentWork).await.unwrap(),
        AppResponse::NotFound
    ));

    cancel.cancel();
    serving.await.unwrap().unwrap();
    server.remove_socket().unwrap();
    runtime.store.shutdown().await.unwrap();
}

fn source_format(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude-transcript-2.1",
        Provider::Codex => "codex-rollout-1",
    }
}

fn file_time(seconds: i64, nanoseconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(
        i128::from(seconds) * 1_000_000_000 + i128::from(nanoseconds),
    )
    .unwrap()
}
