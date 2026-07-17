use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use rustix::fs::{self as rustix_fs, Mode, OFlags};

const OWNER_DIRECTORY_MODE: u32 = 0o700;
const OWNER_FILE_MODE: u32 = 0o600;

fn security_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn invalid_path_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn ensure_directory_metadata(metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.is_dir() {
        return Err(security_error("evidence root is not a directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(security_error("evidence root is not owner-controlled"));
        }
    }
    Ok(())
}

fn create_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, OWNER_DIRECTORY_MODE);
    match builder.create(path) {
        Ok(()) => {
            set_directory_mode(path)?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn set_directory_mode(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::symlink_metadata(path)?.permissions();
        permissions.set_mode(OWNER_DIRECTORY_MODE);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(invalid_path_error("evidence root must not be empty"));
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(crate) fn ensure_owner_directory(path: &Path) -> io::Result<()> {
    let absolute = absolute_path(path)?;
    let mut current = PathBuf::new();

    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(invalid_path_error(
                    "evidence root contains parent traversal",
                ));
            }
            Component::Normal(name) => {
                current.push(name);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) => {
                        if metadata.file_type().is_symlink() {
                            if current == absolute {
                                return Err(security_error("evidence root must not be a symlink"));
                            }
                            if !fs::metadata(&current)?.is_dir() {
                                return Err(security_error(
                                    "evidence root contains a symlink or non-directory",
                                ));
                            }
                        } else if !metadata.is_dir() {
                            return Err(security_error(
                                "evidence root contains a symlink or non-directory",
                            ));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        create_directory(&current)?;
                        let metadata = fs::symlink_metadata(&current)?;
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            return Err(security_error(
                                "evidence root contains a symlink or non-directory",
                            ));
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }

    let metadata = fs::symlink_metadata(&absolute)?;
    if metadata.file_type().is_symlink() {
        return Err(security_error("evidence root must not be a symlink"));
    }
    ensure_directory_metadata(&metadata)
}

pub(crate) fn set_owner_file_mode(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix_fs::fchmod(file, Mode::from_raw_mode(0o600)).map_err(io::Error::from)?;
    }
    Ok(())
}

fn ensure_file_metadata(metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.is_file() {
        return Err(security_error("evidence target is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o7777 != OWNER_FILE_MODE
        {
            return Err(security_error("evidence target is not owner-controlled"));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    before.dev() == after.dev() && before.ino() == after.ino()
}

#[cfg(not(unix))]
fn same_file(_before: &fs::Metadata, _after: &fs::Metadata) -> bool {
    true
}

fn open_owner_file(path: &Path) -> io::Result<File> {
    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() {
        return Err(security_error("evidence target must not be a symlink"));
    }
    ensure_file_metadata(&before)?;

    let parent = path
        .parent()
        .ok_or_else(|| invalid_path_error("evidence target has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid_path_error("evidence target has no file name"))?;
    let directory = File::open(parent)?;

    #[cfg(unix)]
    let file = File::from(
        rustix_fs::openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    #[cfg(not(unix))]
    let file = File::open(path)?;

    let after = file.metadata()?;
    if !same_file(&before, &after) {
        return Err(security_error("evidence target changed during open"));
    }
    ensure_file_metadata(&after)?;
    Ok(file)
}

pub(crate) fn validate_owner_file(path: &Path) -> io::Result<()> {
    let _file = open_owner_file(path)?;
    Ok(())
}

pub(crate) fn read_owner_file_nofollow(path: &Path, cap: usize) -> io::Result<Vec<u8>> {
    let file = open_owner_file(path)?;
    let metadata = file.metadata()?;
    let size = usize::try_from(metadata.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "evidence file is too large"))?;
    if size > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "evidence file is too large",
        ));
    }

    let mut contents = Vec::with_capacity(size);
    file.take(cap.saturating_add(1) as u64)
        .read_to_end(&mut contents)?;
    if contents.len() > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "evidence file is too large",
        ));
    }
    Ok(contents)
}
