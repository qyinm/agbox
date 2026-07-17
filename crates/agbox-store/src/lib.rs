mod crypto;
mod evidence;
mod fs_security;
mod migrate;

use std::{ffi::OsStr, fs::File, path::Path};

use rusqlite::Connection;

#[cfg(feature = "test-support")]
pub use crypto::MemoryKeyProvider;
pub use crypto::{CryptoError, KeyProvider, KeyringKeyProvider};
pub use evidence::{EvidenceContext, EvidenceError, EvidenceOwnerRef, EvidenceVault};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("filesystem security check failed")]
    Io(#[from] std::io::Error),
    #[error("database operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the legacy database name is reserved")]
    LegacyDatabaseReserved,
    #[error("unsupported database schema version {0}")]
    UnsupportedSchema(i64),
}

pub struct Store {
    writer: Connection,
    _directory: File,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Opens the clean-slate v2 state database.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is reserved, the database files are not
    /// owner-controlled, the schema version is unsupported, or `SQLite` cannot
    /// initialize the database.
    pub fn open_new(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if path.file_name() == Some(OsStr::new("agbox.db")) {
            return Err(StoreError::LegacyDatabaseReserved);
        }
        let opened = migrate::open_writer(path)?;
        Ok(Self {
            writer: opened.connection,
            _directory: opened.directory,
        })
    }

    /// Returns the `SQLite` user schema version.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot read the schema version.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        Ok(self
            .writer
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    /// Returns the active `SQLite` journal mode.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot read the journal mode.
    pub fn journal_mode(&self) -> Result<String, StoreError> {
        Ok(self
            .writer
            .pragma_query_value(None, "journal_mode", |row| row.get(0))?)
    }

    #[cfg(feature = "test-support")]
    /// Reports whether a table or virtual table exists.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot query its schema catalog.
    pub fn table_exists(&self, table: &str) -> Result<bool, StoreError> {
        Ok(self.writer.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_schema
                WHERE type IN ('table', 'view') AND name = ?1
            )",
            [table],
            |row| row.get(0),
        )?)
    }
}
