//! Process-only measurements and candidate bindings for release-gate runs.

use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use sha2::{Digest, Sha256};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Samples only this process. It intentionally never enumerates user process
/// names, command lines, or environment variables.
#[derive(Debug)]
pub struct ProcessSampler {
    system: System,
    pid: Pid,
}

impl ProcessSampler {
    #[must_use]
    pub fn current() -> Self {
        Self {
            system: System::new(),
            pid: Pid::from_u32(std::process::id()),
        }
    }

    #[must_use]
    pub fn resident_bytes(&mut self) -> u64 {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        self.system
            .process(self.pid)
            .map_or(0, sysinfo::Process::memory)
    }
}

/// Returns the lowercase SHA-256 of a regular candidate binary.
///
/// # Errors
///
/// Returns an I/O error when the path is unsafe, unavailable, or unreadable.
pub fn binary_sha256(path: &Path) -> Result<String, io::Error> {
    let metadata = path.symlink_metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsafe binary"));
    }
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
