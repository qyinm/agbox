use std::{
    collections::{HashSet, VecDeque},
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

use agbox_adapters::{DiscoveredSource, RootSpec, SourceAdapter};
use agbox_core::Provider;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::identity::unix_identity;

pub const DISCOVERY_ENTRIES_PER_YIELD: usize = 256;
pub const MAX_DISCOVERY_CURSOR_BYTES: usize = 32 * 1024;
const MAX_DISCOVERY_FAULTS: usize = 32;
const MAX_DIRECTORY_RETRIES: u8 = 3;
const MAX_LIVE_DIRECTORY_STREAMS: usize = 32;

#[derive(Clone)]
pub struct DiscoveryCursor {
    root_device: u64,
    root_inode: u64,
    pending_directories: VecDeque<DirectoryCursor>,
}

impl Serialize for DiscoveryCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct Wire<'a> {
            root_device: u64,
            root_inode: u64,
            pending_directories: Vec<WireDirectory<'a>>,
        }
        #[derive(Serialize)]
        struct WireDirectory<'a> {
            up: usize,
            suffix: &'a [Vec<u8>],
            entries_consumed: u64,
            device: u64,
            inode: u64,
            mtime_seconds: i64,
            mtime_nanoseconds: i64,
            ctime_seconds: i64,
            ctime_nanoseconds: i64,
            retry_attempts: u8,
        }
        let mut previous: &[Vec<u8>] = &[];
        let mut directories = Vec::with_capacity(self.pending_directories.len());
        for directory in &self.pending_directories {
            let common = previous
                .iter()
                .zip(&directory.components)
                .take_while(|(a, b)| a == b)
                .count();
            directories.push(WireDirectory {
                up: previous.len() - common,
                suffix: &directory.components[common..],
                entries_consumed: directory.entries_consumed,
                device: directory.device,
                inode: directory.inode,
                mtime_seconds: directory.mtime_seconds,
                mtime_nanoseconds: directory.mtime_nanoseconds,
                ctime_seconds: directory.ctime_seconds,
                ctime_nanoseconds: directory.ctime_nanoseconds,
                retry_attempts: directory.retry_attempts,
            });
            previous = &directory.components;
        }
        Wire {
            root_device: self.root_device,
            root_inode: self.root_inode,
            pending_directories: directories,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DiscoveryCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            root_device: u64,
            root_inode: u64,
            pending_directories: Vec<WireDirectory>,
        }
        #[derive(Deserialize)]
        struct WireDirectory {
            up: usize,
            suffix: Vec<Vec<u8>>,
            entries_consumed: u64,
            device: u64,
            inode: u64,
            mtime_seconds: i64,
            mtime_nanoseconds: i64,
            ctime_seconds: i64,
            ctime_nanoseconds: i64,
            retry_attempts: u8,
        }
        let wire = Wire::deserialize(deserializer)?;
        let mut previous = Vec::new();
        let mut pending_directories = VecDeque::new();
        for directory in wire.pending_directories {
            if directory.up > previous.len() {
                return Err(serde::de::Error::custom("invalid discovery cursor delta"));
            }
            previous.truncate(previous.len() - directory.up);
            previous.extend(directory.suffix);
            pending_directories.push_back(DirectoryCursor {
                components: previous.clone(),
                entries_consumed: directory.entries_consumed,
                device: directory.device,
                inode: directory.inode,
                mtime_seconds: directory.mtime_seconds,
                mtime_nanoseconds: directory.mtime_nanoseconds,
                ctime_seconds: directory.ctime_seconds,
                ctime_nanoseconds: directory.ctime_nanoseconds,
                retry_attempts: directory.retry_attempts,
            });
        }
        Ok(Self {
            root_device: wire.root_device,
            root_inode: wire.root_inode,
            pending_directories,
        })
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
struct DirectoryCursor {
    components: Vec<Vec<u8>>,
    entries_consumed: u64,
    device: u64,
    inode: u64,
    mtime_seconds: i64,
    mtime_nanoseconds: i64,
    ctime_seconds: i64,
    ctime_nanoseconds: i64,
    retry_attempts: u8,
}

impl fmt::Debug for DiscoveryCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscoveryCursor")
            .field("pending_directories", &self.pending_directories.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryFaultClass {
    MetadataUnavailable,
    DirectoryUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryFault {
    pub class: DiscoveryFaultClass,
}

#[derive(Debug)]
pub struct DiscoveryBatch {
    pub sources: Vec<DiscoveredSource>,
    pub faults: Vec<DiscoveryFault>,
    pub cursor: Option<DiscoveryCursor>,
    pub visited_entries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DiscoveryError {
    #[error("discovery root is unavailable")]
    RootUnavailable,
    #[error("discovery cursor is invalid")]
    InvalidCursor,
    #[error("discovery provider adapter is unavailable")]
    AdapterUnavailable,
    #[error("discovery cursor capacity was exceeded")]
    CursorCapacity,
}

pub struct DiscoveryWalker {
    provider: Provider,
    adapter: &'static dyn SourceAdapter,
    spec: RootSpec,
    root: File,
    cursor: DiscoveryCursor,
    active: Vec<ActiveDirectory>,
    restored: bool,
}

impl fmt::Debug for DiscoveryWalker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscoveryWalker")
            .field("provider", &self.provider)
            .field("class", &self.spec.class)
            .field("recursive", &self.spec.recursive)
            .field(
                "pending_directories",
                &self.cursor.pending_directories.len(),
            )
            .finish_non_exhaustive()
    }
}

struct PageEntry {
    name: Vec<u8>,
    stat: rustix::fs::Stat,
}

struct DirectoryPage {
    entries: Vec<PageEntry>,
    eof: bool,
    fault: bool,
    invalid_cursor: bool,
    reads: usize,
}

struct ActiveDirectory {
    cursor: DirectoryCursor,
    iterator: Dir,
    recovery_remaining: u64,
}

enum OpenDirectoryError {
    Unavailable,
    InvalidCursor,
}

impl DiscoveryWalker {
    /// Binds discovery to one open canonical root directory descriptor.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the root, provider, or initial cursor
    /// cannot satisfy the no-follow and serialization contracts.
    pub fn new(provider: Provider, mut spec: RootSpec) -> Result<Self, DiscoveryError> {
        let canonical = spec
            .path
            .canonicalize()
            .map_err(|_| DiscoveryError::RootUnavailable)?;
        let root = open_root_nofollow(&canonical)?;
        let stat = rustix::fs::fstat(&root).map_err(|_| DiscoveryError::RootUnavailable)?;
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            return Err(DiscoveryError::RootUnavailable);
        }
        let (root_device, root_inode) = stat_identity(&stat)?;
        spec.path = canonical;
        let adapter = agbox_adapters::adapters()
            .iter()
            .copied()
            .find(|adapter| adapter.provider() == provider)
            .ok_or(DiscoveryError::AdapterUnavailable)?;
        let root_cursor = DirectoryCursor {
            components: Vec::new(),
            entries_consumed: 0,
            device: root_device,
            inode: root_inode,
            mtime_seconds: stat.st_mtime,
            mtime_nanoseconds: stat.st_mtime_nsec,
            ctime_seconds: stat.st_ctime,
            ctime_nanoseconds: stat.st_ctime_nsec,
            retry_attempts: 0,
        };
        let cursor = DiscoveryCursor {
            root_device,
            root_inode,
            pending_directories: VecDeque::from([root_cursor]),
        };
        ensure_cursor_size(&cursor)?;
        Ok(Self {
            provider,
            adapter,
            spec,
            root,
            cursor,
            active: Vec::new(),
            restored: false,
        })
    }

    /// Restores a serialized byte-exact relative cursor against a freshly
    /// opened root descriptor.
    ///
    /// # Errors
    ///
    /// Rejects malformed component bytes, oversized state, or a changed root.
    pub fn from_cursor(
        provider: Provider,
        spec: RootSpec,
        cursor: DiscoveryCursor,
    ) -> Result<Self, DiscoveryError> {
        validate_cursor(&cursor)?;
        let mut walker = Self::new(provider, spec)?;
        if walker.cursor.root_device != cursor.root_device
            || walker.cursor.root_inode != cursor.root_inode
        {
            return Err(DiscoveryError::InvalidCursor);
        }
        walker.cursor = cursor;
        walker.restored = true;
        Ok(walker)
    }

    /// Enumerates no more than 256 actual directory entries.
    ///
    /// Entries are sorted only within the bounded OS-cookie page. Concurrent
    /// directory insertion/rename is best-effort and is reconciled by a later
    /// root scan; identity-sensitive opens always revalidate the snapshot.
    ///
    /// # Errors
    ///
    /// Returns a cursor-capacity or root-identity error without dropping an
    /// unaccounted directory entry.
    #[allow(clippy::too_many_lines)] // The transaction-like cursor update is intentionally co-located.
    pub fn next_batch(&mut self, limit: usize) -> Result<DiscoveryBatch, DiscoveryError> {
        let hard_limit = limit.min(DISCOVERY_ENTRIES_PER_YIELD);
        let mut sources = Vec::new();
        let mut faults = Vec::new();
        let mut visited_entries = 0;
        let mut operations = 0;

        while visited_entries < hard_limit && operations < hard_limit {
            let Some(mut directory_cursor) = self.cursor.pending_directories.pop_front() else {
                break;
            };
            operations += 1;
            if self
                .active
                .last()
                .is_none_or(|active| active.cursor != directory_cursor)
            {
                let directory = match self.open_directory(&directory_cursor, self.restored) {
                    Ok(directory) => directory,
                    Err(OpenDirectoryError::Unavailable) => {
                        bounded_fault(&mut faults, DiscoveryFaultClass::DirectoryUnavailable);
                        retry_or_quarantine(&mut self.cursor, &mut directory_cursor);
                        self.active.clear();
                        continue;
                    }
                    Err(OpenDirectoryError::InvalidCursor) => {
                        return Err(DiscoveryError::InvalidCursor);
                    }
                };
                let Ok(iterator) = Dir::new(directory) else {
                    bounded_fault(&mut faults, DiscoveryFaultClass::DirectoryUnavailable);
                    retry_or_quarantine(&mut self.cursor, &mut directory_cursor);
                    self.active.clear();
                    continue;
                };
                if self.active.len() >= MAX_LIVE_DIRECTORY_STREAMS {
                    let _ = self.active.remove(0);
                }
                self.active.push(ActiveDirectory {
                    recovery_remaining: directory_cursor.entries_consumed,
                    cursor: directory_cursor.clone(),
                    iterator,
                });
            }
            if self.open_directory(&directory_cursor, false).is_err() {
                self.active.clear();
                bounded_fault(&mut faults, DiscoveryFaultClass::DirectoryUnavailable);
                retry_or_quarantine(&mut self.cursor, &mut directory_cursor);
                continue;
            }
            let mut page_start = directory_cursor.clone();
            let page = {
                let Some(active) = self.active.last_mut() else {
                    bounded_fault(&mut faults, DiscoveryFaultClass::DirectoryUnavailable);
                    break;
                };
                read_page(active, hard_limit - visited_entries, &mut faults)
            };
            visited_entries = visited_entries.saturating_add(page.reads);
            if self.open_directory(&directory_cursor, false).is_err() {
                self.active.clear();
                bounded_fault(&mut faults, DiscoveryFaultClass::DirectoryUnavailable);
                retry_or_quarantine(&mut self.cursor, &mut directory_cursor);
                continue;
            }
            if page.invalid_cursor {
                self.active.clear();
                return Err(DiscoveryError::InvalidCursor);
            }
            if page.fault {
                retry_or_quarantine(&mut self.cursor, &mut page_start);
                self.active.clear();
                continue;
            }

            let mut next_cursor = self.cursor.clone();
            if !page.eof {
                let active_cursor = self
                    .active
                    .last()
                    .map_or_else(|| directory_cursor.clone(), |active| active.cursor.clone());
                next_cursor.pending_directories.push_front(active_cursor);
            }

            let mut child_directory = None;
            let mut page_sources = Vec::new();
            for entry in page.entries {
                let relative = join_components(&directory_cursor.components, &entry.name);
                if skipped_components(&relative) {
                    continue;
                }
                let file_type = FileType::from_raw_mode(entry.stat.st_mode);
                if file_type.is_dir() && self.spec.recursive {
                    let (device, inode) = stat_identity(&entry.stat)?;
                    child_directory = Some(DirectoryCursor {
                        components: relative,
                        entries_consumed: 0,
                        device,
                        inode,
                        mtime_seconds: entry.stat.st_mtime,
                        mtime_nanoseconds: entry.stat.st_mtime_nsec,
                        ctime_seconds: entry.stat.st_ctime,
                        ctime_nanoseconds: entry.stat.st_ctime_nsec,
                        retry_attempts: 0,
                    });
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let relative_path = components_to_path(&relative);
                if !metadata_only_adapter_match(self.provider, &self.spec, &relative_path) {
                    continue;
                }
                let (device, inode) = stat_identity(&entry.stat)?;
                let file_identity = unix_identity(device, inode);
                let mtime = stat_time(&entry.stat).ok_or(DiscoveryError::RootUnavailable)?;
                let ctime = stat_ctime(&entry.stat).ok_or(DiscoveryError::RootUnavailable)?;
                let size = u64::try_from(entry.stat.st_size)
                    .map_err(|_| DiscoveryError::RootUnavailable)?;
                let session_time =
                    self.adapter
                        .trusted_session_time(&self.spec, &relative_path, mtime);
                page_sources.push(DiscoveredSource {
                    source_id: stable_source_id(
                        self.provider,
                        self.cursor.root_device,
                        self.cursor.root_inode,
                        &file_identity,
                    ),
                    provider: self.provider,
                    root: self.spec.path.clone(),
                    path: self.spec.path.join(&relative_path),
                    class: self.spec.class,
                    file_identity,
                    generation: 1,
                    size,
                    mtime,
                    ctime,
                    session_time,
                });
            }

            if let Some(child) = child_directory {
                next_cursor.pending_directories.push_front(child);
            }
            if ensure_cursor_size(&next_cursor).is_err() {
                let mut rollback = self.cursor.clone();
                rollback.pending_directories.push_front(page_start);
                ensure_cursor_size(&rollback)?;
                self.cursor = rollback;
                let _ = self.active.pop();
                return Err(DiscoveryError::CursorCapacity);
            }
            self.cursor = next_cursor;
            sources.extend(page_sources);
            if page.eof {
                let _ = self.active.pop();
            }
            if page.fault {
                break;
            }
        }

        let cursor = (!self.cursor.pending_directories.is_empty()).then(|| self.cursor.clone());
        Ok(DiscoveryBatch {
            sources,
            faults,
            cursor,
            visited_entries,
        })
    }

    fn open_directory(
        &self,
        cursor: &DirectoryCursor,
        check_snapshot: bool,
    ) -> Result<File, OpenDirectoryError> {
        let mut directory = self
            .root
            .try_clone()
            .map_err(|_| OpenDirectoryError::Unavailable)?;
        for component in &cursor.components {
            let name = OsStr::from_bytes(component);
            let parent = directory
                .try_clone()
                .map_err(|_| OpenDirectoryError::Unavailable)?;
            directory = rustix::fs::openat(
                &directory,
                name,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| OpenDirectoryError::Unavailable)?;
            let rebound = rustix::fs::openat(
                &parent,
                name,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| OpenDirectoryError::Unavailable)?;
            if !same_file_identity(&directory, &rebound)? {
                return Err(OpenDirectoryError::InvalidCursor);
            }
        }
        let stat = rustix::fs::fstat(&directory).map_err(|_| OpenDirectoryError::Unavailable)?;
        let (device, inode) =
            stat_identity(&stat).map_err(|_| OpenDirectoryError::InvalidCursor)?;
        if !FileType::from_raw_mode(stat.st_mode).is_dir()
            || device != cursor.device
            || inode != cursor.inode
            || (check_snapshot
                && (stat.st_mtime != cursor.mtime_seconds
                    || stat.st_mtime_nsec != cursor.mtime_nanoseconds
                    || stat.st_ctime != cursor.ctime_seconds
                    || stat.st_ctime_nsec != cursor.ctime_nanoseconds))
        {
            return Err(OpenDirectoryError::InvalidCursor);
        }
        Ok(directory)
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    #[must_use]
    pub fn live_stream_count_for_test(&self) -> usize {
        self.active.len()
    }
}

fn same_file_identity(left: &File, right: &File) -> Result<bool, OpenDirectoryError> {
    let left = rustix::fs::fstat(left).map_err(|_| OpenDirectoryError::Unavailable)?;
    let right = rustix::fs::fstat(right).map_err(|_| OpenDirectoryError::Unavailable)?;
    Ok(left.st_dev == right.st_dev && left.st_ino == right.st_ino)
}

fn open_root_nofollow(path: &Path) -> Result<File, DiscoveryError> {
    if !path.is_absolute() {
        return Err(DiscoveryError::RootUnavailable);
    }
    let mut directory = rustix::fs::open(
        "/",
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| DiscoveryError::RootUnavailable)?;
    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            continue;
        };
        directory = rustix::fs::openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| DiscoveryError::RootUnavailable)?;
    }
    Ok(directory)
}

/// Collapses overlapping-root sightings by provider and filesystem identity,
/// preferring the most-specific verified root.
#[must_use]
pub fn deduplicate_overlapping_sources(
    mut sources: Vec<DiscoveredSource>,
) -> Vec<DiscoveredSource> {
    sources.sort_by(|left, right| {
        right
            .root
            .components()
            .count()
            .cmp(&left.root.components().count())
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut retained = HashSet::new();
    sources
        .retain(|source| retained.insert((source.provider.as_str(), source.file_identity.clone())));
    sources
}

fn read_page(
    active: &mut ActiveDirectory,
    limit: usize,
    faults: &mut Vec<DiscoveryFault>,
) -> DirectoryPage {
    let mut entries = Vec::new();
    let mut reads = 0;
    let mut eof = false;
    let mut fault = false;
    let mut invalid_cursor = false;
    while reads < limit {
        let Some(result) = active.iterator.next() else {
            eof = true;
            invalid_cursor = false;
            break;
        };
        reads += 1;
        let Ok(entry) = result else {
            bounded_fault(faults, DiscoveryFaultClass::DirectoryUnavailable);
            fault = true;
            break;
        };
        if active.recovery_remaining > 0 {
            active.recovery_remaining -= 1;
            continue;
        }
        let Some(consumed) = active.cursor.entries_consumed.checked_add(1) else {
            bounded_fault(faults, DiscoveryFaultClass::DirectoryUnavailable);
            fault = true;
            break;
        };
        active.cursor.entries_consumed = consumed;
        let name = entry.file_name().to_bytes().to_vec();
        if matches!(name.as_slice(), b"." | b"..") {
            continue;
        }
        let Ok(directory_fd) = active.iterator.fd() else {
            bounded_fault(faults, DiscoveryFaultClass::DirectoryUnavailable);
            fault = true;
            break;
        };
        let Ok(stat) = rustix::fs::statat(
            directory_fd,
            OsStr::from_bytes(&name),
            AtFlags::SYMLINK_NOFOLLOW,
        ) else {
            bounded_fault(faults, DiscoveryFaultClass::MetadataUnavailable);
            continue;
        };
        let is_directory = FileType::from_raw_mode(stat.st_mode).is_dir();
        entries.push(PageEntry { name, stat });
        if is_directory {
            break;
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    DirectoryPage {
        entries,
        eof,
        fault,
        invalid_cursor,
        reads,
    }
}

fn validate_cursor(cursor: &DiscoveryCursor) -> Result<(), DiscoveryError> {
    ensure_cursor_size(cursor)?;
    if cursor.pending_directories.iter().all(|directory| {
        directory
            .components
            .iter()
            .all(|component| valid_component(component))
            && directory.retry_attempts <= MAX_DIRECTORY_RETRIES
    }) {
        Ok(())
    } else {
        Err(DiscoveryError::InvalidCursor)
    }
}

fn ensure_cursor_size(cursor: &DiscoveryCursor) -> Result<(), DiscoveryError> {
    let encoded = serde_json::to_vec(cursor).map_err(|_| DiscoveryError::InvalidCursor)?;
    if encoded.len() <= MAX_DISCOVERY_CURSOR_BYTES {
        Ok(())
    } else {
        Err(DiscoveryError::CursorCapacity)
    }
}

fn valid_component(component: &[u8]) -> bool {
    !component.is_empty()
        && component != b"."
        && component != b".."
        && !component.contains(&0)
        && !component.contains(&b'/')
}

fn join_components(parent: &[Vec<u8>], name: &[u8]) -> Vec<Vec<u8>> {
    let mut result = parent.to_vec();
    result.push(name.to_vec());
    result
}

fn components_to_path(components: &[Vec<u8>]) -> PathBuf {
    components
        .iter()
        .fold(PathBuf::new(), |mut path, component| {
            path.push(OsString::from_vec(component.clone()));
            path
        })
}

fn skipped_components(components: &[Vec<u8>]) -> bool {
    components.iter().any(|component| {
        [
            b"backup".as_slice(),
            b"backups".as_slice(),
            b"cache".as_slice(),
            b"caches".as_slice(),
            b"tmp".as_slice(),
            b"temp".as_slice(),
        ]
        .iter()
        .any(|ignored| component.eq_ignore_ascii_case(ignored))
    })
}

fn metadata_only_adapter_match(provider: Provider, root: &RootSpec, relative: &Path) -> bool {
    root.recursive
        && (provider != Provider::Claude || root.class == agbox_adapters::RootClass::Active)
        && relative
            .extension()
            .is_some_and(|extension| extension == "jsonl")
}

fn bounded_fault(faults: &mut Vec<DiscoveryFault>, class: DiscoveryFaultClass) {
    if faults.len() < MAX_DISCOVERY_FAULTS {
        faults.push(DiscoveryFault { class });
    }
}

fn retry_or_quarantine(cursor: &mut DiscoveryCursor, directory: &mut DirectoryCursor) {
    directory.retry_attempts = directory.retry_attempts.saturating_add(1);
    if directory.retry_attempts <= MAX_DIRECTORY_RETRIES {
        cursor.pending_directories.push_back(directory.clone());
    }
}

fn stat_identity(stat: &rustix::fs::Stat) -> Result<(u64, u64), DiscoveryError> {
    let device = u64::try_from(stat.st_dev).map_err(|_| DiscoveryError::RootUnavailable)?;
    Ok((device, stat.st_ino))
}

fn stat_time(stat: &rustix::fs::Stat) -> Option<OffsetDateTime> {
    i128::from(stat.st_mtime)
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(i128::from(stat.st_mtime_nsec)))
        .and_then(|value| OffsetDateTime::from_unix_timestamp_nanos(value).ok())
}

fn stat_ctime(stat: &rustix::fs::Stat) -> Option<OffsetDateTime> {
    i128::from(stat.st_ctime)
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(i128::from(stat.st_ctime_nsec)))
        .and_then(|value| OffsetDateTime::from_unix_timestamp_nanos(value).ok())
}

fn stable_source_id(
    provider: Provider,
    root_device: u64,
    root_inode: u64,
    file_identity: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox.source.fs-identity.v1");
    hash_part(&mut hasher, provider.as_str().as_bytes());
    hash_part(&mut hasher, &root_device.to_le_bytes());
    hash_part(&mut hasher, &root_inode.to_le_bytes());
    hash_part(&mut hasher, file_identity.as_bytes());
    format!("source_{}", &hasher.finalize().to_hex()[..32])
}

fn hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{DirectoryCursor, DiscoveryCursor, MAX_DISCOVERY_CURSOR_BYTES};
    use std::collections::VecDeque;

    #[test]
    fn cursor_serialization_round_trips_arbitrary_component_bytes_exactly() {
        let cursor = DiscoveryCursor {
            root_device: 1,
            root_inode: 2,
            pending_directories: VecDeque::from([DirectoryCursor {
                components: vec![vec![b'd', b'i', b'r', 0xff, 0x80]],
                entries_consumed: 17,
                device: 3,
                inode: 4,
                mtime_seconds: 5,
                mtime_nanoseconds: 6,
                ctime_seconds: 7,
                ctime_nanoseconds: 8,
                retry_attempts: 0,
            }]),
        };
        let encoded = serde_json::to_vec(&cursor).unwrap();
        assert!(encoded.len() <= MAX_DISCOVERY_CURSOR_BYTES);
        let decoded: DiscoveryCursor = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), encoded);
    }
}
