//! Filesystem confinement for the local evidence vault.
//!
//! The owner-only policy treats processes running as the current account as
//! trusted to mutate the vault, while ownership, exact modes, no-follow opens,
//! and descriptor-relative operations exclude other OS users. Intermediate
//! symlink components are accepted only when owned by the system root account.

use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use rustix::fs::{self as rustix_fs, AtFlags, FileType, Mode, OFlags};

#[cfg(test)]
mod tests {
    use super::{DirectoryIdentity, is_trusted_intermediate_symlink_owner, require_identity_match};

    #[test]
    fn intermediate_symlink_owner_policy_allows_only_root() {
        assert!(is_trusted_intermediate_symlink_owner(0));
        assert!(!is_trusted_intermediate_symlink_owner(1));
        assert!(!is_trusted_intermediate_symlink_owner(501));
        assert!(!is_trusted_intermediate_symlink_owner(u32::MAX));
    }

    #[test]
    fn startup_identity_mismatch_is_rejected() {
        let held = DirectoryIdentity {
            device: 11,
            inode: 22,
        };
        let same = DirectoryIdentity {
            device: 11,
            inode: 22,
        };
        let replaced = DirectoryIdentity {
            device: 11,
            inode: 23,
        };

        assert!(require_identity_match(held, same).is_ok());
        assert!(require_identity_match(held, replaced).is_err());
    }
}

const OWNER_DIRECTORY_MODE: u32 = 0o700;
const OWNER_FILE_MODE: u32 = 0o600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

fn require_identity_match(
    expected: DirectoryIdentity,
    observed: DirectoryIdentity,
) -> io::Result<()> {
    if expected == observed {
        Ok(())
    } else {
        Err(security_error("evidence root changed during startup"))
    }
}

fn is_trusted_intermediate_symlink_owner(uid: u32) -> bool {
    uid == 0
}

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

        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o7777 != OWNER_DIRECTORY_MODE
        {
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
                            let trusted_system_symlink = {
                                use std::os::unix::fs::MetadataExt;

                                is_trusted_intermediate_symlink_owner(metadata.uid())
                            };
                            #[cfg(not(unix))]
                            let trusted_system_symlink = false;
                            if !trusted_system_symlink {
                                return Err(security_error(
                                    "evidence root contains an untrusted symlink",
                                ));
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
            mode & 0o7777 == OWNER_DIRECTORY_MODE
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

#[cfg(unix)]
fn metadata_identity(metadata: &fs::Metadata) -> DirectoryIdentity {
    use std::os::unix::fs::MetadataExt;

    DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(unix)]
fn verify_directory_path_identity(path: &Path, expected: DirectoryIdentity) -> io::Result<()> {
    let link_metadata = fs::symlink_metadata(path)?;
    if link_metadata.file_type().is_symlink() {
        return Err(security_error("evidence root must not be a symlink"));
    }
    let metadata = fs::metadata(path)?;
    ensure_directory_metadata(&metadata)?;
    require_identity_match(expected, metadata_identity(&metadata))
}

#[cfg(not(unix))]
fn verify_directory_path_identity(path: &Path) -> io::Result<()> {
    let link_metadata = fs::symlink_metadata(path)?;
    if link_metadata.file_type().is_symlink() {
        return Err(security_error("evidence root must not be a symlink"));
    }
    ensure_directory_metadata(&fs::metadata(path)?)
}

pub(crate) fn open_bound_owner_directory(path: &Path) -> io::Result<(PathBuf, File)> {
    ensure_owner_directory(path)?;
    let directory = open_owner_directory(path)?;

    #[cfg(unix)]
    let expected = {
        let metadata = directory.metadata()?;
        ensure_directory_metadata(&metadata)?;
        metadata_identity(&metadata)
    };

    // Canonicalize only after the no-follow descriptor is held, then compare
    // both the original spelling and resolved path against that descriptor.
    let canonical = path.canonicalize()?;
    #[cfg(unix)]
    {
        verify_directory_path_identity(path, expected)?;
        verify_directory_path_identity(&canonical, expected)?;
    }
    #[cfg(not(unix))]
    {
        verify_directory_path_identity(path)?;
        verify_directory_path_identity(&canonical)?;
    }
    Ok((canonical, directory))
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

#[cfg(unix)]
fn verify_name_matches_file(directory: &File, name: &OsStr, file: &File) -> io::Result<()> {
    let expected = rustix_fs::fstat(file).map_err(io::Error::from)?;
    ensure_owner_stat(&expected, FileType::RegularFile)?;
    let observed =
        rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
    if !same_stat(&expected, &observed) {
        return Err(security_error(
            "evidence temporary file changed during publish",
        ));
    }
    ensure_owner_stat(&observed, FileType::RegularFile)
}

#[cfg(not(unix))]
fn verify_name_matches_file(directory: &File, name: &OsStr, file: &File) -> io::Result<()> {
    let _ = directory;
    let _ = name;
    ensure_file_metadata(&file.metadata()?)
}

pub(crate) fn link_owner_file(
    directory: &File,
    temporary: &OsStr,
    destination: &OsStr,
    temporary_file: &File,
) -> io::Result<()> {
    // Keep the descriptor open and verify the source name immediately before
    // linking, so a replaced temporary path cannot be published.
    verify_name_matches_file(directory, temporary, temporary_file)?;
    #[cfg(unix)]
    {
        rustix_fs::linkat(
            directory,
            temporary,
            directory,
            destination,
            AtFlags::empty(),
        )
        .map_err(io::Error::from)?;

        // A successful hard link must resolve to the same inode as the held
        // temporary descriptor. Remove the destination if the identity check
        // observes a concurrent replacement and fail closed.
        if let Err(error) = verify_name_matches_file(directory, destination, temporary_file) {
            let _ = remove_owner_file(directory, destination);
            return Err(error);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::hard_link(Path::new(temporary), Path::new(destination))?;
        if let Err(error) = verify_name_matches_file(directory, destination, temporary_file) {
            let _ = remove_owner_file(directory, destination);
            return Err(error);
        }
        Ok(())
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
