use std::{
    fmt,
    fs::File,
    path::{Component, Path, PathBuf},
};

use agbox_adapters::DiscoveredSource;
use rustix::fs::{FileType, Mode, OFlags};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSnapshot {
    pub source_id: String,
    pub file_identity: String,
    pub path: PathBuf,
    pub size: u64,
    pub generation: u64,
}

impl SourceSnapshot {
    #[must_use]
    pub fn new(
        source_id: String,
        file_identity: String,
        path: PathBuf,
        size: u64,
        generation: u64,
    ) -> Self {
        Self {
            source_id,
            file_identity,
            path,
            size,
            generation,
        }
    }

    #[cfg(feature = "test-support")]
    #[must_use]
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
    }
}

impl From<&DiscoveredSource> for SourceSnapshot {
    fn from(source: &DiscoveredSource) -> Self {
        Self::new(
            source.source_id.clone(),
            source.file_identity.clone(),
            source.path.clone(),
            source.size,
            source.generation,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceGeneration {
    pub source_id: String,
    pub generation: u64,
    pub moved: bool,
    pub replaced: bool,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GenerationError {
    #[error("source generation overflow")]
    Overflow,
    #[error("source generation must be nonzero")]
    ZeroGeneration,
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
    let generation = if replaced || truncated {
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
        let metadata = canonical_root
            .symlink_metadata()
            .map_err(|_| VerifiedOpenError::IdentityChanged)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(VerifiedOpenError::IdentityChanged);
        }
        let root = rustix::fs::open(
            &canonical_root,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| VerifiedOpenError::IdentityChanged)?;
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
        self.open_relative(relative, &source.file_identity)
    }

    /// Opens a relative source with no-follow checks at every component.
    ///
    /// # Errors
    ///
    /// Returns a path-independent identity error for malformed or changed input.
    pub fn open_relative(
        &self,
        relative: &Path,
        expected_identity: &str,
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
        }
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
        let stat = rustix::fs::fstat(&file).map_err(|_| VerifiedOpenError::IdentityChanged)?;
        let device = u64::try_from(stat.st_dev).map_err(|_| VerifiedOpenError::IdentityChanged)?;
        let inode = stat.st_ino;
        if !FileType::from_raw_mode(stat.st_mode).is_file()
            || unix_identity(device, inode) != expected_identity
        {
            return Err(VerifiedOpenError::IdentityChanged);
        }
        Ok(file)
    }
}

#[must_use]
pub(crate) fn unix_identity(device: u64, inode: u64) -> String {
    format!("unix:{device}:{inode}")
}
