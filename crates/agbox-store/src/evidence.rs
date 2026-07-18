use std::{
    ffi::OsString,
    fs::File,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use agbox_core::{EventId, EvidenceId, ProjectId, WorkId, limits::MAX_INLINE_BYTES};
use zeroize::Zeroizing;

use crate::{
    crypto::{CryptoError, KeyProvider, open, seal},
    fs_security::{
        create_owner_temp_file, ensure_owner_directory, link_owner_file,
        open_bound_owner_directory, read_owner_file_nofollow, remove_owner_file,
        set_owner_file_mode, validate_owner_file,
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
    root_directory: File,
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
    /// credential store cannot provide the master key. Owner-only mode treats
    /// the current account as trusted to mutate its vault; other OS users are
    /// excluded by ownership, exact permissions, and no-follow checks.
    #[allow(clippy::needless_pass_by_value)]
    pub fn open(root: PathBuf, keys: Arc<dyn KeyProvider>) -> Result<Self, EvidenceError> {
        ensure_owner_directory(&root)?;
        let key = keys.master_key()?;
        let (root, root_directory) = open_bound_owner_directory(&root)?;
        Ok(Self {
            root,
            root_directory,
            key,
        })
    }

    pub(crate) fn seal_database_field(
        &self,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, EvidenceError> {
        const MAX_DATABASE_FIELD_BYTES: usize = 32 * 1024;
        if plaintext.len() > MAX_DATABASE_FIELD_BYTES || aad.len() > MAX_DATABASE_FIELD_BYTES {
            return Err(EvidenceError::TooLarge);
        }
        seal(&self.key, aad, plaintext).map_err(EvidenceError::from)
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
        let destination = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "evidence target has no file name",
            )
        })?;
        match validate_owner_file(&self.root_directory, destination) {
            Ok(()) => {
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
        let temporary = OsString::from(format!(
            ".{}.{}.tmp",
            id.as_str(),
            uuid::Uuid::new_v4().simple()
        ));
        let mut file = create_owner_temp_file(&self.root_directory, &temporary)?;
        let mut cleanup = TempCleanup::new(&self.root_directory, temporary.clone());
        set_owner_file_mode(&file)?;
        file.write_all(&envelope)?;
        file.sync_all()?;
        match link_owner_file(&self.root_directory, &temporary, destination, &file) {
            Ok(()) => {
                cleanup.remove_now()?;
                self.root_directory.sync_all()?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                cleanup.remove_now()?;
                validate_owner_file(&self.root_directory, destination)?;
                if self.get(id, context)?.as_slice() == plaintext {
                    Ok(())
                } else {
                    Err(EvidenceError::Conflict)
                }
            }
            Err(error) => Err(error.into()),
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
        let name = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "evidence target has no file name",
            )
        })?;
        let envelope = read_owner_file_nofollow(&self.root_directory, name, MAX_INLINE_BYTES + 64)?;
        Ok(Zeroizing::new(open(
            &self.key,
            &Self::aad(id, context),
            &envelope,
        )?))
    }

    /// Removes a vault object after the database transaction has recorded a
    /// delete-pending queue entry. It uses the same descriptor-relative,
    /// no-follow confinement as reads and writes.
    pub(crate) fn remove(&self, id: &EvidenceId) -> Result<(), EvidenceError> {
        let path = self.path(id)?;
        let name = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "evidence target has no file name",
            )
        })?;
        match remove_owner_file(&self.root_directory, name) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Lists at most 256 old, regular owner files with safe evidence IDs. The
    /// writer must still verify that no metadata row exists immediately before
    /// removing each candidate.
    pub(crate) fn orphan_candidates(
        &self,
        now: SystemTime,
    ) -> Result<Vec<EvidenceId>, EvidenceError> {
        ensure_owner_directory(&self.root)?;
        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(&self.root)?.take(256) {
            let entry = entry?;
            let name = entry.file_name();
            let text = name.to_string_lossy();
            let Some(raw) = text.strip_suffix(".agbx") else {
                continue;
            };
            let Some(id) = EvidenceId::parse_wire(raw) else {
                continue;
            };
            validate_owner_file(&self.root_directory, &name)?;
            let age = now
                .duration_since(entry.metadata()?.modified()?)
                .unwrap_or(Duration::ZERO);
            if age >= Duration::from_hours(24) {
                candidates.push(id);
            }
        }
        Ok(candidates)
    }
}

struct TempCleanup<'a> {
    directory: &'a File,
    name: OsString,
    active: bool,
}

impl<'a> TempCleanup<'a> {
    fn new(directory: &'a File, name: OsString) -> Self {
        Self {
            directory,
            name,
            active: true,
        }
    }

    fn remove_now(&mut self) -> std::io::Result<()> {
        if !self.active {
            return Ok(());
        }
        match remove_owner_file(self.directory, &self.name) {
            Ok(()) => {
                self.active = false;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.active = false;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for TempCleanup<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = remove_owner_file(self.directory, &self.name);
        }
    }
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'/' && byte != b'\\')
}

#[cfg(test)]
mod database_field_tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use super::*;
    use crate::{MemoryKeyProvider, crypto::open};

    #[test]
    fn database_field_envelopes_are_bound_to_length_delimited_aad() {
        let temp = tempfile::tempdir().unwrap();
        let vault = EvidenceVault::open(
            temp.path().join("evidence"),
            Arc::new(MemoryKeyProvider::fixed([91_u8; 32])),
        )
        .unwrap();
        let aad_project_a = b"\x07project\x01a";
        let aad_project_b = b"\x07project\x01b";
        let envelope = vault
            .seal_database_field(aad_project_a, b"/private/source")
            .unwrap();
        assert_eq!(
            open(&[91_u8; 32], aad_project_a, &envelope).unwrap(),
            b"/private/source"
        );
        assert!(open(&[91_u8; 32], aad_project_b, &envelope).is_err());
        assert!(open(&[91_u8; 32], b"source-path-domain", &envelope).is_err());
    }
}
