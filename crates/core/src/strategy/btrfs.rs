use super::linux::{Filesystem, filesystem};
#[cfg(test)]
use super::reflink::c_path;
use super::reflink::{
    MetadataTarget, copy_metadata_linux, import_directory_linux, import_directory_linux_filtered,
};
use super::{Strategy, StrategyInit};
use crate::{CopyMode, Error, InitProgress, Result};
use std::fs;
use std::path::Path;

pub(super) struct BtrfsStrategy;

impl Strategy for BtrfsStrategy {
    fn copy_directory(&self, from: &Path, to: &Path, mode: CopyMode) -> Result<()> {
        copy_directory_linux(from, to, mode)
    }

    fn initialize_directory(
        &self,
        path: &Path,
        progress: &mut dyn FnMut(InitProgress),
    ) -> Result<StrategyInit> {
        initialize_directory_linux(path, progress)
    }

    fn remove_directory(&self, path: &Path) -> Result<()> {
        remove_directory_linux(path)
    }
}

fn copy_directory_linux(from: &Path, to: &Path, mode: CopyMode) -> Result<()> {
    if !is_btrfs_filesystem(from)? {
        return Err(Error::CowUnavailable(format!(
            "Linux snapshot creation requires btrfs; {} is on another filesystem",
            from.display()
        )));
    }
    if !is_btrfs_subvolume(from)? {
        return Err(Error::InitializationRequired(from.to_path_buf()));
    }
    match mode {
        CopyMode::All => create_btrfs_snapshot(from, to),
        CopyMode::Filtered => create_filtered_btrfs_subvolume(from, to),
    }
}

#[cfg(target_os = "linux")]
fn create_filtered_btrfs_subvolume(from: &Path, to: &Path) -> Result<()> {
    create_btrfs_subvolume(to)?;
    import_directory_linux_filtered(from, to, &mut |_| {})?;
    copy_metadata_linux(from, to, MetadataTarget::FileOrDirectory)
}

#[cfg(target_os = "linux")]
fn initialize_directory_linux(
    path: &Path,
    progress: &mut dyn FnMut(InitProgress),
) -> Result<StrategyInit> {
    if !is_btrfs_filesystem(path)? {
        return Err(Error::CowUnavailable(format!(
            "{} is not on a btrfs filesystem",
            path.display()
        )));
    }
    if is_btrfs_subvolume(path)? {
        return Ok(StrategyInit::AlreadyNative);
    }

    let parent = path
        .parent()
        .ok_or_else(|| Error::Path(format!("workspace has no parent: {}", path.display())))?;
    let operation_id = ulid::Ulid::new();
    let staging = parent.join(format!(".rift-init-{operation_id}"));
    let original = parent.join(format!(".rift-init-original-{operation_id}"));

    progress(InitProgress::CreatingSubvolume);
    create_btrfs_subvolume(&staging)?;

    let result = (|| {
        progress(InitProgress::ImportingWorkspace);
        import_directory_linux(path, &staging, progress)?;
        progress(InitProgress::ActivatingWorkspace);
        fs::rename(path, &original).map_err(|error| {
            Error::CowUnavailable(format!(
                "failed to move original workspace aside for activation: {error}"
            ))
        })?;
        if let Err(error) = fs::rename(&staging, path) {
            return match fs::rename(&original, path) {
                Ok(()) => Err(Error::CowUnavailable(format!(
                    "failed to activate initialized workspace; restored the original workspace: {error}"
                ))),
                Err(rollback) => Err(Error::CowUnavailable(format!(
                    "failed to activate initialized workspace: {error}; also failed to restore the original workspace: {rollback}"
                ))),
            };
        }
        copy_metadata_linux(&original, path, MetadataTarget::FileOrDirectory)?;
        progress(InitProgress::RemovingOriginal);
        fs::remove_dir_all(&original).map_err(|error| {
            Error::CowUnavailable(format!(
                "initialized workspace is active but failed to remove the original directory: {error}"
            ))
        })?;
        Ok(StrategyInit::Converted)
    })();
    if result.is_err() && staging.exists() {
        let _ = remove_directory_linux(&staging);
    }
    result
}

#[cfg(target_os = "linux")]
fn remove_directory_linux(path: &Path) -> Result<()> {
    if !is_btrfs_subvolume(path)? {
        fs::remove_dir_all(path)?;
        return Ok(());
    }
    delete_btrfs_subvolume(path)
}

#[cfg(target_os = "linux")]
fn is_btrfs_subvolume(path: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;

    if !is_btrfs_filesystem(path)? {
        return Ok(false);
    }
    Ok(fs::metadata(path)?.ino() == 256)
}

#[cfg(target_os = "linux")]
fn is_btrfs_filesystem(path: &Path) -> Result<bool> {
    Ok(matches!(filesystem(path)?, Filesystem::Btrfs))
}

#[cfg(target_os = "linux")]
fn create_btrfs_subvolume(path: &Path) -> Result<()> {
    btrfs_path_ioctl(path, BTRFS_IOC_SUBVOL_CREATE, None, "create subvolume")
}

#[cfg(target_os = "linux")]
fn create_btrfs_snapshot(from: &Path, to: &Path) -> Result<()> {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    let source = File::open(from)?;
    btrfs_path_ioctl(
        to,
        BTRFS_IOC_SNAP_CREATE,
        Some(source.as_raw_fd()),
        "snapshot",
    )
}

#[cfg(target_os = "linux")]
fn delete_btrfs_subvolume(path: &Path) -> Result<()> {
    match btrfs_path_ioctl(path, BTRFS_IOC_SNAP_DESTROY, None, "delete subvolume") {
        Ok(()) => Ok(()),
        Err(Error::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) =>
        {
            remove_emptyable_subvolume(path)
        }
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn remove_emptyable_subvolume(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry_path)?;
        } else {
            fs::remove_file(entry_path)?;
        }
    }
    fs::remove_dir(path)?;
    Ok(())
}

#[cfg(target_os = "linux")]
const BTRFS_IOC_SNAP_CREATE: libc::c_ulong = 0x5000_9401;
#[cfg(target_os = "linux")]
const BTRFS_IOC_SUBVOL_CREATE: libc::c_ulong = 0x5000_940e;
#[cfg(target_os = "linux")]
const BTRFS_IOC_SNAP_DESTROY: libc::c_ulong = 0x5000_940f;

#[cfg(target_os = "linux")]
#[repr(C)]
struct BtrfsIoctlVolArgs {
    fd: i64,
    name: [libc::c_char; 4088],
}

#[cfg(target_os = "linux")]
fn btrfs_path_ioctl(
    path: &Path,
    request: libc::c_ulong,
    source_fd: Option<libc::c_int>,
    action: &str,
) -> Result<()> {
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let parent = path
        .parent()
        .ok_or_else(|| Error::Path(format!("path has no parent: {}", path.display())))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::Path(format!("path has no name: {}", path.display())))?
        .as_bytes();
    if name.is_empty() || name.len() >= 4088 || name.contains(&0) {
        return Err(Error::Path(format!(
            "invalid btrfs subvolume name: {}",
            path.display()
        )));
    }
    let mut args = BtrfsIoctlVolArgs {
        fd: source_fd.map_or(0, i64::from),
        name: [0; 4088],
    };
    for (destination, byte) in args.name.iter_mut().zip(name) {
        *destination = *byte as libc::c_char;
    }
    let parent = File::open(parent)?;
    // SAFETY: `parent` is an open directory fd, and `args` has the C layout
    // expected by the btrfs volume ioctls for the duration of this call.
    let result = unsafe { libc::ioctl(parent.as_raw_fd(), request, &args) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if request == BTRFS_IOC_SNAP_DESTROY {
        return Err(Error::Io(error));
    }
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return Err(Error::AlreadyExists(path.to_path_buf()));
    }
    Err(Error::CowUnavailable(format!(
        "failed to {action} {}: {error}",
        path.display()
    )))
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use tempfile::{Builder, TempDir};

    fn btrfs_temp() -> Option<TempDir> {
        let temp = Builder::new()
            .prefix(".rift-core-test-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        is_btrfs_filesystem(temp.path()).unwrap().then_some(temp)
    }

    #[test]
    fn btrfs_integration_environment_is_available() {
        if std::env::var_os("RIFT_REQUIRE_BTRFS_TESTS").is_some() {
            assert!(
                btrfs_temp().is_some(),
                "RIFT_REQUIRE_BTRFS_TESTS requires the checkout filesystem to be btrfs"
            );
        }
    }

    fn set_xattr(path: &Path, name: &str, value: &[u8]) {
        let path = c_path(path).unwrap();
        let name = std::ffi::CString::new(name).unwrap();
        assert_eq!(
            // SAFETY: test inputs are valid C strings and `value` is a live
            // byte slice whose contents are copied by the kernel.
            unsafe {
                libc::lsetxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            },
            0
        );
    }

    fn get_xattr(path: &Path, name: &str) -> Vec<u8> {
        let path = c_path(path).unwrap();
        let name = std::ffi::CString::new(name).unwrap();
        // SAFETY: test inputs are valid C strings. A null buffer with size 0
        // requests the attribute value length.
        let size =
            unsafe { libc::lgetxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
        assert!(size >= 0);
        let mut value = vec![0; size as usize];
        assert_eq!(
            // SAFETY: `value` is allocated with the exact size returned by
            // `lgetxattr`, and the C strings live for this call.
            unsafe {
                libc::lgetxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            },
            size
        );
        value
    }

    #[test]
    fn native_init_imports_files_links_metadata_and_progress() {
        let Some(temp) = btrfs_temp() else {
            return;
        };
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o750)).unwrap();
        let nested = source.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o700)).unwrap();
        let file = nested.join("file.txt");
        fs::write(&file, "hello").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        set_xattr(&file, "user.rift_test", b"xattr");
        fs::hard_link(&file, nested.join("hard.txt")).unwrap();
        std::os::unix::fs::symlink("file.txt", nested.join("link.txt")).unwrap();
        let mut progress = Vec::new();

        assert_eq!(
            initialize_directory_linux(&source, &mut |event| progress.push(event)).unwrap(),
            StrategyInit::Converted
        );
        assert!(is_btrfs_subvolume(&source).unwrap());
        assert_eq!(
            fs::read_to_string(source.join("nested/file.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            fs::read_link(source.join("nested/link.txt")).unwrap(),
            Path::new("file.txt")
        );
        assert_eq!(
            fs::metadata(source.join("nested/file.txt")).unwrap().ino(),
            fs::metadata(source.join("nested/hard.txt")).unwrap().ino()
        );
        assert_eq!(
            fs::metadata(source.join("nested/file.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            get_xattr(&source.join("nested/file.txt"), "user.rift_test"),
            b"xattr"
        );
        assert!(progress.contains(&InitProgress::CreatingSubvolume));
        assert!(progress.contains(&InitProgress::ImportingWorkspace));
        assert!(progress.contains(&InitProgress::ActivatingWorkspace));
        assert!(progress.contains(&InitProgress::RemovingOriginal));
        assert!(
            progress
                .iter()
                .any(|event| matches!(event, InitProgress::ImportedEntries { .. }))
        );
        remove_directory_linux(&source).unwrap();
    }

    #[test]
    fn native_snapshot_and_delete_use_btrfs_strategy() {
        let Some(temp) = btrfs_temp() else {
            return;
        };
        let source = temp.path().join("source");
        let snapshot = temp.path().join("snapshot");
        let filtered = temp.path().join("filtered");
        create_btrfs_subvolume(&source).unwrap();
        fs::write(source.join("file.txt"), "shared before mutation").unwrap();

        copy_directory_linux(&source, &snapshot, CopyMode::All).unwrap();
        assert!(is_btrfs_subvolume(&snapshot).unwrap());
        assert_eq!(
            fs::read_to_string(snapshot.join("file.txt")).unwrap(),
            "shared before mutation"
        );
        assert_copy_diverges_after_mutation(&source.join("file.txt"), &snapshot.join("file.txt"));

        copy_directory_linux(&source, &filtered, CopyMode::Filtered).unwrap();
        assert!(is_btrfs_subvolume(&filtered).unwrap());
        assert_copy_diverges_after_mutation(&source.join("file.txt"), &filtered.join("file.txt"));

        remove_directory_linux(&filtered).unwrap();
        remove_directory_linux(&snapshot).unwrap();
        remove_directory_linux(&source).unwrap();
        assert!(!filtered.exists());
        assert!(!snapshot.exists());
        assert!(!source.exists());
    }

    #[test]
    fn native_strategy_reports_non_btrfs_and_unsupported_entries() {
        let temp = TempDir::new().unwrap();
        assert!(!is_btrfs_filesystem(temp.path()).unwrap());
        assert!(!is_btrfs_subvolume(temp.path()).unwrap());
        assert!(matches!(
            initialize_directory_linux(temp.path(), &mut |_| {}),
            Err(Error::CowUnavailable(_))
        ));
        assert!(matches!(
            copy_directory_linux(temp.path(), &temp.path().join("snapshot"), CopyMode::All),
            Err(Error::CowUnavailable(_))
        ));

        let Some(btrfs) = btrfs_temp() else {
            return;
        };
        let from = btrfs.path().join("source");
        let to = btrfs.path().join("destination");
        fs::create_dir(&from).unwrap();
        fs::create_dir(&to).unwrap();
        let fifo = from.join("fifo");
        let fifo_name = c_path(&fifo).unwrap();
        // SAFETY: `fifo_name` is a valid C path and the mode is a normal
        // permission bitmask for creating a FIFO in this test.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            import_directory_linux(&from, &to, &mut |_| {}),
            Err(Error::UnsupportedEntry(path)) if path == fifo
        ));
    }

    #[test]
    fn native_fallback_removes_populated_tree() {
        let temp = TempDir::new().unwrap();
        let tree = temp.path().join("tree");
        fs::create_dir(&tree).unwrap();
        fs::create_dir(tree.join("nested")).unwrap();
        fs::write(tree.join("nested/file.txt"), "hello").unwrap();
        remove_emptyable_subvolume(&tree).unwrap();
        assert!(!tree.exists());
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
