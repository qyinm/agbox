use std::{
    ffi::OsStr,
    fmt,
    fs::{self, File},
    future::Future,
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use agbox_core::Provider;
use agbox_store::{CryptoError, KeyProvider, open, seal};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::SourceKey;

pub const MAX_HOOK_PAYLOAD_BYTES: usize = agbox_core::limits::MAX_INLINE_BYTES;
pub const MAX_SPOOL_ENTRY_BYTES: usize = 4 * 1_024;
pub const MAX_SPOOL_BYTES: u64 = 8 * 1_024 * 1_024;
pub const MAX_SPOOL_ENTRIES: usize = 1_024;
const ENVELOPE_OVERHEAD: usize = 5 + 24 + 16;
const MAX_SESSION_ID_BYTES: usize = 1_024;
const MAX_HOOK_PATH_BYTES: usize = 4 * 1_024;
const LOCK_FILE_NAME: &[u8] = b".spool.lock";
const TEMP_SUFFIX: &[u8] = b".agbx.tmp";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventKind {
    SessionStart,
    SessionEnd,
    SourceUpdated,
}

#[derive(Clone, Eq, PartialEq)]
pub struct HookSignal {
    provider: Provider,
    kind: HookEventKind,
    session_hash: String,
    source_id: String,
    generation: u64,
    observed_unix_seconds: i64,
    target_size: u64,
}

impl fmt::Debug for HookSignal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookSignal")
            .field("provider", &self.provider)
            .field("kind", &self.kind)
            .field("generation", &self.generation)
            .field("target_size", &self.target_size)
            .finish_non_exhaustive()
    }
}

impl Serialize for HookSignal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        HookSignalWire::from(self).serialize(serializer)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HookSignalWire {
    provider: Provider,
    kind: HookEventKind,
    session_hash: String,
    source_id: String,
    generation: u64,
    observed_unix_seconds: i64,
    target_size: u64,
}

impl From<&HookSignal> for HookSignalWire {
    fn from(value: &HookSignal) -> Self {
        Self {
            provider: value.provider,
            kind: value.kind,
            session_hash: value.session_hash.clone(),
            source_id: value.source_id.clone(),
            generation: value.generation,
            observed_unix_seconds: value.observed_unix_seconds,
            target_size: value.target_size,
        }
    }
}

impl HookSignal {
    /// Normalizes a hook signal and hashes its provider-native session identifier.
    ///
    /// # Errors
    ///
    /// Rejects empty or oversized session identifiers and normalized signals
    /// that do not fit the spool entry contract.
    fn new(
        provider: Provider,
        kind: HookEventKind,
        native_session_id: impl AsRef<[u8]>,
        source: &SourceKey,
        observed_at: OffsetDateTime,
        target_size: u64,
    ) -> Result<Self, SpoolError> {
        let native_session_id = native_session_id.as_ref();
        if native_session_id.is_empty() || native_session_id.len() > MAX_SESSION_ID_BYTES {
            return Err(SpoolError::InvalidPayload);
        }
        let signal = Self {
            provider,
            kind,
            session_hash: blake3::hash(native_session_id).to_hex().to_string(),
            source_id: source.source_id().to_owned(),
            generation: source.generation(),
            observed_unix_seconds: observed_at.unix_timestamp(),
            target_size,
        };
        signal.validate()?;
        Ok(signal)
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn fixture_for_test(
        provider: Provider,
        kind: HookEventKind,
        native_session_id: impl AsRef<[u8]>,
        source: &SourceKey,
        observed_at: OffsetDateTime,
        target_size: u64,
    ) -> Result<Self, SpoolError> {
        Self::new(
            provider,
            kind,
            native_session_id,
            source,
            observed_at,
            target_size,
        )
    }

    /// Streaming-extracts only the normalized allowlisted hook fields.
    ///
    /// Unknown, prompt, message, tool, and environment values are skipped by
    /// Serde's streaming deserializer and are never retained in the signal.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, unknown-kind, or unverified inputs.
    pub fn from_reader<R: Read>(
        reader: R,
        verifier: &dyn HookSourceVerifier,
        observed_at: OffsetDateTime,
    ) -> Result<Self, SpoolError> {
        let mut reader = LimitedReader::new(reader, MAX_HOOK_PAYLOAD_BYTES);
        let parsed = {
            let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
            let parsed = RawHookInput::deserialize(&mut deserializer);
            let ended = parsed.as_ref().ok().map(|_| deserializer.end());
            match (parsed, ended) {
                (Ok(value), Some(Ok(()))) => Ok(value),
                _ => Err(SpoolError::InvalidPayload),
            }
        };
        if reader.exceeded {
            return Err(SpoolError::PayloadTooLarge);
        }
        let parsed = parsed?;
        let provider = parse_provider(&parsed.provider)?;
        let kind = parse_kind(&parsed.hook_event_name)?;
        if parsed.session_id.is_empty()
            || parsed.session_id.len() > MAX_SESSION_ID_BYTES
            || parsed.transcript_path.as_os_str().as_bytes().len() > MAX_HOOK_PATH_BYTES
        {
            return Err(SpoolError::InvalidPayload);
        }
        let (source, target_size) = verifier
            .verify(provider, &parsed.transcript_path, parsed.target_size)
            .ok_or(SpoolError::UnverifiedSource)?;
        Self::new(
            provider,
            kind,
            parsed.session_id.as_bytes(),
            &source,
            observed_at,
            target_size,
        )
    }

    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }

    #[must_use]
    pub const fn kind(&self) -> HookEventKind {
        self.kind
    }

    #[must_use]
    pub const fn target_size(&self) -> u64 {
        self.target_size
    }

    /// Recreates the validated source generation key.
    ///
    /// # Errors
    ///
    /// Returns an invalid-entry error only if the in-memory invariant was violated.
    pub fn source_key(&self) -> Result<SourceKey, SpoolError> {
        SourceKey::new(self.source_id.clone(), self.generation)
            .map_err(|_| SpoolError::InvalidEntry)
    }

    fn from_wire(wire: HookSignalWire) -> Result<Self, SpoolError> {
        let signal = Self {
            provider: wire.provider,
            kind: wire.kind,
            session_hash: wire.session_hash,
            source_id: wire.source_id,
            generation: wire.generation,
            observed_unix_seconds: wire.observed_unix_seconds,
            target_size: wire.target_size,
        };
        signal.validate()?;
        Ok(signal)
    }

    fn validate(&self) -> Result<(), SpoolError> {
        if self.session_hash.len() != 64
            || !self
                .session_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || SourceKey::new(self.source_id.clone(), self.generation).is_err()
        {
            return Err(SpoolError::InvalidEntry);
        }
        let encoded = serde_json::to_vec(&HookSignalWire::from(self))
            .map_err(|_| SpoolError::InvalidEntry)?;
        if encoded.len() > MAX_SPOOL_ENTRY_BYTES {
            return Err(SpoolError::Full);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RawHookInput {
    provider: String,
    hook_event_name: String,
    session_id: String,
    #[serde(alias = "source_path")]
    transcript_path: PathBuf,
    target_size: u64,
}

pub trait HookSourceVerifier: fmt::Debug + Send + Sync {
    fn verify(&self, provider: Provider, path: &Path, target_size: u64)
    -> Option<(SourceKey, u64)>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HookSpoolLimits {
    pub max_entries: usize,
    pub max_bytes: u64,
    pub max_entry_bytes: usize,
}

impl Default for HookSpoolLimits {
    fn default() -> Self {
        Self {
            max_entries: MAX_SPOOL_ENTRIES,
            max_bytes: MAX_SPOOL_BYTES,
            max_entry_bytes: MAX_SPOOL_ENTRY_BYTES,
        }
    }
}

#[derive(thiserror::Error)]
pub enum SpoolError {
    #[error("hook payload is invalid")]
    InvalidPayload,
    #[error("hook payload exceeds its byte limit")]
    PayloadTooLarge,
    #[error("hook source locator is not verified")]
    UnverifiedSource,
    #[error("hook spool is full")]
    Full,
    #[error("hook spool entry is invalid")]
    InvalidEntry,
    #[error("hook reconciliation did not commit")]
    CommitFailed,
    #[error("hook spool filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("hook spool encryption failed")]
    Crypto(#[from] CryptoError),
}

impl fmt::Debug for SpoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPayload => "InvalidPayload",
            Self::PayloadTooLarge => "PayloadTooLarge",
            Self::UnverifiedSource => "UnverifiedSource",
            Self::Full => "Full",
            Self::InvalidEntry => "InvalidEntry",
            Self::CommitFailed => "CommitFailed",
            Self::Io(_) => "Io",
            Self::Crypto(_) => "Crypto",
        })
    }
}

pub struct HookSpool {
    directory_path: PathBuf,
    directory: File,
    key: Zeroizing<[u8; 32]>,
    limits: HookSpoolLimits,
    sequence: AtomicU64,
    gate: Mutex<()>,
    drain_gate: tokio::sync::Mutex<()>,
}

impl fmt::Debug for HookSpool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookSpool")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl HookSpool {
    /// Opens or creates an owner-only encrypted hook spool.
    ///
    /// # Errors
    ///
    /// Rejects unsafe directories or unavailable encryption credentials.
    pub fn new(
        directory: impl AsRef<Path>,
        keys: Arc<dyn KeyProvider>,
    ) -> Result<Self, SpoolError> {
        Self::with_limits(directory, keys, HookSpoolLimits::default())
    }

    /// Opens a spool with limits no larger than the production hard caps.
    ///
    /// # Errors
    ///
    /// Rejects zero, oversized, or filesystem-unsafe configurations.
    #[allow(clippy::needless_pass_by_value)] // One load severs spool lifetime from the provider.
    pub fn with_limits(
        directory: impl AsRef<Path>,
        keys: Arc<dyn KeyProvider>,
        limits: HookSpoolLimits,
    ) -> Result<Self, SpoolError> {
        validate_limits(limits)?;
        let (directory_path, directory) = prepare_directory(directory.as_ref())?;
        let key = keys.master_key()?;
        let spool = Self {
            directory_path,
            directory,
            key,
            limits,
            sequence: AtomicU64::new(0),
            gate: Mutex::new(()),
            drain_gate: tokio::sync::Mutex::new(()),
        };
        {
            let _lock = spool.lock_sync()?;
            spool.cleanup_temps()?;
        }
        Ok(spool)
    }

    /// Encrypts and atomically adds a normalized entry after checking all caps.
    ///
    /// # Errors
    ///
    /// Returns [`SpoolError::Full`] without deleting existing entries when any
    /// count, byte, or per-entry cap would be exceeded.
    pub fn enqueue(&self, signal: &HookSignal) -> Result<(), SpoolError> {
        let _guard = self.gate.lock().map_err(|_| SpoolError::InvalidEntry)?;
        let _lock = self.lock_sync()?;
        self.cleanup_temps()?;
        let plaintext =
            Zeroizing::new(serde_json::to_vec(signal).map_err(|_| SpoolError::InvalidEntry)?);
        if plaintext.len() > self.limits.max_entry_bytes {
            return Err(SpoolError::Full);
        }
        let entries = self.scan_entries()?;
        let current_bytes = entries
            .iter()
            .try_fold(0_u64, |total, entry| total.checked_add(entry.bytes))
            .ok_or(SpoolError::Full)?;
        if entries.len() >= self.limits.max_entries {
            return Err(SpoolError::Full);
        }
        let name = self.next_name()?;
        let envelope = seal(&self.key, name.as_bytes(), plaintext.as_slice())?;
        let envelope_bytes = u64::try_from(envelope.len()).map_err(|_| SpoolError::Full)?;
        if current_bytes
            .checked_add(envelope_bytes)
            .is_none_or(|value| value > self.limits.max_bytes)
        {
            return Err(SpoolError::Full);
        }
        let temporary = format!(".{name}.tmp");
        let mut file = rustix::fs::openat(
            &self.directory,
            temporary.as_str(),
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map(File::from)
        .map_err(io::Error::from)?;
        rustix::fs::fchmod(&file, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR)
            .map_err(io::Error::from)?;
        let mut cleanup = TempCleanup {
            directory: &self.directory,
            name: temporary.as_bytes(),
            published: false,
        };
        file.write_all(&envelope)?;
        file.sync_all()?;
        rustix::fs::renameat_with(
            &self.directory,
            temporary.as_str(),
            &self.directory,
            name.as_str(),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)?;
        cleanup.published = true;
        self.directory.sync_all()?;
        Ok(())
    }

    /// Drains encrypted entries in lexical creation order.
    ///
    /// An entry is unlinked only after `commit` resolves successfully. A failed
    /// commit or invalid entry remains available for transcript-backed recovery.
    ///
    /// # Errors
    ///
    /// Stops at the first invalid entry or failed commit.
    pub async fn drain<F, Fut, E>(&self, mut commit: F) -> Result<usize, SpoolError>
    where
        F: FnMut(HookSignal) -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        let _drain = self.drain_gate.lock().await;
        let _file_lock = self.lock_async().await?;
        self.cleanup_temps()?;
        let entries = self.scan_entries()?;
        let mut committed = 0_usize;
        for entry in entries {
            let plaintext = self.read_entry(&entry)?;
            let wire: HookSignalWire =
                serde_json::from_slice(&plaintext).map_err(|_| SpoolError::InvalidEntry)?;
            let signal = HookSignal::from_wire(wire)?;
            commit(signal).await.map_err(|_| SpoolError::CommitFailed)?;
            let current = rustix::fs::statat(
                &self.directory,
                OsStr::from_bytes(&entry.name),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(io::Error::from)?;
            if !entry.matches(&current) {
                return Err(SpoolError::InvalidEntry);
            }
            rustix::fs::unlinkat(
                &self.directory,
                OsStr::from_bytes(&entry.name),
                rustix::fs::AtFlags::empty(),
            )
            .map_err(io::Error::from)?;
            self.directory.sync_all()?;
            committed = committed.saturating_add(1);
        }
        Ok(committed)
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn entry_paths(&self) -> Result<Vec<PathBuf>, SpoolError> {
        let _guard = self.gate.lock().map_err(|_| SpoolError::InvalidEntry)?;
        let _lock = self.lock_sync()?;
        self.cleanup_temps()?;
        Ok(self
            .scan_entries()?
            .into_iter()
            .map(|entry| self.directory_path.join(OsStr::from_bytes(&entry.name)))
            .collect())
    }

    fn read_entry(&self, entry: &SpoolEntry) -> Result<Zeroizing<Vec<u8>>, SpoolError> {
        let max_envelope = self
            .limits
            .max_entry_bytes
            .checked_add(ENVELOPE_OVERHEAD)
            .ok_or(SpoolError::InvalidEntry)?;
        let file = rustix::fs::openat(
            &self.directory,
            OsStr::from_bytes(&entry.name),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)?;
        let current = rustix::fs::fstat(&file).map_err(io::Error::from)?;
        if !entry.matches(&current) {
            return Err(SpoolError::InvalidEntry);
        }
        let entry_capacity = usize::try_from(entry.bytes).unwrap_or(usize::MAX);
        let mut bytes = Vec::with_capacity(max_envelope.min(entry_capacity));
        file.take(u64::try_from(max_envelope + 1).map_err(|_| SpoolError::InvalidEntry)?)
            .read_to_end(&mut bytes)?;
        if bytes.len() > max_envelope {
            return Err(SpoolError::InvalidEntry);
        }
        open(&self.key, &entry.name, &bytes)
            .map(Zeroizing::new)
            .map_err(|_| SpoolError::InvalidEntry)
    }

    fn scan_entries(&self) -> Result<Vec<SpoolEntry>, SpoolError> {
        let mut entries = Vec::with_capacity(self.limits.max_entries.min(64));
        let directory = directory_stream(&self.directory)?;
        let mut visited = 0_usize;
        for entry in directory {
            let entry = entry.map_err(io::Error::from)?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") || name == LOCK_FILE_NAME {
                continue;
            }
            visited = visited.saturating_add(1);
            if visited > self.limits.max_entries + MAX_SPOOL_ENTRIES {
                return Err(SpoolError::Full);
            }
            if name.ends_with(TEMP_SUFFIX) || !name.ends_with(b".agbx") {
                return Err(SpoolError::InvalidEntry);
            }
            let stat = rustix::fs::statat(
                &self.directory,
                OsStr::from_bytes(name),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(io::Error::from)?;
            if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file()
                || stat.st_mode & 0o077 != 0
            {
                return Err(SpoolError::InvalidEntry);
            }
            entries.push(SpoolEntry {
                name: name.to_vec(),
                bytes: u64::try_from(stat.st_size).map_err(|_| SpoolError::InvalidEntry)?,
                device: u64::try_from(stat.st_dev).map_err(|_| SpoolError::InvalidEntry)?,
                inode: u128::from(stat.st_ino),
            });
            if entries.len() > self.limits.max_entries {
                return Err(SpoolError::Full);
            }
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn next_name(&self) -> Result<String, SpoolError> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SpoolError::InvalidEntry)?
            .as_nanos();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        Ok(format!("{nanos:020}-{sequence:016x}.agbx"))
    }

    fn cleanup_temps(&self) -> Result<(), SpoolError> {
        let directory = directory_stream(&self.directory)?;
        let mut temps = 0_usize;
        for entry in directory {
            let entry = entry.map_err(io::Error::from)?;
            let name = entry.file_name().to_bytes();
            if name.ends_with(TEMP_SUFFIX) {
                temps = temps.saturating_add(1);
                if temps > MAX_SPOOL_ENTRIES {
                    return Err(SpoolError::Full);
                }
                rustix::fs::unlinkat(
                    &self.directory,
                    OsStr::from_bytes(name),
                    rustix::fs::AtFlags::empty(),
                )
                .map_err(io::Error::from)?;
            }
        }
        if temps != 0 {
            self.directory.sync_all()?;
        }
        Ok(())
    }

    fn lock_sync(&self) -> Result<OwnedSpoolLock, SpoolError> {
        OwnedSpoolLock::acquire(open_lock_file(&self.directory)?)
    }

    async fn lock_async(&self) -> Result<OwnedSpoolLock, SpoolError> {
        let directory = self.directory.try_clone()?;
        tokio::task::spawn_blocking(move || {
            let lock = open_lock_file(&directory)?;
            OwnedSpoolLock::acquire(lock)
        })
        .await
        .map_err(|_| SpoolError::InvalidEntry)?
    }
}

struct SpoolEntry {
    name: Vec<u8>,
    bytes: u64,
    device: u64,
    inode: u128,
}

impl SpoolEntry {
    fn matches(&self, stat: &rustix::fs::Stat) -> bool {
        u64::try_from(stat.st_dev).ok() == Some(self.device)
            && u128::from(stat.st_ino) == self.inode
    }
}

struct TempCleanup<'a> {
    directory: &'a File,
    name: &'a [u8],
    published: bool,
}

impl Drop for TempCleanup<'_> {
    fn drop(&mut self) {
        if !self.published {
            let _ = rustix::fs::unlinkat(
                self.directory,
                OsStr::from_bytes(self.name),
                rustix::fs::AtFlags::empty(),
            );
        }
    }
}

struct OwnedSpoolLock(File);

impl OwnedSpoolLock {
    fn acquire(file: File) -> Result<Self, SpoolError> {
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(io::Error::from)?;
        Ok(Self(file))
    }
}

impl Drop for OwnedSpoolLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.0, rustix::fs::FlockOperation::Unlock);
    }
}

fn validate_limits(limits: HookSpoolLimits) -> Result<(), SpoolError> {
    if limits.max_entries == 0
        || limits.max_entries > MAX_SPOOL_ENTRIES
        || limits.max_bytes == 0
        || limits.max_bytes > MAX_SPOOL_BYTES
        || limits.max_entry_bytes == 0
        || limits.max_entry_bytes > MAX_SPOOL_ENTRY_BYTES
    {
        return Err(SpoolError::Full);
    }
    Ok(())
}

fn prepare_directory(path: &Path) -> Result<(PathBuf, File), SpoolError> {
    if path.as_os_str().is_empty() {
        return Err(SpoolError::InvalidEntry);
    }
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(SpoolError::InvalidEntry);
    }
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    let canonical = path.canonicalize()?;
    let metadata = fs::symlink_metadata(&canonical)?;
    if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(SpoolError::InvalidEntry);
    }
    let directory = rustix::fs::open(
        &canonical,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)?;
    rustix::fs::fchmod(
        &directory,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
    )
    .map_err(io::Error::from)?;
    let stat = rustix::fs::fstat(&directory).map_err(io::Error::from)?;
    if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir()
        || u64::try_from(stat.st_dev).ok() != Some(metadata.dev())
        || u128::from(stat.st_ino) != u128::from(metadata.ino())
    {
        return Err(SpoolError::InvalidEntry);
    }
    Ok((canonical, directory))
}

fn open_lock_file(directory: &File) -> Result<File, SpoolError> {
    let file = rustix::fs::openat(
        directory,
        OsStr::from_bytes(LOCK_FILE_NAME),
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .map(File::from)
    .map_err(io::Error::from)?;
    rustix::fs::fchmod(&file, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR)
        .map_err(io::Error::from)?;
    let stat = rustix::fs::fstat(&file).map_err(io::Error::from)?;
    if !rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(SpoolError::InvalidEntry);
    }
    Ok(file)
}

fn directory_stream(directory: &File) -> Result<rustix::fs::Dir, SpoolError> {
    let stream = rustix::fs::openat(
        directory,
        ".",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)?;
    rustix::fs::Dir::new(stream)
        .map_err(io::Error::from)
        .map_err(SpoolError::from)
}

fn parse_provider(value: &str) -> Result<Provider, SpoolError> {
    match value {
        "claude" => Ok(Provider::Claude),
        "codex" => Ok(Provider::Codex),
        _ => Err(SpoolError::InvalidPayload),
    }
}

fn parse_kind(value: &str) -> Result<HookEventKind, SpoolError> {
    match value {
        "session_start" => Ok(HookEventKind::SessionStart),
        "session_end" | "stop" => Ok(HookEventKind::SessionEnd),
        "source_updated" => Ok(HookEventKind::SourceUpdated),
        _ => Err(SpoolError::InvalidPayload),
    }
}

struct LimitedReader<R> {
    inner: R,
    remaining: usize,
    exceeded: bool,
}

impl<R> LimitedReader<R> {
    const fn new(inner: R, limit: usize) -> Self {
        Self {
            inner,
            remaining: limit,
            exceeded: false,
        }
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            let mut probe = [0_u8; 1];
            let read = self.inner.read(&mut probe)?;
            self.exceeded = read != 0;
            return Ok(0);
        }
        let limit = buffer.len().min(self.remaining);
        let read = self.inner.read(&mut buffer[..limit])?;
        self.remaining -= read;
        Ok(read)
    }
}
