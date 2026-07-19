//! Bounded structured logging primitives for the daemon.

use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub const MAX_LOG_ENTRY_BYTES: usize = 4 * 1024;
pub const LOG_QUEUE_CAPACITY: usize = 1_024;
pub const MAX_LOG_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub const RETAINED_LOG_FILES: u8 = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEvent {
    pub kind: &'static str,
    pub result: &'static str,
    pub byte_length: u32,
}

#[derive(Debug, Default)]
pub struct BoundedLogWriter {
    directory: Option<PathBuf>,
    entries: VecDeque<LogEvent>,
    dropped: u64,
}

impl BoundedLogWriter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            directory: None,
            entries: VecDeque::with_capacity(LOG_QUEUE_CAPACITY),
            dropped: 0,
        }
    }

    /// Opens an owner-only directory for the bounded typed log stream.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory is not owner-controlled.
    pub fn with_directory(directory: impl AsRef<Path>) -> std::io::Result<Self> {
        fs::create_dir_all(directory.as_ref())?;
        let metadata = fs::symlink_metadata(directory.as_ref())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "log directory is unsafe",
            ));
        }
        fs::set_permissions(directory.as_ref(), fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            directory: Some(directory.as_ref().to_path_buf()),
            ..Self::new()
        })
    }

    pub fn push(&mut self, event: LogEvent) {
        if event.byte_length as usize > MAX_LOG_ENTRY_BYTES
            || self.entries.len() == LOG_QUEUE_CAPACITY
        {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.entries.push_back(event);
    }

    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn drain(&mut self) -> impl Iterator<Item = LogEvent> + '_ {
        self.entries.drain(..)
    }

    /// Flushes queued typed events to owner-only rotating files.
    ///
    /// # Errors
    ///
    /// Returns an I/O error without discarding entries which were not written.
    pub fn flush(&mut self) -> std::io::Result<()> {
        let Some(directory) = self.directory.clone() else {
            return Ok(());
        };
        while let Some(event) = self.entries.front().cloned() {
            let encoded = serde_json::to_vec(&event_to_wire(&event))?;
            if encoded.len() > MAX_LOG_ENTRY_BYTES {
                self.entries.pop_front();
                self.dropped = self.dropped.saturating_add(1);
                continue;
            }
            let active = directory.join("agbox.log");
            let current_bytes = fs::metadata(&active).map_or(0, |metadata| metadata.len());
            let needed = u64::try_from(encoded.len().saturating_add(1)).unwrap_or(u64::MAX);
            if current_bytes.saturating_add(needed) > MAX_LOG_FILE_BYTES {
                rotate(&directory)?;
            }
            let mut file = OpenOptions::new().create(true).append(true).open(&active)?;
            fs::set_permissions(&active, fs::Permissions::from_mode(0o600))?;
            file.write_all(&encoded)?;
            file.write_all(b"\n")?;
            file.sync_data()?;
            self.entries.pop_front();
        }
        Ok(())
    }
}

fn event_to_wire(event: &LogEvent) -> serde_json::Value {
    serde_json::json!({
        "kind": event.kind,
        "result": event.result,
        "byte_length": event.byte_length,
    })
}

fn rotate(directory: &Path) -> std::io::Result<()> {
    let oldest = directory.join(format!("agbox.log.{RETAINED_LOG_FILES}"));
    match fs::remove_file(oldest) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for index in (1..RETAINED_LOG_FILES).rev() {
        let source = directory.join(format!("agbox.log.{index}"));
        let destination = directory.join(format!("agbox.log.{}", index + 1));
        if source.exists() {
            fs::rename(source, destination)?;
        }
    }
    let active = directory.join("agbox.log");
    if active.exists() {
        fs::rename(active, directory.join("agbox.log.1"))?;
    }
    Ok(())
}
