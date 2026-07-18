use std::{
    fmt, fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use agbox_core::ProjectId;

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
    allowed_root: PathBuf,
}

impl fmt::Debug for ProjectResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectResolver")
            .finish_non_exhaustive()
    }
}

impl ProjectResolver {
    /// Binds project lookup to a canonical, non-symlink directory root.
    ///
    /// # Errors
    ///
    /// Rejects unavailable, symlink, or non-directory roots.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let root = root.as_ref();
        let metadata = root
            .symlink_metadata()
            .map_err(|_| ProjectError::Unavailable)?;
        if metadata.file_type().is_symlink() {
            return Err(ProjectError::Symlink);
        }
        if !metadata.is_dir() {
            return Err(ProjectError::Unavailable);
        }
        let allowed_root = root.canonicalize().map_err(|_| ProjectError::Unavailable)?;
        Ok(Self { allowed_root })
    }

    /// Walks upward from a verified cwd to a real `.git` marker.
    ///
    /// # Errors
    ///
    /// Rejects path escape, every symlink component, missing repositories, and
    /// invalid filesystem identity.
    pub fn resolve(&self, cwd: impl AsRef<Path>) -> Result<ResolvedProject, ProjectError> {
        let cwd = cwd.as_ref();
        let canonical = cwd.canonicalize().map_err(|_| ProjectError::Unavailable)?;
        if !canonical.starts_with(&self.allowed_root) {
            return Err(ProjectError::RootEscape);
        }
        self.reject_symlink_components(&canonical)?;
        let metadata = canonical
            .metadata()
            .map_err(|_| ProjectError::Unavailable)?;
        let mut current = if metadata.is_dir() {
            canonical
        } else {
            canonical
                .parent()
                .ok_or(ProjectError::RootEscape)?
                .to_path_buf()
        };
        loop {
            let marker = current.join(".git");
            match marker.symlink_metadata() {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(ProjectError::Symlink);
                    }
                    if metadata.is_dir() || metadata.is_file() {
                        return resolved_project(current);
                    }
                    return Err(ProjectError::NotRepository);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(ProjectError::Unavailable),
            }
            if current == self.allowed_root {
                break;
            }
            if !current.pop() || !current.starts_with(&self.allowed_root) {
                return Err(ProjectError::RootEscape);
            }
        }
        Err(ProjectError::NotRepository)
    }

    fn reject_symlink_components(&self, canonical: &Path) -> Result<(), ProjectError> {
        if !canonical.starts_with(&self.allowed_root) {
            return Err(ProjectError::RootEscape);
        }
        let relative = canonical
            .strip_prefix(&self.allowed_root)
            .map_err(|_| ProjectError::RootEscape)?;
        let mut current = self.allowed_root.clone();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(ProjectError::RootEscape);
            };
            current.push(name);
            let metadata = current
                .symlink_metadata()
                .map_err(|_| ProjectError::Unavailable)?;
            if metadata.file_type().is_symlink() {
                return Err(ProjectError::Symlink);
            }
        }
        Ok(())
    }
}

fn resolved_project(root: PathBuf) -> Result<ResolvedProject, ProjectError> {
    let metadata = fs::metadata(&root).map_err(|_| ProjectError::Unavailable)?;
    let repository_identity = format!("repo-fs-v1:{}:{}", metadata.dev(), metadata.ino());
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
