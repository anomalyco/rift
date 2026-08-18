use super::{Strategy, StrategyInit};
use crate::{CopyMode, Error, InitProgress, Result, filter::CopyFilter};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

pub(super) struct LinuxReflinkStrategy;

impl Strategy for LinuxReflinkStrategy {
    fn copy_directory(&self, from: &Path, to: &Path, mode: CopyMode) -> Result<()> {
        let destination_parent = same_filesystem_parent(from, to)?;
        verify_reflinks_linux(destination_parent)?;
        match mode {
            CopyMode::All => clone_directory_linux(from, to),
            CopyMode::Filtered => clone_directory_linux_filtered(from, to),
        }
    }

    fn initialize_directory(
        &self,
        path: &Path,
        _progress: &mut dyn FnMut(InitProgress),
    ) -> Result<StrategyInit> {
        verify_reflinks_linux(path)?;
        Ok(StrategyInit::AlreadyNative)
    }
}

fn same_filesystem_parent<'a>(from: &Path, to: &'a Path) -> Result<&'a Path> {
    use std::os::unix::fs::MetadataExt;

    let destination_parent = to
        .parent()
        .ok_or_else(|| Error::Path(format!("destination has no parent: {}", to.display())))?;
    if fs::metadata(from)?.dev() == fs::metadata(destination_parent)?.dev() {
        Ok(destination_parent)
    } else {
        Err(Error::CowUnavailable(format!(
            "Linux reflinks require source and destination on the same filesystem: {}",
            to.display()
        )))
    }
}

pub(super) fn clone_directory_linux(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir(to)?;
    import_directory_linux(from, to, &mut |_| {})?;
    copy_metadata_linux(from, to, MetadataTarget::FileOrDirectory)
}

pub(super) fn clone_directory_linux_filtered(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir(to)?;
    import_directory_linux_filtered(from, to, &mut |_| {})?;
    copy_metadata_linux(from, to, MetadataTarget::FileOrDirectory)
}

pub(super) fn import_directory_linux(
    from: &Path,
    to: &Path,
    progress: &mut dyn FnMut(InitProgress),
) -> Result<()> {
    import_directory_linux_with_filter(from, to, progress, None)
}

pub(super) fn import_directory_linux_filtered(
    from: &Path,
    to: &Path,
    progress: &mut dyn FnMut(InitProgress),
) -> Result<()> {
    import_directory_linux_with_filter(from, to, progress, Some(CopyFilter::for_source(from)))
}

fn import_directory_linux_with_filter(
    from: &Path,
    to: &Path,
    progress: &mut dyn FnMut(InitProgress),
    filter: Option<CopyFilter>,
) -> Result<()> {
    use std::collections::HashMap;
    use std::os::unix::fs::MetadataExt;

    let mut hard_links = HashMap::new();
    let mut directories = Vec::new();
    for (entry, entries) in WalkDir::new(from)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            filter.as_ref().map_or(true, |filter| {
                entry
                    .path()
                    .strip_prefix(from)
                    .map_or(true, |path| !filter.excludes(path))
            })
        })
        .zip(1..)
    {
        let entry = entry?;
        let source = entry.path();
        let destination = to.join(
            source
                .strip_prefix(from)
                .map_err(|error| Error::Path(error.to_string()))?,
        );
        let metadata = fs::symlink_metadata(source)?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            fs::create_dir(&destination)?;
            directories.push((source.to_path_buf(), destination));
        } else if file_type.is_file() {
            let key = (metadata.dev(), metadata.ino());
            if metadata.nlink() > 1 {
                if let Some(existing) = hard_links.get(&key) {
                    fs::hard_link(existing, &destination)?;
                } else {
                    reflink_file_linux(source, &destination)?;
                    hard_links.insert(key, destination.clone());
                }
            } else {
                reflink_file_linux(source, &destination)?;
            }
            copy_metadata_linux(source, &destination, MetadataTarget::FileOrDirectory)?;
        } else if file_type.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(source)?, &destination)?;
            copy_metadata_linux(source, &destination, MetadataTarget::Symlink)?;
        } else {
            return Err(Error::UnsupportedEntry(source.to_path_buf()));
        }
        progress(InitProgress::ImportedEntries { entries });
    }
    for (source, destination) in directories.into_iter().rev() {
        copy_metadata_linux(&source, &destination, MetadataTarget::FileOrDirectory)?;
    }
    Ok(())
}

pub(super) fn reflink_file_linux(from: &Path, to: &Path) -> Result<()> {
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;

    const FICLONE: libc::c_ulong = 0x4004_9409;
    let source = File::open(from)?;
    let destination = OpenOptions::new().write(true).create_new(true).open(to)?;
    // SAFETY: both file descriptors come from live `File` values, and FICLONE
    // only reads the source fd while mutating the destination file.
    if unsafe { libc::ioctl(destination.as_raw_fd(), FICLONE, source.as_raw_fd()) } == 0 {
        return Ok(());
    }
    Err(Error::CowUnavailable(format!(
        "failed to reflink {}: {}",
        from.display(),
        std::io::Error::last_os_error()
    )))
}

pub(super) fn verify_reflinks_linux(path: &Path) -> Result<()> {
    let operation_id = ulid::Ulid::new();
    let source = path.join(format!(".rift-reflink-probe-{operation_id}"));
    let destination = path.join(format!(".rift-reflink-probe-copy-{operation_id}"));
    fs::write(&source, b"rift")?;
    let result = reflink_file_linux(&source, &destination).map_err(|error| match error {
        Error::CowUnavailable(message) => Error::CowUnavailable(format!(
            "{} does not support Linux copy-on-write reflinks: {message}",
            path.display()
        )),
        error => error,
    });
    let cleanup = [&source, &destination]
        .into_iter()
        .filter(|path| path.exists())
        .try_for_each(fs::remove_file);
    result.and(cleanup.map_err(Error::from))
}

#[derive(Clone, Copy)]
pub(super) enum MetadataTarget {
    FileOrDirectory,
    Symlink,
}

pub(super) fn copy_metadata_linux(from: &Path, to: &Path, target: MetadataTarget) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::symlink_metadata(from)?;
    let destination = c_path(to)?;
    // SAFETY: `destination` is a valid null-terminated path, and uid/gid come
    // from filesystem metadata for `from`.
    if unsafe { libc::lchown(destination.as_ptr(), metadata.uid(), metadata.gid()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if matches!(target, MetadataTarget::FileOrDirectory) {
        fs::set_permissions(to, fs::Permissions::from_mode(metadata.mode()))?;
    }
    copy_xattrs_linux(from, to)?;
    let times = [
        libc::timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        libc::timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    ];
    // SAFETY: `destination` is a live C string and `times` contains exactly the
    // two timestamps expected by `utimensat`.
    if unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            destination.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn copy_xattrs_linux(from: &Path, to: &Path) -> Result<()> {
    let from = c_path(from)?;
    let to = c_path(to)?;
    // SAFETY: `from` is a valid C path. A null buffer with size 0 asks the
    // kernel for the required list size.
    let size = unsafe { libc::llistxattr(from.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut names = vec![0_u8; size as usize];
    // SAFETY: `names` was allocated with the size reported by the previous
    // `llistxattr` call, and its pointer is valid for writes of that length.
    if size > 0
        && unsafe { libc::llistxattr(from.as_ptr(), names.as_mut_ptr().cast(), names.len()) } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::ffi::CString::new(name)
            .map_err(|_| Error::Path("extended attribute name contains a null byte".into()))?;
        // SAFETY: `from` and `name` are valid C strings. A null buffer with
        // size 0 asks the kernel for this attribute's value length.
        let size =
            unsafe { libc::lgetxattr(from.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if size < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut value = vec![0_u8; size as usize];
        // SAFETY: `value` was allocated with the exact size reported by
        // `lgetxattr`, and the path and attribute name are valid C strings.
        if size > 0
            && unsafe {
                libc::lgetxattr(
                    from.as_ptr(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: `to` and `name` are valid C strings, and `value` points to
        // an initialized byte buffer of the supplied length.
        if unsafe {
            libc::lsetxattr(
                to.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

pub(super) fn c_path(path: &Path) -> Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::Path(format!("path contains a null byte: {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::linux::{Filesystem, LinuxStrategy, filesystem};
    use crate::test_support::linux_extents::assert_shared_extents_when_reliable;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use tempfile::{Builder, TempDir};

    fn reflink_temp() -> Option<TempDir> {
        let temp = Builder::new()
            .prefix(".rift-core-test-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        (!matches!(filesystem(temp.path()).unwrap(), Filesystem::Btrfs)
            && verify_reflinks_linux(temp.path()).is_ok())
        .then_some(temp)
    }

    fn non_reflink_temp() -> Option<TempDir> {
        let temp = Builder::new()
            .prefix(".rift-core-test-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        (!matches!(filesystem(temp.path()).unwrap(), Filesystem::Btrfs)
            && verify_reflinks_linux(temp.path()).is_err())
        .then_some(temp)
    }

    #[test]
    fn linux_reflink_integration_environment_is_available() {
        if std::env::var_os("RIFT_REQUIRE_REFLINK_TESTS").is_some() {
            assert!(
                reflink_temp().is_some(),
                "RIFT_REQUIRE_REFLINK_TESTS requires a non-btrfs Linux filesystem with reflinks"
            );
        }
    }

    #[test]
    fn native_init_verifies_reflink_support() {
        let Some(temp) = reflink_temp() else {
            return;
        };
        assert_eq!(
            LinuxStrategy
                .initialize_directory(temp.path(), &mut |_| {})
                .unwrap(),
            StrategyInit::AlreadyNative
        );
    }

    #[test]
    fn native_copy_preserves_files_links_and_metadata() {
        let Some(temp) = reflink_temp() else {
            return;
        };
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let nested = source.join("nested");
        fs::create_dir(&source).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o750)).unwrap();
        fs::create_dir(&nested).unwrap();
        let file = nested.join("file.txt");
        let cow = nested.join("cow.txt");
        fs::write(&file, "hello").unwrap();
        fs::write(&cow, "shared before mutation").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        fs::hard_link(&file, nested.join("hard.txt")).unwrap();
        std::os::unix::fs::symlink("file.txt", nested.join("link.txt")).unwrap();

        LinuxStrategy
            .copy_directory(&source, &destination, CopyMode::All)
            .unwrap();

        assert_eq!(
            fs::read_to_string(destination.join("nested/file.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            fs::read_link(destination.join("nested/link.txt")).unwrap(),
            Path::new("file.txt")
        );
        assert_eq!(
            fs::metadata(destination.join("nested/file.txt"))
                .unwrap()
                .ino(),
            fs::metadata(destination.join("nested/hard.txt"))
                .unwrap()
                .ino()
        );
        assert_eq!(
            fs::metadata(destination.join("nested/file.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o750
        );
        assert_shared_extents_when_reliable(&cow, &destination.join("nested/cow.txt"));
        assert_copy_diverges_after_mutation(&cow, &destination.join("nested/cow.txt"));
        LinuxStrategy.remove_directory(&destination).unwrap();
        assert!(!destination.exists());
    }

    #[test]
    fn native_copy_rejects_storage_on_another_filesystem() {
        let Some(temp) = reflink_temp() else {
            return;
        };
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let other = TempDir::new().unwrap();
        if fs::metadata(&source).unwrap().dev() == fs::metadata(other.path()).unwrap().dev() {
            return;
        }

        assert!(matches!(
            LinuxStrategy.copy_directory(&source, &other.path().join("destination"), CopyMode::All),
            Err(Error::CowUnavailable(_))
        ));
    }

    #[test]
    fn native_copy_rejects_empty_trees_without_reflink_support() {
        let Some(temp) = non_reflink_temp() else {
            return;
        };
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).unwrap();

        assert!(matches!(
            LinuxStrategy.copy_directory(&source, &destination, CopyMode::All),
            Err(Error::CowUnavailable(_))
        ));
        assert!(!destination.exists());
    }

    fn assert_copy_diverges_after_mutation(source: &Path, clone: &Path) {
        let original = fs::read_to_string(source).unwrap();
        assert_eq!(fs::read_to_string(clone).unwrap(), original);
        fs::write(source, "parent mutation").unwrap();
        assert_eq!(fs::read_to_string(clone).unwrap(), original);
        fs::write(clone, "child mutation").unwrap();
        assert_eq!(fs::read_to_string(source).unwrap(), "parent mutation");
        assert_eq!(fs::read_to_string(clone).unwrap(), "child mutation");
    }
}
