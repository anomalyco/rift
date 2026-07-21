use super::Strategy;
use crate::{CopyMode, Error, Result, filter::CopyFilter};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

pub(super) struct ApfsStrategy;

impl Strategy for ApfsStrategy {
    fn copy_directory(&self, from: &Path, to: &Path, mode: CopyMode) -> Result<()> {
        match mode {
            CopyMode::All => clone_path_apfs(from, to),
            CopyMode::Filtered => clone_filtered_directory_apfs(from, to),
        }
    }
}

fn clone_filtered_directory_apfs(from: &Path, to: &Path) -> Result<()> {
    use std::collections::HashMap;
    use std::os::unix::fs::MetadataExt;

    let filter = CopyFilter;
    let mut hard_links = HashMap::new();
    let mut directories = Vec::new();
    fs::create_dir(to)?;
    for entry in WalkDir::new(from)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .strip_prefix(from)
                .map_or(true, |path| !filter.excludes(path))
        })
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
                    clone_path_apfs(source, &destination)?;
                    hard_links.insert(key, destination.clone());
                }
            } else {
                clone_path_apfs(source, &destination)?;
            }
            copy_metadata_apfs(source, &destination, MetadataTarget::FileOrDirectory)?;
        } else if file_type.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(source)?, &destination)?;
            copy_metadata_apfs(source, &destination, MetadataTarget::Symlink)?;
        } else {
            return Err(Error::UnsupportedEntry(source.to_path_buf()));
        }
    }
    for (source, destination) in directories.into_iter().rev() {
        copy_metadata_apfs(&source, &destination, MetadataTarget::FileOrDirectory)?;
    }
    copy_metadata_apfs(from, to, MetadataTarget::FileOrDirectory)?;
    Ok(())
}

fn clone_path_apfs(from: &Path, to: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| Error::Path(format!("path contains a null byte: {}", from.display())))?;
    let destination = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| Error::Path(format!("path contains a null byte: {}", to.display())))?;
    // SAFETY: `source` and `destination` are null-terminated C strings
    // built above, and both live for the duration of the call.
    let result = unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) };
    if result == 0 {
        return Ok(());
    }
    Err(Error::CowUnavailable(format!(
        "failed to clone {}: {}",
        from.display(),
        std::io::Error::last_os_error()
    )))
}

#[derive(Clone, Copy)]
enum MetadataTarget {
    FileOrDirectory,
    Symlink,
}

fn copy_metadata_apfs(from: &Path, to: &Path, target: MetadataTarget) -> Result<()> {
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
    copy_xattrs_apfs(from, to)?;
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

fn copy_xattrs_apfs(from: &Path, to: &Path) -> Result<()> {
    let from = c_path(from)?;
    let to = c_path(to)?;
    // SAFETY: `from` is a valid C path. A null buffer with size 0 asks the
    // kernel for the required list size.
    let size =
        unsafe { libc::listxattr(from.as_ptr(), std::ptr::null_mut(), 0, libc::XATTR_NOFOLLOW) };
    if size < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut names = vec![0_u8; size as usize];
    // SAFETY: `names` was allocated with the size reported by the previous
    // `listxattr` call, and its pointer is valid for writes of that length.
    if size > 0
        && unsafe {
            libc::listxattr(
                from.as_ptr(),
                names.as_mut_ptr().cast(),
                names.len(),
                libc::XATTR_NOFOLLOW,
            )
        } < 0
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
        let size = unsafe {
            libc::getxattr(
                from.as_ptr(),
                name.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        if size < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut value = vec![0_u8; size as usize];
        // SAFETY: `value` was allocated with the exact size reported by
        // `getxattr`, and the path and attribute name are valid C strings.
        if size > 0
            && unsafe {
                libc::getxattr(
                    from.as_ptr(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: `to`, `name`, and `value` are valid for the duration of the
        // call. `XATTR_NOFOLLOW` keeps symlink behavior consistent.
        if unsafe {
            libc::setxattr(
                to.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn c_path(path: &Path) -> Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::Path(format!("path contains a null byte: {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use tempfile::TempDir;

    #[test]
    fn strategy_clones_and_removes_a_workspace() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("nested")).unwrap();
        fs::write(source.join("nested/file.txt"), "hello").unwrap();
        let strategy = ApfsStrategy;

        strategy
            .copy_directory(&source, &destination, CopyMode::All)
            .unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("nested/file.txt")).unwrap(),
            "hello"
        );
        strategy.remove_directory(&destination).unwrap();
        assert!(!destination.exists());
    }

    #[test]
    fn integration_environment_is_required_by_ci() {
        if std::env::var_os("RIFT_REQUIRE_APFS_TESTS").is_some() {
            let temp = TempDir::new().unwrap();
            let source = temp.path().join("source");
            let destination = temp.path().join("destination");
            fs::create_dir(&source).unwrap();
            assert!(
                ApfsStrategy
                    .copy_directory(&source, &destination, CopyMode::All)
                    .is_ok()
            );
        }
    }

    #[test]
    fn filtered_strategy_preserves_included_metadata_and_hard_links() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let nested = source.join("nested");
        fs::create_dir(&source).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o750)).unwrap();
        fs::create_dir(&nested).unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o710)).unwrap();
        let file = nested.join("file.txt");
        fs::write(&file, "hello").unwrap();
        let file_path = c_path(&file).unwrap();
        let attribute = std::ffi::CString::new("com.rift.test").unwrap();
        let attribute_value = b"preserved";
        // SAFETY: the path and attribute are valid C strings, and the value
        // pointer is valid for `attribute_value.len()` bytes.
        assert_eq!(
            unsafe {
                libc::setxattr(
                    file_path.as_ptr(),
                    attribute.as_ptr(),
                    attribute_value.as_ptr().cast(),
                    attribute_value.len(),
                    0,
                    0,
                )
            },
            0
        );
        // The read-only mode makes a redundant xattr rewrite fail with EACCES,
        // while the special bits verify clonefile's mode exception is repaired.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o6555)).unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
            0o6555
        );
        // Confirm this fixture rejects the same setxattr operation the old
        // metadata replay performed.
        assert_eq!(
            unsafe {
                libc::setxattr(
                    file_path.as_ptr(),
                    attribute.as_ptr(),
                    attribute_value.as_ptr().cast(),
                    attribute_value.len(),
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EACCES)
        );
        fs::hard_link(&file, nested.join("hard.txt")).unwrap();
        std::os::unix::fs::symlink("file.txt", nested.join("link.txt")).unwrap();
        fs::create_dir_all(source.join("node_modules/pkg")).unwrap();
        fs::write(source.join("node_modules/pkg/index.js"), "module").unwrap();

        ApfsStrategy
            .copy_directory(&source, &destination, CopyMode::Filtered)
            .unwrap();

        assert!(!destination.join("node_modules").exists());
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
                & 0o7777,
            0o6555
        );
        let cloned_file = c_path(&destination.join("nested/file.txt")).unwrap();
        let mut cloned_attribute = [0_u8; 9];
        // SAFETY: the path and attribute are valid C strings, and the buffer
        // pointer is valid for `cloned_attribute.len()` bytes.
        let cloned_attribute_size = unsafe {
            libc::getxattr(
                cloned_file.as_ptr(),
                attribute.as_ptr(),
                cloned_attribute.as_mut_ptr().cast(),
                cloned_attribute.len(),
                0,
                0,
            )
        };
        assert_eq!(cloned_attribute_size, attribute_value.len() as isize);
        assert_eq!(&cloned_attribute, attribute_value);
        assert_eq!(
            fs::metadata(destination.join("nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o710
        );
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }
}
