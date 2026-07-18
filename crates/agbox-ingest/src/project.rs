use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io::Read,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
};

use agbox_core::ProjectId;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};

const MAX_GITDIR_MARKER_BYTES: u64 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProjectError {
    #[error("project path is unavailable")]
    Unavailable,
    #[error("project path escapes its allowed root")]
    RootEscape,
    #[error("project path contains a symlink")]
    Symlink,
    #[error("no repository marker was found")]
    NotRepository,
    #[error("project identity is invalid")]
    InvalidIdentity,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ResolvedProject {
    pub project_id: ProjectId,
    pub repository_identity: String,
    pub root: PathBuf,
}

impl fmt::Debug for ResolvedProject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedProject")
            .field("project_id", &self.project_id)
            .field("repository_identity", &self.repository_identity)
            .finish_non_exhaustive()
    }
}

pub struct ProjectResolver {
    supplied_root: PathBuf,
    canonical_root: PathBuf,
    root: File,
    root_device: u64,
    root_inode: u64,
}

impl fmt::Debug for ProjectResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectResolver")
            .finish_non_exhaustive()
    }
}

impl ProjectResolver {
    /// Binds project lookup to an open, canonical, non-symlink directory.
    ///
    /// # Errors
    ///
    /// Rejects unavailable, symlink, or non-directory roots.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let supplied_root = root.as_ref().to_path_buf();
        let metadata = supplied_root
            .symlink_metadata()
            .map_err(|_| ProjectError::Unavailable)?;
        if metadata.file_type().is_symlink() {
            return Err(ProjectError::Symlink);
        }
        if !metadata.is_dir() {
            return Err(ProjectError::Unavailable);
        }
        let canonical_root = supplied_root
            .canonicalize()
            .map_err(|_| ProjectError::Unavailable)?;
        let root = rustix::fs::open(
            &canonical_root,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| ProjectError::Unavailable)?;
        let stat = rustix::fs::fstat(&root).map_err(|_| ProjectError::Unavailable)?;
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            return Err(ProjectError::Unavailable);
        }
        let (root_device, root_inode) = stat_identity(&stat)?;
        Ok(Self {
            supplied_root,
            canonical_root,
            root,
            root_device,
            root_inode,
        })
    }

    /// Walks upward from a descriptor-resolved cwd to a validated `.git`
    /// directory or a bounded, contained `gitdir:` marker.
    ///
    /// # Errors
    ///
    /// Rejects path escape, every post-bind symlink, malformed markers,
    /// missing repositories, changed roots, and invalid filesystem identity.
    pub fn resolve(&self, cwd: impl AsRef<Path>) -> Result<ResolvedProject, ProjectError> {
        self.verify_root()?;
        let components = self.relative_components(cwd.as_ref())?;
        let cwd_directory = self.open_components(&components)?;
        drop(cwd_directory);

        for length in (0..=components.len()).rev() {
            let repository_components = &components[..length];
            let repository = self.open_components(repository_components)?;
            match self.validate_git_marker(&repository, repository_components)? {
                MarkerResult::Valid => {
                    let stat =
                        rustix::fs::fstat(&repository).map_err(|_| ProjectError::Unavailable)?;
                    let (device, inode) = stat_identity(&stat)?;
                    return resolved_project(
                        self.path_for_components(repository_components),
                        device,
                        inode,
                    );
                }
                MarkerResult::Missing => {}
            }
        }
        Err(ProjectError::NotRepository)
    }

    fn verify_root(&self) -> Result<(), ProjectError> {
        let stat = rustix::fs::fstat(&self.root).map_err(|_| ProjectError::Unavailable)?;
        let (device, inode) = stat_identity(&stat)?;
        if !FileType::from_raw_mode(stat.st_mode).is_dir()
            || device != self.root_device
            || inode != self.root_inode
        {
            return Err(ProjectError::Unavailable);
        }
        Ok(())
    }

    fn relative_components(&self, cwd: &Path) -> Result<Vec<Vec<u8>>, ProjectError> {
        let relative = cwd
            .strip_prefix(&self.supplied_root)
            .or_else(|_| cwd.strip_prefix(&self.canonical_root))
            .map_err(|_| ProjectError::RootEscape)?;
        normal_components(relative)
    }

    fn open_components(&self, components: &[Vec<u8>]) -> Result<File, ProjectError> {
        let mut directory = self
            .root
            .try_clone()
            .map_err(|_| ProjectError::Unavailable)?;
        for component in components {
            directory = rustix::fs::openat(
                &directory,
                OsStr::from_bytes(component),
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| ProjectError::Symlink)?;
        }
        Ok(directory)
    }

    fn validate_git_marker(
        &self,
        repository: &File,
        repository_components: &[Vec<u8>],
    ) -> Result<MarkerResult, ProjectError> {
        let stat = match rustix::fs::statat(repository, ".git", AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(MarkerResult::Missing),
            Err(_) => return Err(ProjectError::Unavailable),
        };
        let file_type = FileType::from_raw_mode(stat.st_mode);
        if file_type.is_dir() {
            let marker = rustix::fs::openat(
                repository,
                ".git",
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| ProjectError::Symlink)?;
            let opened = rustix::fs::fstat(&marker).map_err(|_| ProjectError::Unavailable)?;
            if stat_identity(&opened)? != stat_identity(&stat)? {
                return Err(ProjectError::Unavailable);
            }
            return Ok(MarkerResult::Valid);
        }
        if !file_type.is_file()
            || stat.st_size < 0
            || u64::try_from(stat.st_size).map_or(true, |size| size > MAX_GITDIR_MARKER_BYTES)
        {
            return Err(ProjectError::NotRepository);
        }
        let marker = rustix::fs::openat(
            repository,
            ".git",
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| ProjectError::Symlink)?;
        let opened = rustix::fs::fstat(&marker).map_err(|_| ProjectError::Unavailable)?;
        if !FileType::from_raw_mode(opened.st_mode).is_file()
            || stat_identity(&opened)? != stat_identity(&stat)?
            || opened.st_size != stat.st_size
        {
            return Err(ProjectError::Unavailable);
        }
        let capacity = usize::try_from(opened.st_size).map_err(|_| ProjectError::NotRepository)?;
        let mut contents = Vec::with_capacity(capacity);
        (&marker)
            .take(MAX_GITDIR_MARKER_BYTES + 1)
            .read_to_end(&mut contents)
            .map_err(|_| ProjectError::Unavailable)?;
        let after_read = rustix::fs::fstat(&marker).map_err(|_| ProjectError::Unavailable)?;
        if contents.len() != capacity
            || stat_identity(&after_read)? != stat_identity(&opened)?
            || after_read.st_size != opened.st_size
            || after_read.st_mtime != opened.st_mtime
            || after_read.st_mtime_nsec != opened.st_mtime_nsec
            || after_read.st_ctime != opened.st_ctime
            || after_read.st_ctime_nsec != opened.st_ctime_nsec
        {
            return Err(ProjectError::Unavailable);
        }
        let target = parse_gitdir_marker(&contents)?;
        let target_components =
            self.resolve_gitdir_target(repository_components, OsStr::from_bytes(target))?;
        let target = self.open_components(&target_components)?;
        let target_stat = rustix::fs::fstat(&target).map_err(|_| ProjectError::Unavailable)?;
        if !FileType::from_raw_mode(target_stat.st_mode).is_dir() {
            return Err(ProjectError::NotRepository);
        }
        Ok(MarkerResult::Valid)
    }

    fn resolve_gitdir_target(
        &self,
        repository_components: &[Vec<u8>],
        target: &OsStr,
    ) -> Result<Vec<Vec<u8>>, ProjectError> {
        let target_path = Path::new(target);
        let (mut result, relative) = if target_path.is_absolute() {
            let relative = target_path
                .strip_prefix(&self.canonical_root)
                .or_else(|_| target_path.strip_prefix(&self.supplied_root))
                .map_err(|_| ProjectError::RootEscape)?;
            (Vec::new(), relative)
        } else {
            (repository_components.to_vec(), target_path)
        };
        for component in relative.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    result.pop().ok_or(ProjectError::RootEscape)?;
                }
                Component::Normal(name) if valid_component(name.as_bytes()) => {
                    result.push(name.as_bytes().to_vec());
                }
                _ => return Err(ProjectError::RootEscape),
            }
        }
        if result.is_empty() && target_path.as_os_str().is_empty() {
            return Err(ProjectError::NotRepository);
        }
        Ok(result)
    }

    fn path_for_components(&self, components: &[Vec<u8>]) -> PathBuf {
        components
            .iter()
            .fold(self.canonical_root.clone(), |mut path, component| {
                path.push(OsString::from_vec(component.clone()));
                path
            })
    }
}

#[derive(Clone, Copy)]
enum MarkerResult {
    Missing,
    Valid,
}

fn normal_components(path: &Path) -> Result<Vec<Vec<u8>>, ProjectError> {
    path.components()
        .map(|component| match component {
            Component::Normal(name) if valid_component(name.as_bytes()) => {
                Ok(name.as_bytes().to_vec())
            }
            _ => Err(ProjectError::RootEscape),
        })
        .collect()
}

fn valid_component(component: &[u8]) -> bool {
    !component.is_empty()
        && component != b"."
        && component != b".."
        && !component.contains(&0)
        && !component.contains(&b'/')
}

fn parse_gitdir_marker(contents: &[u8]) -> Result<&[u8], ProjectError> {
    let line = contents.strip_suffix(b"\n").unwrap_or(contents);
    if line.contains(&b'\n') {
        return Err(ProjectError::NotRepository);
    }
    line.strip_prefix(b"gitdir: ")
        .filter(|target| !target.is_empty())
        .ok_or(ProjectError::NotRepository)
}

fn stat_identity(stat: &rustix::fs::Stat) -> Result<(u64, u64), ProjectError> {
    let device = u64::try_from(stat.st_dev).map_err(|_| ProjectError::InvalidIdentity)?;
    Ok((device, stat.st_ino))
}

fn resolved_project(
    root: PathBuf,
    device: u64,
    inode: u64,
) -> Result<ResolvedProject, ProjectError> {
    let repository_identity = format!("repo-fs-v1:{device}:{inode}");
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox.project.fs-identity.v1");
    hasher.update(
        &u64::try_from(repository_identity.len())
            .map_err(|_| ProjectError::InvalidIdentity)?
            .to_le_bytes(),
    );
    hasher.update(repository_identity.as_bytes());
    let value = format!("project_{}", &hasher.finalize().to_hex()[..32]);
    let project_id = ProjectId::parse_wire(&value).ok_or(ProjectError::InvalidIdentity)?;
    Ok(ResolvedProject {
        project_id,
        repository_identity,
        root,
    })
}
