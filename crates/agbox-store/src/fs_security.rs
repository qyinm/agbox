use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use rustix::fs::{self as rustix_fs, AtFlags, FileType, Mode, OFlags};

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
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::MetadataExt;

                                if metadata.uid() == rustix::process::geteuid().as_raw() {
                                    return Err(security_error(
                                        "evidence root contains a user-owned symlink",
                                    ));
                                }
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

fn ensure_owner_stat(stat: &rustix_fs::Stat, expected: FileType) -> io::Result<()> {
    if FileType::from_raw_mode(stat.st_mode) != expected {
        return Err(if expected == FileType::Directory {
            security_error("evidence root is not a directory")
        } else {
            security_error("evidence target is not a regular file")
        });
    }
    #[cfg(unix)]
    {
        let mode = u32::from(stat.st_mode);
        #[allow(clippy::verbose_bit_mask)]
        let mode_ok = if expected == FileType::Directory {
            mode & 0o077 == 0
        } else {
            mode & 0o7777 == OWNER_FILE_MODE
        };
        if stat.st_uid != rustix::process::geteuid().as_raw() || !mode_ok {
            return Err(if expected == FileType::Directory {
                security_error("evidence root is not owner-controlled")
            } else {
                security_error("evidence target is not owner-controlled")
            });
        }
    }
    Ok(())
}

pub(crate) fn open_owner_directory(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let directory = File::from(
        rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    #[cfg(not(unix))]
    let directory = File::open(path)?;
    #[cfg(unix)]
    ensure_owner_stat(
        &rustix_fs::fstat(&directory).map_err(io::Error::from)?,
        FileType::Directory,
    )?;
    #[cfg(not(unix))]
    ensure_directory_metadata(&directory.metadata()?)?;
    Ok(directory)
}

pub(crate) fn create_owner_temp_file(directory: &File, name: &OsStr) -> io::Result<File> {
    #[cfg(unix)]
    {
        Ok(File::from(
            rustix_fs::openat(
                directory,
                name,
                OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW,
                Mode::from_raw_mode(0o600),
            )
            .map_err(io::Error::from)?,
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        File::create(Path::new(name))
    }
}

pub(crate) fn link_owner_file(
    directory: &File,
    temporary: &OsStr,
    destination: &OsStr,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix_fs::linkat(
            directory,
            temporary,
            directory,
            destination,
            AtFlags::empty(),
        )
        .map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        fs::hard_link(Path::new(temporary), Path::new(destination))
    }
}

pub(crate) fn remove_owner_file(directory: &File, name: &OsStr) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix_fs::unlinkat(directory, name, AtFlags::empty()).map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        fs::remove_file(Path::new(name))
    }
}

#[cfg(not(unix))]
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
#[cfg(unix)]
fn same_stat(before: &rustix_fs::Stat, after: &rustix_fs::Stat) -> bool {
    before.st_dev == after.st_dev && before.st_ino == after.st_ino
}

#[cfg(unix)]
fn open_owner_file(directory: &File, name: &OsStr) -> io::Result<File> {
    let before =
        rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
    ensure_owner_stat(&before, FileType::RegularFile)?;
    let file = File::from(
        rustix_fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    let after = rustix_fs::fstat(&file).map_err(io::Error::from)?;
    if !same_stat(&before, &after) {
        return Err(security_error("evidence target changed during open"));
    }
    ensure_owner_stat(&after, FileType::RegularFile)?;
    Ok(file)
}

#[cfg(not(unix))]
fn open_owner_file(directory: &File, name: &OsStr) -> io::Result<File> {
    let _ = directory;
    let path = Path::new(name);
    let before = fs::symlink_metadata(path)?;
    ensure_file_metadata(&before)?;
    let file = File::open(path)?;
    ensure_file_metadata(&file.metadata()?)?;
    Ok(file)
}

pub(crate) fn validate_owner_file(directory: &File, name: &OsStr) -> io::Result<()> {
    let _file = open_owner_file(directory, name)?;
    Ok(())
}

pub(crate) fn read_owner_file_nofollow(
    directory: &File,
    name: &OsStr,
    cap: usize,
) -> io::Result<Vec<u8>> {
    let file = open_owner_file(directory, name)?;
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
