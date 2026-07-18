use std::{
    collections::HashMap,
    fmt,
    fs::File,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use agbox_core::Provider;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{QueueItem, SourceKey, WorkPriority};

pub const WATCH_SIGNAL_CAPACITY: usize = 256;
pub const MAX_BACKEND_EVENT_PATHS: usize = 16;
pub const POLL_INTERVAL: Duration = Duration::from_mins(5);
const MAX_ROOT_ID_BYTES: usize = 64;
const MAX_RELATIVE_PATH_BYTES: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchEventKind {
    Create,
    Write,
    Rename,
    Remove,
    Reconcile,
}

#[derive(Clone)]
pub struct WatchRoot {
    id: String,
    provider: Provider,
    path: PathBuf,
}

impl WatchRoot {
    /// Creates a watch root bound to its current canonical directory.
    ///
    /// # Errors
    ///
    /// Rejects unbounded identifiers and unavailable or non-directory roots.
    pub fn new(
        id: impl Into<String>,
        provider: Provider,
        path: impl AsRef<Path>,
    ) -> Result<Self, WatcherError> {
        let id = id.into();
        if id.is_empty()
            || id.len() > MAX_ROOT_ID_BYTES
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(WatcherError::InvalidRoot);
        }
        let path = path
            .as_ref()
            .canonicalize()
            .map_err(|_| WatcherError::InvalidRoot)?;
        if !path.is_dir() {
            return Err(WatcherError::InvalidRoot);
        }
        Ok(Self { id, provider, path })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }
}

impl fmt::Debug for WatchRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WatchRoot")
            .field("id", &self.id)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct WatchSignal {
    root_id: String,
    relative_path: PathBuf,
    kind: WatchEventKind,
}

impl fmt::Debug for WatchSignal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WatchSignal")
            .field("root_id", &self.root_id)
            .field(
                "relative_path_bytes",
                &self.relative_path.as_os_str().as_bytes().len(),
            )
            .field("kind", &self.kind)
            .finish()
    }
}

impl WatchSignal {
    #[must_use]
    pub fn root_id(&self) -> &str {
        &self.root_id
    }

    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    #[must_use]
    pub const fn kind(&self) -> WatchEventKind {
        self.kind
    }
}

#[derive(Clone)]
pub struct WatchSignalBridge {
    sender: mpsc::Sender<WatchSignal>,
    roots: Arc<[WatchRoot]>,
    overflow: Arc<AtomicBool>,
    overflow_count: Arc<AtomicU64>,
}

impl fmt::Debug for WatchSignalBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WatchSignalBridge")
            .field("roots", &self.roots.len())
            .field("overflow", &self.overflow.load(Ordering::Acquire))
            .field(
                "overflow_count",
                &self.overflow_count.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl WatchSignalBridge {
    #[must_use]
    pub fn new(roots: Vec<WatchRoot>) -> (Self, mpsc::Receiver<WatchSignal>) {
        let (sender, receiver) = mpsc::channel(WATCH_SIGNAL_CAPACITY);
        (
            Self {
                sender,
                roots: roots.into(),
                overflow: Arc::new(AtomicBool::new(false)),
                overflow_count: Arc::new(AtomicU64::new(0)),
            },
            receiver,
        )
    }

    pub fn push_paths<I, P>(&self, kind: WatchEventKind, paths: I)
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut paths = paths.into_iter();
        let mut consumed = 0_usize;
        for path in paths.by_ref().take(MAX_BACKEND_EVENT_PATHS) {
            consumed = consumed.saturating_add(1);
            let Some(signal) = self.normalize(kind, path.as_ref()) else {
                self.mark_overflow();
                continue;
            };
            if self.sender.try_send(signal).is_err() {
                self.mark_overflow();
                break;
            }
        }
        if paths.next().is_some() {
            self.mark_overflow();
        }
        if consumed == 0 {
            self.mark_overflow();
        }
    }

    #[must_use]
    pub fn take_overflow(&self) -> bool {
        self.overflow.swap(false, Ordering::AcqRel)
    }

    #[must_use]
    pub fn overflow_count(&self) -> u64 {
        self.overflow_count.load(Ordering::Relaxed)
    }

    fn is_overflowed(&self) -> bool {
        self.overflow.load(Ordering::Acquire)
    }

    fn mark_overflow(&self) {
        self.overflow.store(true, Ordering::Release);
        let _ = self
            .overflow_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(1))
            });
    }

    fn normalize(&self, kind: WatchEventKind, path: &Path) -> Option<WatchSignal> {
        if !path.is_absolute() {
            return None;
        }
        let path = canonicalize_existing_ancestor(path)?;
        let root = self
            .roots
            .iter()
            .filter(|root| path.starts_with(&root.path))
            .max_by_key(|root| root.path.components().count())?;
        let relative_path = path.strip_prefix(&root.path).ok()?;
        if relative_path.as_os_str().is_empty()
            || relative_path.as_os_str().as_bytes().len() > MAX_RELATIVE_PATH_BYTES
            || !relative_path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return None;
        }
        Some(WatchSignal {
            root_id: root.id.clone(),
            relative_path: relative_path.to_owned(),
            kind,
        })
    }
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        if let Ok(canonical) = ancestor.canonicalize() {
            let mut result = canonical;
            for component in suffix.iter().rev() {
                result.push(component);
            }
            return Some(result);
        }
        let name = ancestor.file_name()?.to_owned();
        suffix.push(name);
        ancestor = ancestor.parent()?;
        if suffix
            .iter()
            .map(|component| component.as_bytes().len())
            .sum::<usize>()
            > MAX_RELATIVE_PATH_BYTES
        {
            return None;
        }
    }
}

#[derive(Clone)]
pub struct WatchedSource {
    key: SourceKey,
    root_id: String,
    relative_path: PathBuf,
    priority: WorkPriority,
}

impl WatchedSource {
    /// Creates a source locator containing only a verified root ID and bounded relative path.
    ///
    /// # Errors
    ///
    /// Rejects absolute, empty, escaping, or oversized relative paths.
    pub fn new(
        key: SourceKey,
        root_id: impl Into<String>,
        relative_path: PathBuf,
        priority: WorkPriority,
    ) -> Result<Self, WatcherError> {
        let root_id = root_id.into();
        if root_id.is_empty()
            || root_id.len() > MAX_ROOT_ID_BYTES
            || relative_path.as_os_str().is_empty()
            || relative_path.as_os_str().as_bytes().len() > MAX_RELATIVE_PATH_BYTES
            || relative_path.is_absolute()
            || !relative_path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(WatcherError::InvalidSource);
        }
        Ok(Self {
            key,
            root_id,
            relative_path,
            priority,
        })
    }

    #[must_use]
    pub fn key(&self) -> &SourceKey {
        &self.key
    }

    #[must_use]
    pub fn root_id(&self) -> &str {
        &self.root_id
    }

    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}

impl fmt::Debug for WatchedSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WatchedSource")
            .field("key", &self.key)
            .field("root_id", &self.root_id)
            .field("priority", &self.priority)
            .finish_non_exhaustive()
    }
}

pub trait WatcherCatalog: fmt::Debug + Send + Sync + 'static {
    fn visit(
        &self,
        root_id: Option<&str>,
        relative_path: Option<&Path>,
        visitor: &mut dyn FnMut(&WatchedSource),
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WatcherError {
    #[error("watch root is invalid")]
    InvalidRoot,
    #[error("watched source locator is invalid")]
    InvalidSource,
    #[error("watch backend could not start")]
    BackendUnavailable,
    #[error("watcher task stopped")]
    TaskStopped,
}

pub struct WatcherRuntime {
    roots: Arc<[WatchRoot]>,
    catalog: Arc<dyn WatcherCatalog>,
}

impl fmt::Debug for WatcherRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WatcherRuntime")
            .field("roots", &self.roots.len())
            .finish_non_exhaustive()
    }
}

impl WatcherRuntime {
    /// Creates a runtime with unique verified root identifiers.
    ///
    /// # Errors
    ///
    /// Rejects empty or duplicate root sets.
    pub fn new(
        roots: Vec<WatchRoot>,
        catalog: Arc<dyn WatcherCatalog>,
    ) -> Result<Self, WatcherError> {
        if roots.is_empty() {
            return Err(WatcherError::InvalidRoot);
        }
        let mut ids = std::collections::HashSet::with_capacity(roots.len());
        if roots.iter().any(|root| !ids.insert(root.id.clone())) {
            return Err(WatcherError::InvalidRoot);
        }
        Ok(Self {
            roots: roots.into(),
            catalog,
        })
    }

    #[cfg(feature = "test-support")]
    #[allow(clippy::missing_errors_doc)]
    pub async fn start_for_test(
        self,
        shutdown: watch::Receiver<bool>,
    ) -> Result<WatcherHandle, WatcherError> {
        self.start_inner(shutdown, None, None)
            .await
            .map_err(join_error)?
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn start_with_registration_barrier(
        self,
        shutdown: watch::Receiver<bool>,
        paused: oneshot::Sender<()>,
        resume: oneshot::Receiver<()>,
    ) -> tokio::task::JoinHandle<Result<WatcherHandle, WatcherError>> {
        self.start_inner(shutdown, Some(paused), Some(resume))
    }

    /// Starts the watcher and resolves only after reconciliation has closed the startup gap.
    ///
    /// # Errors
    ///
    /// Returns a backend or task error before readiness is published.
    pub async fn start(
        self,
        shutdown: watch::Receiver<bool>,
    ) -> Result<WatcherHandle, WatcherError> {
        self.start_inner(shutdown, None, None)
            .await
            .map_err(join_error)?
    }

    fn start_inner(
        self,
        shutdown: watch::Receiver<bool>,
        paused: Option<oneshot::Sender<()>>,
        resume: Option<oneshot::Receiver<()>>,
    ) -> tokio::task::JoinHandle<Result<WatcherHandle, WatcherError>> {
        tokio::spawn(async move {
            let baseline = snapshot_sizes(&self.roots, self.catalog.as_ref());
            let (bridge, signals) = WatchSignalBridge::new(self.roots.to_vec());
            let callback = bridge.clone();
            let mut watcher =
                notify::recommended_watcher(move |event: notify::Result<Event>| match event {
                    Ok(event) => callback.push_paths(map_event_kind(event.kind), event.paths),
                    Err(_) => callback.mark_overflow(),
                })
                .map_err(|_| WatcherError::BackendUnavailable)?;
            for root in self.roots.iter() {
                watcher
                    .watch(&root.path, RecursiveMode::Recursive)
                    .map_err(|_| WatcherError::BackendUnavailable)?;
            }
            if let Some(paused) = paused {
                let _ = paused.send(());
            }
            if let Some(resume) = resume {
                let _ = resume.await;
            }

            let (output_tx, output_rx) = mpsc::channel(WATCH_SIGNAL_CAPACITY);
            reconcile_startup(
                &self.roots,
                self.catalog.as_ref(),
                &baseline,
                &output_tx,
                &bridge,
            )
            .await;
            let (control_tx, control_rx) = mpsc::channel(1);
            let task = tokio::spawn(run_loop(WatchLoop {
                _watcher: watcher,
                roots: self.roots,
                catalog: self.catalog,
                bridge: bridge.clone(),
                signals,
                output: output_tx,
                control: control_rx,
                shutdown,
            }));
            Ok(WatcherHandle {
                receiver: output_rx,
                bridge,
                control: control_tx,
                task: Some(task),
            })
        })
    }
}

fn join_error(_: tokio::task::JoinError) -> WatcherError {
    WatcherError::TaskStopped
}

pub struct WatcherHandle {
    receiver: mpsc::Receiver<QueueItem>,
    bridge: WatchSignalBridge,
    control: mpsc::Sender<Control>,
    task: Option<tokio::task::JoinHandle<Result<(), WatcherError>>>,
}

impl fmt::Debug for WatcherHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WatcherHandle")
            .field("pending", &self.receiver.len())
            .finish_non_exhaustive()
    }
}

impl WatcherHandle {
    pub async fn recv(&mut self) -> Option<QueueItem> {
        self.receiver.recv().await
    }

    /// Attempts to receive one already-reconciled source target.
    ///
    /// # Errors
    ///
    /// Returns Tokio's bounded-channel empty or disconnected status.
    pub fn try_recv(&mut self) -> Result<QueueItem, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    #[cfg(feature = "test-support")]
    #[allow(clippy::missing_errors_doc)]
    pub fn inject<I, P>(&self, kind: WatchEventKind, paths: I) -> Result<(), WatcherError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        self.bridge.push_paths(kind, paths);
        Ok(())
    }

    #[cfg(feature = "test-support")]
    #[allow(clippy::missing_errors_doc)]
    pub async fn reconcile_pending_for_test(&mut self) -> Result<Vec<QueueItem>, WatcherError> {
        let (sender, receiver) = oneshot::channel();
        self.control
            .send(Control::Flush(sender))
            .await
            .map_err(|_| WatcherError::TaskStopped)?;
        receiver.await.map_err(|_| WatcherError::TaskStopped)?;
        let mut items = Vec::with_capacity(self.receiver.len());
        while let Ok(item) = self.receiver.try_recv() {
            items.push(item);
        }
        Ok(items)
    }

    #[cfg(feature = "test-support")]
    #[allow(clippy::missing_errors_doc)]
    pub async fn recv_reconciled_for_test(&mut self) -> Result<QueueItem, WatcherError> {
        let mut items = self.reconcile_pending_for_test().await?;
        items.drain(..).next().ok_or(WatcherError::TaskStopped)
    }

    /// Joins the watcher after its shutdown receiver has been signalled.
    ///
    /// # Errors
    ///
    /// Returns when the backend loop stopped unexpectedly.
    pub async fn join(mut self) -> Result<(), WatcherError> {
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await.map_err(join_error)?
    }

    pub(crate) fn into_runtime_parts(mut self) -> Result<RuntimeParts, WatcherError> {
        let task = self.task.take().ok_or(WatcherError::TaskStopped)?;
        Ok((self.receiver, task))
    }
}

enum Control {
    Flush(oneshot::Sender<()>),
}

type WatcherTask = tokio::task::JoinHandle<Result<(), WatcherError>>;
type RuntimeParts = (mpsc::Receiver<QueueItem>, WatcherTask);

struct WatchLoop {
    _watcher: RecommendedWatcher,
    roots: Arc<[WatchRoot]>,
    catalog: Arc<dyn WatcherCatalog>,
    bridge: WatchSignalBridge,
    signals: mpsc::Receiver<WatchSignal>,
    output: mpsc::Sender<QueueItem>,
    control: mpsc::Receiver<Control>,
    shutdown: watch::Receiver<bool>,
}

async fn run_loop(mut state: WatchLoop) -> Result<(), WatcherError> {
    let start = tokio::time::Instant::now() + POLL_INTERVAL;
    let mut poll = tokio::time::interval_at(start, POLL_INTERVAL);
    loop {
        if *state.shutdown.borrow() {
            drain_signals(
                &state.roots,
                state.catalog.as_ref(),
                &state.bridge,
                &mut state.signals,
                &state.output,
            )
            .await;
            break;
        }
        tokio::select! {
            biased;
            changed = state.shutdown.changed() => {
                if changed.is_err() || *state.shutdown.borrow() {
                    drain_signals(
                        &state.roots,
                        state.catalog.as_ref(),
                        &state.bridge,
                        &mut state.signals,
                        &state.output,
                    ).await;
                    break;
                }
            }
            Some(Control::Flush(done)) = state.control.recv() => {
                drain_signals(
                    &state.roots,
                    state.catalog.as_ref(),
                    &state.bridge,
                    &mut state.signals,
                    &state.output,
                ).await;
                let _ = done.send(());
            }
            Some(signal) = state.signals.recv() => {
                reconcile_signal(
                    &state.roots,
                    state.catalog.as_ref(),
                    &state.bridge,
                    &signal,
                    &state.output,
                ).await;
                reconcile_overflow(
                    &state.roots,
                    state.catalog.as_ref(),
                    &state.bridge,
                    &state.output,
                ).await;
            }
            _ = poll.tick() => {
                reconcile_all(
                    &state.roots,
                    state.catalog.as_ref(),
                    &state.bridge,
                    &state.output,
                    false,
                ).await;
                reconcile_overflow(
                    &state.roots,
                    state.catalog.as_ref(),
                    &state.bridge,
                    &state.output,
                ).await;
            }
            permit = state.output.reserve(), if state.bridge.is_overflowed() => {
                permit.map_err(|_| WatcherError::TaskStopped)?;
                reconcile_overflow(
                    &state.roots,
                    state.catalog.as_ref(),
                    &state.bridge,
                    &state.output,
                ).await;
            }
            else => break,
        }
    }
    Ok(())
}

async fn drain_signals(
    roots: &[WatchRoot],
    catalog: &dyn WatcherCatalog,
    bridge: &WatchSignalBridge,
    signals: &mut mpsc::Receiver<WatchSignal>,
    output: &mpsc::Sender<QueueItem>,
) {
    while let Ok(signal) = signals.try_recv() {
        reconcile_signal(roots, catalog, bridge, &signal, output).await;
    }
    reconcile_overflow(roots, catalog, bridge, output).await;
}

async fn reconcile_overflow(
    roots: &[WatchRoot],
    catalog: &dyn WatcherCatalog,
    bridge: &WatchSignalBridge,
    output: &mpsc::Sender<QueueItem>,
) {
    if bridge.take_overflow() {
        reconcile_all(roots, catalog, bridge, output, true).await;
    }
}

async fn reconcile_startup(
    roots: &[WatchRoot],
    catalog: &dyn WatcherCatalog,
    baseline: &HashMap<SourceKey, u64>,
    output: &mpsc::Sender<QueueItem>,
    bridge: &WatchSignalBridge,
) {
    let root_index = root_index(roots);
    let mut seen = 0_usize;
    catalog.visit(None, None, &mut |source| {
        seen = seen.saturating_add(1);
        let Some(size) = source_size(&root_index, source) else {
            return;
        };
        if baseline.get(&source.key).is_none_or(|prior| size > *prior)
            && output
                .try_send(QueueItem {
                    key: source.key.clone(),
                    target_offset: size,
                    priority: WorkPriority::Live,
                })
                .is_err()
        {
            bridge.mark_overflow();
        }
    });
    if seen >= crate::DISCOVERY_ENTRIES_PER_YIELD {
        tokio::task::yield_now().await;
    }
}

async fn reconcile_signal(
    roots: &[WatchRoot],
    catalog: &dyn WatcherCatalog,
    bridge: &WatchSignalBridge,
    signal: &WatchSignal,
    output: &mpsc::Sender<QueueItem>,
) {
    let root_index = root_index(roots);
    let relative =
        matches!(signal.kind, WatchEventKind::Write).then_some(signal.relative_path.as_path());
    let mut visited = 0_usize;
    catalog.visit(Some(&signal.root_id), relative, &mut |source| {
        visited = visited.saturating_add(1);
        if let Some(size) = source_size(&root_index, source)
            && output
                .try_send(QueueItem {
                    key: source.key.clone(),
                    target_offset: size,
                    priority: WorkPriority::Live,
                })
                .is_err()
        {
            bridge.mark_overflow();
        }
    });
    if visited >= crate::DISCOVERY_ENTRIES_PER_YIELD {
        tokio::task::yield_now().await;
    }
}

async fn reconcile_all(
    roots: &[WatchRoot],
    catalog: &dyn WatcherCatalog,
    bridge: &WatchSignalBridge,
    output: &mpsc::Sender<QueueItem>,
    force_live: bool,
) {
    let root_index = root_index(roots);
    let mut visited = 0_usize;
    catalog.visit(None, None, &mut |source| {
        visited = visited.saturating_add(1);
        if let Some(size) = source_size(&root_index, source)
            && output
                .try_send(QueueItem {
                    key: source.key.clone(),
                    target_offset: size,
                    priority: if force_live {
                        WorkPriority::Live
                    } else {
                        source.priority
                    },
                })
                .is_err()
        {
            bridge.mark_overflow();
        }
    });
    for _ in
        (crate::DISCOVERY_ENTRIES_PER_YIELD..visited).step_by(crate::DISCOVERY_ENTRIES_PER_YIELD)
    {
        tokio::task::yield_now().await;
    }
}

fn snapshot_sizes(roots: &[WatchRoot], catalog: &dyn WatcherCatalog) -> HashMap<SourceKey, u64> {
    let root_index = root_index(roots);
    let mut sizes = HashMap::new();
    catalog.visit(None, None, &mut |source| {
        if let Some(size) = source_size(&root_index, source) {
            sizes.insert(source.key.clone(), size);
        }
    });
    sizes
}

fn root_index(roots: &[WatchRoot]) -> HashMap<&str, &Path> {
    roots
        .iter()
        .map(|root| (root.id.as_str(), root.path.as_path()))
        .collect()
}

fn source_size(roots: &HashMap<&str, &Path>, source: &WatchedSource) -> Option<u64> {
    let root = roots.get(source.root_id.as_str())?;
    let mut directory = rustix::fs::open(
        *root,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .ok()?;
    let mut components = source.relative_path.components().peekable();
    while let Some(Component::Normal(component)) = components.next() {
        let last = components.peek().is_none();
        let flags = rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | if last {
                rustix::fs::OFlags::empty()
            } else {
                rustix::fs::OFlags::DIRECTORY
            };
        directory = rustix::fs::openat(&directory, component, flags, rustix::fs::Mode::empty())
            .map(File::from)
            .ok()?;
    }
    let stat = rustix::fs::fstat(&directory).ok()?;
    rustix::fs::FileType::from_raw_mode(stat.st_mode)
        .is_file()
        .then(|| u64::try_from(stat.st_size).ok())
        .flatten()
}

fn map_event_kind(kind: EventKind) -> WatchEventKind {
    use notify::event::{ModifyKind, RenameMode};
    match kind {
        EventKind::Create(_) => WatchEventKind::Create,
        EventKind::Remove(_) => WatchEventKind::Remove,
        EventKind::Modify(ModifyKind::Name(
            RenameMode::Any
            | RenameMode::Both
            | RenameMode::From
            | RenameMode::Other
            | RenameMode::To,
        )) => WatchEventKind::Rename,
        EventKind::Modify(_) => WatchEventKind::Write,
        EventKind::Access(_) | EventKind::Other | EventKind::Any => WatchEventKind::Reconcile,
    }
}
