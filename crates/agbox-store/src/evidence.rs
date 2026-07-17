use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Arc,
};

use agbox_core::{EventId, EvidenceId, ProjectId, WorkId, limits::MAX_INLINE_BYTES};
use zeroize::Zeroizing;

use crate::{
    crypto::{CryptoError, KeyProvider, open, seal},
    fs_security::{
        ensure_owner_directory, read_owner_file_nofollow, set_owner_file_mode, validate_owner_file,
    },
};

#[derive(Clone, Copy, Debug)]
pub struct EvidenceContext<'a> {
    pub project_id: &'a ProjectId,
    pub owner: EvidenceOwnerRef<'a>,
}

#[derive(Clone, Copy, Debug)]
pub enum EvidenceOwnerRef<'a> {
    Event(&'a EventId),
    Work(&'a WorkId),
}

impl<'a> EvidenceOwnerRef<'a> {
    fn aad_parts(self) -> (&'static str, &'a str) {
        match self {
            Self::Event(id) => ("event", id.as_str()),
            Self::Work(id) => ("work", id.as_str()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("immutable evidence ID already contains different content")]
    Conflict,
    #[error("evidence exceeds the inline-content bound")]
    TooLarge,
}

pub struct EvidenceVault {
    root: PathBuf,
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for EvidenceVault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EvidenceVault")
            .finish_non_exhaustive()
    }
}

impl EvidenceVault {
    /// Opens an owner-controlled evidence directory and loads its key once.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceError`] when the root is not owner-controlled or the
    /// credential store cannot provide the master key.
    #[allow(clippy::needless_pass_by_value)]
    pub fn open(root: PathBuf, keys: Arc<dyn KeyProvider>) -> Result<Self, EvidenceError> {
        ensure_owner_directory(&root)?;
        let key = keys.master_key()?;
        let root = root.canonicalize()?;
        ensure_owner_directory(&root)?;
        Ok(Self { root, key })
    }

    fn aad(id: &EvidenceId, context: EvidenceContext<'_>) -> Vec<u8> {
        let (owner_kind, owner_id) = context.owner.aad_parts();
        format!(
            "{}\0{}\0{}\0{}",
            id.as_str(),
            context.project_id.as_str(),
            owner_kind,
            owner_id
        )
        .into_bytes()
    }

    fn path(&self, id: &EvidenceId) -> Result<PathBuf, EvidenceError> {
        if !safe_component(id.as_str()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "evidence ID is not a safe path component",
            )
            .into());
        }
        ensure_owner_directory(&self.root)?;
        let path = self.root.join(format!("{}.agbx", id.as_str()));
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "evidence target has no parent",
            )
        })?;
        if parent.canonicalize()? != self.root {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "evidence target is outside its owner root",
            )
            .into());
        }
        Ok(path)
    }

    /// Stores immutable encrypted evidence under `id`.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceError::Conflict`] when an existing immutable record
    /// contains different plaintext, or another variant for invalid input,
    /// filesystem, and encryption failures.
    pub fn put(
        &self,
        id: &EvidenceId,
        context: EvidenceContext<'_>,
        plaintext: &[u8],
    ) -> Result<(), EvidenceError> {
        if plaintext.len() > MAX_INLINE_BYTES {
            return Err(EvidenceError::TooLarge);
        }
        let path = self.path(id)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                validate_owner_file(&path)?;
                return if self.get(id, context)?.as_slice() == plaintext {
                    Ok(())
                } else {
                    Err(EvidenceError::Conflict)
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let envelope = seal(&self.key, &Self::aad(id, context), plaintext)?;
        let temporary = self.root.join(format!(
            ".{}.{}.tmp",
            id.as_str(),
            uuid::Uuid::new_v4().simple()
        ));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        set_owner_file_mode(&file)?;
        file.write_all(&envelope)?;
        file.sync_all()?;
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {
                fs::remove_file(&temporary)?;
                fs::File::open(&self.root)?.sync_all()?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary)?;
                validate_owner_file(&path)?;
                if self.get(id, context)?.as_slice() == plaintext {
                    Ok(())
                } else {
                    Err(EvidenceError::Conflict)
                }
            }
            Err(error) => {
                let _cleanup = fs::remove_file(&temporary);
                Err(error.into())
            }
        }
    }

    /// Reads and authenticates encrypted evidence under `id`.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceError`] when the file is not owner-controlled or its
    /// authenticated context does not match.
    pub fn get(
        &self,
        id: &EvidenceId,
        context: EvidenceContext<'_>,
    ) -> Result<Zeroizing<Vec<u8>>, EvidenceError> {
        let path = self.path(id)?;
        let envelope = read_owner_file_nofollow(&path, MAX_INLINE_BYTES + 64)?;
        Ok(Zeroizing::new(open(
            &self.key,
            &Self::aad(id, context),
            &envelope,
        )?))
    }
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'/' && byte != b'\\')
}
