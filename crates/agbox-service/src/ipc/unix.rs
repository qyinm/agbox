//! Unix-domain socket confinement for the local IPC boundary.

use std::{
    ffi::OsString,
    fs::{self, File},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    time::Duration,
};

use interprocess::local_socket::{
    GenericFilePath, ListenerOptions, ToFsName,
    tokio::{Listener, Stream, prelude::*},
};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};

use super::IpcError;

#[derive(Debug)]
pub(crate) struct BoundUnixListener {
    listener: Listener,
    path: PathBuf,
    directory: File,
    name: OsString,
}

impl BoundUnixListener {
    pub(crate) async fn bind(path: &Path) -> Result<Self, IpcError> {
        let parent = path.parent().ok_or(IpcError::UnsafeSocketPath)?;
        ensure_runtime_directory(parent)?;
        let canonical_parent = parent
            .canonicalize()
            .map_err(|_| IpcError::UnsafeSocketPath)?;
        let socket_name = path
            .file_name()
            .map(OsString::from)
            .ok_or(IpcError::UnsafeSocketPath)?;
        let directory = rustix::fs::open(
            &canonical_parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| IpcError::UnsafeSocketPath)?;
        let canonical_path = canonical_parent.join(&socket_name);
        if fs::symlink_metadata(&canonical_path).is_ok() {
            if connect(&canonical_path).await.is_ok() {
                return Err(IpcError::AlreadyRunning);
            }
            remove_stale_socket(&directory, &socket_name)?;
        }
        let listener = create_listener(&canonical_path).await?;
        fs::set_permissions(&canonical_path, fs::Permissions::from_mode(0o600))
            .map_err(|_| IpcError::UnsafeSocketPath)?;
        Ok(Self {
            listener,
            path: canonical_path,
            directory,
            name: socket_name,
        })
    }

    pub(crate) async fn accept(&self) -> Result<Stream, IpcError> {
        self.listener
            .accept()
            .await
            .map_err(|_| IpcError::AcceptFailed)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn remove(&self) -> Result<(), IpcError> {
        match rustix::fs::statat(&self.directory, &self.name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                if !FileType::from_raw_mode(stat.st_mode).is_socket()
                    || stat.st_uid != rustix::process::geteuid().as_raw()
                    || (stat.st_mode & 0o077) != 0
                {
                    return Err(IpcError::UnsafeSocketPath);
                }
                rustix::fs::unlinkat(&self.directory, &self.name, AtFlags::empty())
                    .map_err(|_| IpcError::UnsafeSocketPath)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(IpcError::UnsafeSocketPath),
        }
    }
}

async fn create_listener(path: &Path) -> Result<Listener, IpcError> {
    const TRANSIENT_PERMISSION_RETRIES: u8 = 3;
    for attempt in 0..TRANSIENT_PERMISSION_RETRIES {
        let listener_name = path
            .as_os_str()
            .to_fs_name::<GenericFilePath>()
            .map_err(|_| IpcError::BindFailed)?;
        match ListenerOptions::new()
            .name(listener_name)
            .reclaim_name(false)
            .create_tokio()
        {
            Ok(listener) => return Ok(listener),
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    && attempt + 1 < TRANSIENT_PERMISSION_RETRIES =>
            {
                // macOS can briefly reject a new filesystem socket immediately
                // after another short-lived test/runtime socket is removed.
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                return Err(IpcError::AlreadyRunning);
            }
            Err(_) => return Err(IpcError::BindFailed),
        }
    }
    Err(IpcError::BindFailed)
}

pub(crate) async fn connect(path: &Path) -> Result<Stream, IpcError> {
    let name = path
        .as_os_str()
        .to_fs_name::<GenericFilePath>()
        .map_err(|_| IpcError::Transport)?;
    Stream::connect(name).await.map_err(|_| IpcError::Transport)
}

#[allow(clippy::verbose_bit_mask)] // Permission classes are security-relevant here.
fn ensure_runtime_directory(path: &Path) -> Result<(), IpcError> {
    fs::create_dir_all(path).map_err(|_| IpcError::UnsafeSocketPath)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| IpcError::UnsafeSocketPath)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(IpcError::UnsafeSocketPath);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| IpcError::UnsafeSocketPath)?;
    let secured = fs::symlink_metadata(path).map_err(|_| IpcError::UnsafeSocketPath)?;
    (!secured.file_type().is_symlink()
        && secured.is_dir()
        && secured.uid() == rustix::process::geteuid().as_raw()
        && (secured.mode() & 0o077) == 0)
        .then_some(())
        .ok_or(IpcError::UnsafeSocketPath)
}

fn remove_stale_socket(directory: &File, name: &OsString) -> Result<(), IpcError> {
    let stat = rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| IpcError::UnsafeSocketPath)?;
    let kind = FileType::from_raw_mode(stat.st_mode);
    if !kind.is_socket()
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || (stat.st_mode & 0o022) != 0
    {
        return Err(IpcError::UnsafeSocketPath);
    }
    rustix::fs::unlinkat(directory, name, AtFlags::empty()).map_err(|_| IpcError::UnsafeSocketPath)
}

#[allow(dead_code)]
fn _directory_fd(directory: &File) -> i32 {
    directory.as_raw_fd()
}
