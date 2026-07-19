use std::{
    fmt,
    fs::File,
    path::{Component, Path, PathBuf},
};

use agbox_adapters::DiscoveredSource;
use rustix::fs::{FileType, Mode, OFlags};
use time::OffsetDateTime;

#[derive(Clone, Eq, PartialEq)]
pub struct SourceSnapshot {
    pub source_id: String,
    pub file_identity: String,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: OffsetDateTime,
    pub ctime: OffsetDateTime,
    pub generation: u64,
}

impl SourceSnapshot {
    /// Creates a validated metadata snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationError::InvalidIdentity`] for non-canonical source
    /// or filesystem identity wire values.
    pub fn new(
        source_id: String,
        file_identity: String,
        path: PathBuf,
        size: u64,
        generation: u64,
    ) -> Result<Self, GenerationError> {
        if !valid_source_id(&source_id) || !valid_file_identity(&file_identity) {
            return Err(GenerationError::InvalidIdentity);
        }
        Ok(Self {
            source_id,
            file_identity,
            path,
            size,
            mtime: OffsetDateTime::UNIX_EPOCH,
            ctime: OffsetDateTime::UNIX_EPOCH,
            generation,
        })
    }

    #[must_use]
    pub fn with_mtime(mut self, mtime: OffsetDateTime) -> Self {
        self.mtime = mtime;
        self
    }

    #[must_use]
    pub fn with_ctime(mut self, ctime: OffsetDateTime) -> Self {
        self.ctime = ctime;
        self
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    #[allow(clippy::expect_used, clippy::missing_panics_doc)]
    pub fn fixture(
        source_id: &str,
        file_identity: &str,
        path: &str,
        size: u64,
        generation: u64,
    ) -> Self {
        Self::new(
            source_id.to_owned(),
            file_identity.to_owned(),
            PathBuf::from(path),
            size,
            generation,
        )
        .expect("test fixture identities are valid")
    }
}

impl TryFrom<&DiscoveredSource> for SourceSnapshot {
    type Error = GenerationError;

    fn try_from(source: &DiscoveredSource) -> Result<Self, Self::Error> {
        Ok(Self::new(
            source.source_id.clone(),
            source.file_identity.clone(),
            source.path.clone(),
            source.size,
            source.generation,
        )?
        .with_mtime(source.mtime)
        .with_ctime(source.ctime))
    }
}

impl fmt::Debug for SourceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceSnapshot")
            .field("source_id_bytes", &self.source_id.len())
            .field("file_identity_bytes", &self.file_identity.len())
            .field("size", &self.size)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // Public reconciliation facts are independently queryable.
pub struct SourceGeneration {
    pub source_id: String,
    pub generation: u64,
    pub moved: bool,
    pub replaced: bool,
    pub truncated: bool,
    pub modified: bool,
}

impl fmt::Debug for SourceGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceGeneration")
            .field("source_id_bytes", &self.source_id.len())
            .field("generation", &self.generation)
            .field("moved", &self.moved)
            .field("replaced", &self.replaced)
            .field("truncated", &self.truncated)
            .field("modified", &self.modified)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GenerationError {
    #[error("source generation overflow")]
    Overflow,
    #[error("source generation must be nonzero")]
    ZeroGeneration,
    #[error("source identity is invalid")]
    InvalidIdentity,
}

/// Reconciles a metadata snapshot without mutating either immutable observation.
///
/// # Errors
///
/// Returns [`GenerationError`] rather than wrapping a generation counter.
pub fn reconcile_generation(
    previous: &SourceSnapshot,
    observed: &SourceSnapshot,
) -> Result<SourceGeneration, GenerationError> {
    if previous.generation == 0 {
        return Err(GenerationError::ZeroGeneration);
    }
    let same_file = previous.file_identity == observed.file_identity;
    let same_path = previous.path == observed.path;
    let replaced = same_path && !same_file;
    let truncated = same_file && observed.size < previous.size;
    let modified = same_file
        && observed.size == previous.size
        && (observed.mtime != previous.mtime || observed.ctime != previous.ctime);
    let generation = if replaced || truncated || modified {
        previous
            .generation
            .checked_add(1)
            .ok_or(GenerationError::Overflow)?
    } else {
        previous.generation
    };
    Ok(SourceGeneration {
        source_id: previous.source_id.clone(),
        generation,
        moved: same_file && !same_path,
        replaced,
        truncated,
        modified,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VerifiedOpenError {
    #[error("source identity changed")]
    IdentityChanged,
}

pub struct VerifiedSourceOpener {
    canonical_root: PathBuf,
    root: File,
}

impl fmt::Debug for VerifiedSourceOpener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedSourceOpener")
            .finish_non_exhaustive()
    }
}

impl VerifiedSourceOpener {
    /// Binds a no-follow directory descriptor to one canonical discovery root.
    ///
    /// # Errors
    ///
    /// Fails closed when the root cannot be canonicalized or securely opened.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, VerifiedOpenError> {
        let canonical_root = root
            .as_ref()
            .canonicalize()
            .map_err(|_| VerifiedOpenError::IdentityChanged)?;
        let root = open_absolute_directory(&canonical_root)?;
        if !FileType::from_raw_mode(
            rustix::fs::fstat(&root)
                .map_err(|_| VerifiedOpenError::IdentityChanged)?
                .st_mode,
        )
        .is_dir()
        {
            return Err(VerifiedOpenError::IdentityChanged);
        }
        if !same_identity(&root, &open_absolute_directory(&canonical_root)?)? {
            return Err(VerifiedOpenError::IdentityChanged);
        }
        Ok(Self {
            canonical_root,
            root,
        })
    }

    /// Opens a discovered source relative to the descriptor-bound root.
    ///
    /// # Errors
    ///
    /// Returns [`VerifiedOpenError::IdentityChanged`] for every path,
    /// metadata, symlink, non-regular, replacement, or identity race.
    pub fn open(&self, source: &DiscoveredSource) -> Result<File, VerifiedOpenError> {
        if source.root != self.canonical_root {
            return Err(VerifiedOpenError::IdentityChanged);
        }
        let relative = source
            .path
            .strip_prefix(&self.canonical_root)
            .map_err(|_| VerifiedOpenError::IdentityChanged)?;
        self.open_expected(
            relative,
            &source.file_identity,
            Some((source.size, source.mtime, source.ctime)),
        )
    }

    /// Opens a relative source with no-follow checks at every component.
    ///
    /// # Errors
    ///
    /// Returns a path-independent identity error for malformed or changed input.
    #[cfg(feature = "test-support")]
    pub fn open_relative(
        &self,
        relative: &Path,
        expected_identity: &str,
    ) -> Result<File, VerifiedOpenError> {
        self.open_expected(relative, expected_identity, None)
    }

    fn open_expected(
        &self,
        relative: &Path,
        expected_identity: &str,
        expected_snapshot: Option<(u64, OffsetDateTime, OffsetDateTime)>,
    ) -> Result<File, VerifiedOpenError> {
        let components = relative.components().collect::<Vec<_>>();
        if components.is_empty()
            || components
                .iter()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(VerifiedOpenError::IdentityChanged);
        }
        let mut directory = self
            .root
            .try_clone()
            .map_err(|_| VerifiedOpenError::IdentityChanged)?;
        let mut chain = vec![
            directory
                .try_clone()
                .map_err(|_| VerifiedOpenError::IdentityChanged)?,
        ];
        let mut names = Vec::new();
        for component in &components[..components.len() - 1] {
            let Component::Normal(name) = component else {
                return Err(VerifiedOpenError::IdentityChanged);
            };
            directory = rustix::fs::openat(
                &directory,
                *name,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| VerifiedOpenError::IdentityChanged)?;
            names.push(name.to_os_string());
            chain.push(
                directory
                    .try_clone()
                    .map_err(|_| VerifiedOpenError::IdentityChanged)?,
            );
        }
        self.verify_chain(&chain, &names)?;
        let Component::Normal(file_name) = components[components.len() - 1] else {
            return Err(VerifiedOpenError::IdentityChanged);
        };
        let file = rustix::fs::openat(
            &directory,
            file_name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| VerifiedOpenError::IdentityChanged)?;
        self.verify_chain(&chain, &names)?;
        let stat = rustix::fs::fstat(&file).map_err(|_| VerifiedOpenError::IdentityChanged)?;
        let device = u64::try_from(stat.st_dev).map_err(|_| VerifiedOpenError::IdentityChanged)?;
        let inode = stat.st_ino;
        let observed_size =
            u64::try_from(stat.st_size).map_err(|_| VerifiedOpenError::IdentityChanged)?;
        let observed_modified_at = stat_time(&stat).ok_or(VerifiedOpenError::IdentityChanged)?;
        let observed_changed_at = stat_ctime(&stat).ok_or(VerifiedOpenError::IdentityChanged)?;
        if !FileType::from_raw_mode(stat.st_mode).is_file()
            || unix_identity(device, inode) != expected_identity
            || expected_snapshot.is_some_and(|(size, mtime, ctime)| {
                size != observed_size
                    || mtime != observed_modified_at
                    || ctime != observed_changed_at
            })
        {
            return Err(VerifiedOpenError::IdentityChanged);
        }
        Ok(file)
    }

    fn verify_chain(
        &self,
        chain: &[File],
        names: &[std::ffi::OsString],
    ) -> Result<(), VerifiedOpenError> {
        let rebound_root = open_absolute_directory(&self.canonical_root)?;
        if !same_identity(&self.root, &rebound_root)? || !same_identity(&chain[0], &self.root)? {
            return Err(VerifiedOpenError::IdentityChanged);
        }
        for (index, pair) in chain.windows(2).enumerate() {
            let parent = &pair[0];
            let child = &pair[1];
            let observed_parent = rustix::fs::openat(
                child,
                "..",
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| VerifiedOpenError::IdentityChanged)?;
            if !same_identity(parent, &observed_parent)? {
                return Err(VerifiedOpenError::IdentityChanged);
            }
            let rebound_child = rustix::fs::openat(
                parent,
                &names[index],
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| VerifiedOpenError::IdentityChanged)?;
            if !same_identity(child, &rebound_child)? {
                return Err(VerifiedOpenError::IdentityChanged);
            }
        }
        Ok(())
    }
}

fn open_absolute_directory(path: &Path) -> Result<File, VerifiedOpenError> {
    if !path.is_absolute() {
        return Err(VerifiedOpenError::IdentityChanged);
    }
    let mut directory = rustix::fs::open(
        "/",
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| VerifiedOpenError::IdentityChanged)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        directory = rustix::fs::openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| VerifiedOpenError::IdentityChanged)?;
    }
    Ok(directory)
}

fn same_identity(left: &File, right: &File) -> Result<bool, VerifiedOpenError> {
    let left = rustix::fs::fstat(left).map_err(|_| VerifiedOpenError::IdentityChanged)?;
    let right = rustix::fs::fstat(right).map_err(|_| VerifiedOpenError::IdentityChanged)?;
    Ok(left.st_dev == right.st_dev && left.st_ino == right.st_ino)
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

fn valid_source_id(value: &str) -> bool {
    value.len() == 39
        && value.strip_prefix("source_").is_some_and(|digest| {
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
}

fn valid_file_identity(value: &str) -> bool {
    value
        .strip_prefix("unix:")
        .and_then(|suffix| suffix.split_once(':'))
        .is_some_and(|(device, inode)| canonical_u64(device) && canonical_u64(inode))
}

fn canonical_u64(value: &str) -> bool {
    !value.is_empty()
        && (value == "0" || !value.starts_with('0'))
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}

#[must_use]
pub(crate) fn unix_identity(device: u64, inode: u64) -> String {
    format!("unix:{device}:{inode}")
}
