#![allow(clippy::missing_errors_doc)]

//! macOS owner-private filesystem and `LaunchAgent` implementation.

use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use plist::Value;

use crate::{
    paths::AgboxPaths,
    platform::{Change, Platform, PlatformError, ServiceSpec},
};

const RUNTIME_LABEL: &str = "com.agbox.runtime";
const LEGACY_LABEL: &str = "com.agboxhq.watcher";

/// Real macOS platform adapter. It is deliberately small so tests can use a fixture instead.
#[derive(Clone, Debug)]
pub struct MacOsPlatform {
    home: PathBuf,
    executable: PathBuf,
}

impl MacOsPlatform {
    /// Creates the platform using an already-canonical absolute executable path.
    pub fn new(home: PathBuf, executable: PathBuf) -> Result<Self, PlatformError> {
        if !home.is_absolute() || !executable.is_absolute() {
            return Err(PlatformError::UnsafePath);
        }
        Ok(Self { home, executable })
    }

    /// Discovers the current owner home directory and executable without writing anything.
    pub fn for_current_user(executable: PathBuf) -> Result<Self, PlatformError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(PlatformError::HomeUnavailable)?;
        Self::new(home, executable)
    }

    fn launch_agents(&self) -> PathBuf {
        self.home.join("Library/LaunchAgents")
    }

    fn plist_path(&self, label: &str) -> PathBuf {
        self.launch_agents().join(format!("{label}.plist"))
    }

    fn launch_domain() -> String {
        format!("gui/{}", rustix::process::getuid().as_raw())
    }
}

impl Platform for MacOsPlatform {
    fn home(&self) -> Result<PathBuf, PlatformError> {
        validate_private_directory(&self.home)?;
        Ok(self.home.clone())
    }

    fn paths(&self) -> Result<AgboxPaths, PlatformError> {
        Ok(AgboxPaths::from_home(&self.home()?))
    }

    fn executable(&self) -> Result<PathBuf, PlatformError> {
        let metadata = fs::symlink_metadata(&self.executable)
            .map_err(|_| PlatformError::ExecutableUnavailable)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(PlatformError::ExecutableUnavailable);
        }
        Ok(self.executable.clone())
    }

    fn create_private_paths(&self, paths: &AgboxPaths) -> Result<Change, PlatformError> {
        let mut changed = false;
        for path in paths.private_directories() {
            changed |= ensure_private_directory(path)? == Change::Changed;
        }
        Ok(if changed {
            Change::Changed
        } else {
            Change::Unchanged
        })
    }

    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, PlatformError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                validate_owned_regular(&metadata)?;
                fs::read(path)
                    .map(Some)
                    .map_err(|_| PlatformError::Filesystem)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(PlatformError::Filesystem),
        }
    }

    fn write_private_file(&self, path: &Path, contents: &[u8]) -> Result<Change, PlatformError> {
        if self.read_file(path)?.as_deref() == Some(contents) {
            return Ok(Change::Unchanged);
        }
        let parent = path.parent().ok_or(PlatformError::UnsafePath)?;
        ensure_private_directory(parent)?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| PlatformError::Filesystem)?;
        temporary
            .as_file_mut()
            .write_all(contents)
            .map_err(|_| PlatformError::Filesystem)?;
        temporary
            .as_file_mut()
            .sync_all()
            .map_err(|_| PlatformError::Filesystem)?;
        temporary
            .as_file_mut()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| PlatformError::Filesystem)?;
        temporary
            .persist(path)
            .map_err(|_| PlatformError::Filesystem)?;
        let metadata = fs::symlink_metadata(path).map_err(|_| PlatformError::Filesystem)?;
        validate_owned_regular(&metadata)?;
        if metadata.mode() & 0o077 != 0 {
            return Err(PlatformError::UnsafePath);
        }
        Ok(Change::Changed)
    }

    fn install_service(&self, spec: &ServiceSpec) -> Result<Change, PlatformError> {
        if spec.label != RUNTIME_LABEL {
            return Err(PlatformError::UnsafePath);
        }
        let plist = plist_bytes(spec)?;
        self.write_private_file(&self.plist_path(spec.label), &plist)
    }

    fn start_service(&self, label: &str) -> Result<Change, PlatformError> {
        if label != RUNTIME_LABEL {
            return Err(PlatformError::UnsafePath);
        }
        let domain = Self::launch_domain();
        let plist = self.plist_path(label);
        let _ = Command::new("launchctl")
            .args(["bootstrap", &domain])
            .arg(&plist)
            .status();
        let status = Command::new("launchctl")
            .args(["kickstart", "-k", &format!("{domain}/{label}")])
            .status()
            .map_err(|_| PlatformError::Service)?;
        if status.success() {
            Ok(Change::Changed)
        } else {
            Err(PlatformError::Service)
        }
    }

    fn stop_service(&self, label: &str) -> Result<Change, PlatformError> {
        let status = Command::new("launchctl")
            .args(["bootout", &format!("{}/{}", Self::launch_domain(), label)])
            .status()
            .map_err(|_| PlatformError::Service)?;
        if status.success() {
            Ok(Change::Changed)
        } else {
            Ok(Change::Unchanged)
        }
    }

    fn retire_legacy_watcher(&self) -> Result<Change, PlatformError> {
        let legacy = self.plist_path(LEGACY_LABEL);
        let metadata = match fs::symlink_metadata(&legacy) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Change::Unchanged);
            }
            Err(_) => return Err(PlatformError::Filesystem),
        };
        validate_owned_regular(&metadata)?;
        let _ = self.stop_service(LEGACY_LABEL)?;
        let disabled = self
            .home
            .join(".agbox/legacy/com.agboxhq.watcher.plist.disabled");
        let parent = disabled.parent().ok_or(PlatformError::UnsafePath)?;
        ensure_private_directory(parent)?;
        fs::rename(&legacy, &disabled).map_err(|_| PlatformError::Filesystem)?;
        Ok(Change::Changed)
    }
}

fn ensure_private_directory(path: &Path) -> Result<Change, PlatformError> {
    let existed = path.exists();
    fs::create_dir_all(path).map_err(|_| PlatformError::Filesystem)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| PlatformError::Filesystem)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(PlatformError::UnsafePath);
    }
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| PlatformError::Filesystem)?;
    }
    validate_private_directory(path)?;
    Ok(if existed {
        Change::Unchanged
    } else {
        Change::Changed
    })
}

fn validate_private_directory(path: &Path) -> Result<(), PlatformError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PlatformError::UnsafePath)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(PlatformError::UnsafePath);
    }
    Ok(())
}

fn validate_owned_regular(metadata: &fs::Metadata) -> Result<(), PlatformError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(PlatformError::UnsafePath);
    }
    Ok(())
}

fn plist_bytes(spec: &ServiceSpec) -> Result<Vec<u8>, PlatformError> {
    use std::io::Write;

    let mut arguments = vec![Value::String(
        spec.executable.to_string_lossy().into_owned(),
    )];
    arguments.extend(spec.program_arguments.iter().cloned().map(Value::String));
    let mut dictionary = plist::Dictionary::new();
    dictionary.insert("Label".into(), Value::String(spec.label.into()));
    dictionary.insert("ProgramArguments".into(), Value::Array(arguments));
    dictionary.insert("RunAtLoad".into(), Value::Boolean(true));
    dictionary.insert("KeepAlive".into(), Value::Boolean(true));
    dictionary.insert("StandardOutPath".into(), Value::String("/dev/null".into()));
    dictionary.insert(
        "StandardErrorPath".into(),
        Value::String("/dev/null".into()),
    );
    let mut bytes = Vec::new();
    Value::Dictionary(dictionary)
        .to_writer_xml(&mut bytes)
        .map_err(|_| PlatformError::InvalidConfiguration)?;
    bytes.flush().map_err(|_| PlatformError::Filesystem)?;
    Ok(bytes)
}
