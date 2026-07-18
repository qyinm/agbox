use std::{
    collections::VecDeque,
    fmt, fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use agbox_adapters::{DiscoveredSource, RootSpec, SourceAdapter};
use agbox_core::Provider;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::identity::unix_identity;

pub const DISCOVERY_ENTRIES_PER_YIELD: usize = 256;
pub const MAX_DISCOVERY_CURSOR_BYTES: usize = 32 * 1024;
const CURSOR_SAFETY_BYTES: usize = 512;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DiscoveryCursor {
    pending_relative_entries: VecDeque<PathBuf>,
    pending_directories: VecDeque<DirectoryResume>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DirectoryResume {
    relative_directory: PathBuf,
    after_entry: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryFaultClass {
    MetadataUnavailable,
    DirectoryUnavailable,
    CursorCapacity,
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
}

pub struct DiscoveryWalker {
    provider: Provider,
    adapter: &'static dyn SourceAdapter,
    spec: RootSpec,
    root_identity: String,
    cursor: DiscoveryCursor,
    root_enumerated: bool,
}

impl fmt::Debug for DiscoveryWalker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscoveryWalker")
            .field("provider", &self.provider)
            .field("class", &self.spec.class)
            .field("recursive", &self.spec.recursive)
            .field(
                "pending_relative_entries",
                &self.cursor.pending_relative_entries.len(),
            )
            .field(
                "pending_directories",
                &self.cursor.pending_directories.len(),
            )
            .finish_non_exhaustive()
    }
}

impl DiscoveryWalker {
    /// Binds discovery to one canonical regular directory without opening files.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::RootUnavailable`] for a missing, symlink, or
    /// non-directory root.
    pub fn new(provider: Provider, mut spec: RootSpec) -> Result<Self, DiscoveryError> {
        let original = spec.path.clone();
        let metadata = original
            .symlink_metadata()
            .map_err(|_| DiscoveryError::RootUnavailable)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DiscoveryError::RootUnavailable);
        }
        spec.path = original
            .canonicalize()
            .map_err(|_| DiscoveryError::RootUnavailable)?;
        let canonical_metadata = spec
            .path
            .metadata()
            .map_err(|_| DiscoveryError::RootUnavailable)?;
        let root_identity = unix_identity(canonical_metadata.dev(), canonical_metadata.ino());
        let adapter = agbox_adapters::adapters()
            .iter()
            .copied()
            .find(|adapter| adapter.provider() == provider)
            .ok_or(DiscoveryError::AdapterUnavailable)?;
        Ok(Self {
            provider,
            adapter,
            spec,
            root_identity,
            cursor: DiscoveryCursor {
                pending_relative_entries: VecDeque::new(),
                pending_directories: VecDeque::new(),
            },
            root_enumerated: false,
        })
    }

    /// Restores a previously serialized relative-only cursor under a fresh
    /// descriptor-bound root.
    ///
    /// # Errors
    ///
    /// Rejects absolute, parent, prefix, oversized, or otherwise invalid state.
    pub fn from_cursor(
        provider: Provider,
        spec: RootSpec,
        cursor: DiscoveryCursor,
    ) -> Result<Self, DiscoveryError> {
        if !cursor_is_valid(&cursor) {
            return Err(DiscoveryError::InvalidCursor);
        }
        let mut walker = Self::new(provider, spec)?;
        walker.cursor = cursor;
        walker.root_enumerated = true;
        Ok(walker)
    }

    /// Visits at most 256 metadata entries and never opens source contents.
    ///
    /// # Errors
    ///
    /// Only root/cursor construction errors are terminal. Per-entry failures
    /// are isolated into bounded path-free faults.
    pub fn next_batch(&mut self, limit: usize) -> Result<DiscoveryBatch, DiscoveryError> {
        let hard_limit = limit.min(DISCOVERY_ENTRIES_PER_YIELD);
        let mut faults = Vec::new();
        if !self.root_enumerated {
            self.root_enumerated = true;
            self.enqueue_directory(Path::new(""), None, &mut faults);
        }
        let mut sources = Vec::new();
        let mut visited_entries = 0;
        'entries: while visited_entries < hard_limit {
            let relative = loop {
                if let Some(relative) = self.cursor.pending_relative_entries.pop_front() {
                    break relative;
                }
                let Some(resume) = self.cursor.pending_directories.pop_front() else {
                    break 'entries;
                };
                self.enqueue_directory(
                    &resume.relative_directory,
                    Some(&resume.after_entry),
                    &mut faults,
                );
            };
            visited_entries += 1;
            if skipped_component(&relative) || !safe_relative(&relative) {
                continue;
            }
            let absolute = self.spec.path.join(&relative);
            let Ok(metadata) = absolute.symlink_metadata() else {
                faults.push(DiscoveryFault {
                    class: DiscoveryFaultClass::MetadataUnavailable,
                });
                continue;
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if self.spec.recursive {
                    self.enqueue_directory(&relative, None, &mut faults);
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if !self.adapter.matches(&self.spec, &relative) {
                continue;
            }
            let file_identity = unix_identity(metadata.dev(), metadata.ino());
            let source_id = stable_source_id(self.provider, &self.root_identity, &file_identity);
            let mtime = metadata_time(&metadata).unwrap_or(OffsetDateTime::UNIX_EPOCH);
            let session_time = self
                .adapter
                .trusted_session_time(&self.spec, &relative, mtime);
            sources.push(DiscoveredSource {
                source_id,
                provider: self.provider,
                root: self.spec.path.clone(),
                path: absolute,
                class: self.spec.class,
                file_identity,
                generation: 1,
                size: metadata.len(),
                mtime,
                // Only the provider adapter's bounded path/metadata policy may
                // supply a replay date; discovery never parses source content.
                session_time,
            });
        }
        let cursor = (!self.cursor.pending_relative_entries.is_empty()
            || !self.cursor.pending_directories.is_empty())
        .then(|| self.cursor.clone());
        Ok(DiscoveryBatch {
            sources,
            faults,
            cursor,
            visited_entries,
        })
    }

    fn enqueue_directory(
        &mut self,
        relative: &Path,
        after_entry: Option<&Path>,
        faults: &mut Vec<DiscoveryFault>,
    ) {
        let directory = self.spec.path.join(relative);
        let Ok(read) = fs::read_dir(directory) else {
            faults.push(DiscoveryFault {
                class: DiscoveryFaultClass::DirectoryUnavailable,
            });
            return;
        };
        let mut children = Vec::new();
        for entry in read {
            let Ok(entry) = entry else {
                faults.push(DiscoveryFault {
                    class: DiscoveryFaultClass::DirectoryUnavailable,
                });
                continue;
            };
            let child = relative.join(entry.file_name());
            if safe_relative(&child)
                && !skipped_component(&child)
                && after_entry.is_none_or(|after| child.as_path() > after)
            {
                children.push(child);
            }
        }
        children.sort();
        let prior_entry_count = self.cursor.pending_relative_entries.len();
        let previous_after = after_entry.map(Path::to_path_buf);
        let mut last_enqueued = previous_after.clone();
        let mut has_more = false;
        for child in children {
            self.cursor.pending_relative_entries.push_back(child);
            if cursor_estimated_bytes(&self.cursor)
                >= MAX_DISCOVERY_CURSOR_BYTES.saturating_sub(CURSOR_SAFETY_BYTES)
            {
                let _ = self.cursor.pending_relative_entries.pop_back();
                has_more = true;
                break;
            }
            last_enqueued = self.cursor.pending_relative_entries.back().cloned();
        }
        if has_more {
            let Some(resume_after) = last_enqueued else {
                faults.push(DiscoveryFault {
                    class: DiscoveryFaultClass::CursorCapacity,
                });
                return;
            };
            self.cursor.pending_directories.push_back(DirectoryResume {
                relative_directory: relative.to_path_buf(),
                after_entry: resume_after,
            });
            while cursor_estimated_bytes(&self.cursor)
                >= MAX_DISCOVERY_CURSOR_BYTES.saturating_sub(CURSOR_SAFETY_BYTES)
                && self.cursor.pending_relative_entries.len() > prior_entry_count
            {
                let _ = self.cursor.pending_relative_entries.pop_back();
                let replacement = if self.cursor.pending_relative_entries.len() > prior_entry_count
                {
                    self.cursor.pending_relative_entries.back().cloned()
                } else {
                    previous_after.clone()
                };
                if let (Some(resume), Some(replacement)) =
                    (self.cursor.pending_directories.back_mut(), replacement)
                {
                    resume.after_entry = replacement;
                }
            }
            if cursor_estimated_bytes(&self.cursor)
                >= MAX_DISCOVERY_CURSOR_BYTES.saturating_sub(CURSOR_SAFETY_BYTES)
            {
                let _ = self.cursor.pending_directories.pop_back();
                faults.push(DiscoveryFault {
                    class: DiscoveryFaultClass::CursorCapacity,
                });
            }
        }
    }
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn skipped_component(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(value) = component else {
            return false;
        };
        let value = value.to_string_lossy();
        ["backup", "backups", "cache", "caches", "tmp", "temp"]
            .iter()
            .any(|ignored| value.eq_ignore_ascii_case(ignored))
    })
}

fn cursor_is_valid(cursor: &DiscoveryCursor) -> bool {
    cursor_estimated_bytes(cursor) < MAX_DISCOVERY_CURSOR_BYTES
        && cursor
            .pending_relative_entries
            .iter()
            .all(|path| safe_relative(path) && !skipped_component(path))
        && cursor.pending_directories.iter().all(|resume| {
            (resume.relative_directory.as_os_str().is_empty()
                || safe_relative(&resume.relative_directory))
                && safe_relative(&resume.after_entry)
                && !skipped_component(&resume.after_entry)
        })
}

fn cursor_estimated_bytes(cursor: &DiscoveryCursor) -> usize {
    let entries = cursor
        .pending_relative_entries
        .iter()
        .try_fold(64_usize, |total, path| {
            total.checked_add(path.as_os_str().as_encoded_bytes().len() + 8)
        })
        .unwrap_or(usize::MAX);
    cursor
        .pending_directories
        .iter()
        .try_fold(entries, |total, resume| {
            total
                .checked_add(
                    resume
                        .relative_directory
                        .as_os_str()
                        .as_encoded_bytes()
                        .len()
                        + 8,
                )
                .and_then(|value| {
                    value.checked_add(resume.after_entry.as_os_str().as_encoded_bytes().len() + 8)
                })
        })
        .unwrap_or(usize::MAX)
}

fn stable_source_id(provider: Provider, root_identity: &str, file_identity: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox.source.fs-identity.v1");
    hash_part(&mut hasher, provider.as_str().as_bytes());
    hash_part(&mut hasher, root_identity.as_bytes());
    hash_part(&mut hasher, file_identity.as_bytes());
    format!("source_{}", &hasher.finalize().to_hex()[..32])
}

fn hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn metadata_time(metadata: &fs::Metadata) -> Option<OffsetDateTime> {
    let seconds = i128::from(metadata.mtime());
    let nanos = i128::from(metadata.mtime_nsec());
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanos))
        .and_then(|value| OffsetDateTime::from_unix_timestamp_nanos(value).ok())
}
