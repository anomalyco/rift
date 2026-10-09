use crate::strategy::default_strategy;
use crate::test_support::windows_extents::assert_shared_extents;
use crate::{CopyMode, Create, Error, InitOutcome, Manager};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::windows::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, symlink_dir, symlink_file};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::time::Instant;
use tempfile::{Builder, TempDir};
use walkdir::WalkDir;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FileIdInfo,
    GetFileInformationByHandleEx, REPARSE_GUID_DATA_BUFFER, REPARSE_GUID_DATA_BUFFER_0,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
use windows_sys::core::GUID;

const LARGE_FILE_SIZE: usize = (1 << 20) + 123;

// create rewrites the marker and detaches HEAD after copying.
const REWRITTEN_BY_CREATE: [&str; 2] = [".rift", ".git/HEAD"];

#[test]
fn production_refs_volume_round_trip() {
    if !requires_refs_tests() {
        return;
    }
    let temp = current_volume_temp();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "outside").unwrap();
    let source = rich_git_workspace(temp.path(), &outside);
    let mut manager = Manager::open(temp.path().join("registry.sqlite")).unwrap();

    assert_eq!(manager.init(&source).unwrap(), InitOutcome::Registered);
    assert_no_probe_files(&source);
    refresh_git_index(&source);
    assert_eq!(
        git_output(&source, &["diff-files", "--name-only"]),
        "dirty.txt\n"
    );
    let started = Instant::now();
    let child = manager
        .create(Create::new(source.clone()).named("child"))
        .unwrap();
    let elapsed = started.elapsed();
    assert_no_probe_files(child.parent().unwrap());

    assert_same_tree(&source, &child);
    assert!(!child.join("node_modules").exists());
    assert_eq!(
        fs::read_to_string(stream(&child.join("streams.txt"), "rift.test")).unwrap(),
        "alternate stream"
    );
    assert_eq!(
        file_id(&child.join("nested/file.txt")),
        file_id(&child.join("nested/hard.txt"))
    );
    assert_ne!(
        file_id(&child.join("nested/file.txt")),
        file_id(&source.join("nested/file.txt"))
    );
    assert!(
        fs::symlink_metadata(child.join("nested/link.txt"))
            .unwrap()
            .file_type()
            .is_symlink_file()
    );
    assert!(
        fs::symlink_metadata(child.join("dirlink"))
            .unwrap()
            .file_type()
            .is_symlink_dir()
    );
    assert_eq!(
        git_output(&child, &["diff-files", "--name-only"]),
        "dirty.txt\n"
    );
    assert_detached_git_copy(&source, &child);
    assert_shared_extents(&source.join("large.bin"), &child.join("large.bin"));
    assert_isolated(&source.join("large.bin"), &child.join("large.bin"));
    println!(
        "refs round trip: created {} with {} entries in {elapsed:?}",
        child.display(),
        WalkDir::new(&child).into_iter().count()
    );
}

#[test]
fn production_refs_create_rejects_unknown_reparse_points() {
    if !requires_refs_tests() {
        return;
    }
    let temp = current_volume_temp();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("plain.txt"), "plain").unwrap();
    let placeholder = source.join("placeholder.dat");
    fs::write(&placeholder, "placeholder").unwrap();
    set_third_party_reparse_point(&placeholder);
    let mut manager = Manager::open(temp.path().join("registry.sqlite")).unwrap();
    manager.init(&source).unwrap();

    match manager.create(Create::new(source.clone()).named("child")) {
        Err(Error::UnsupportedEntry(path)) => {
            assert!(path.ends_with("placeholder.dat"), "{}", path.display());
        }
        result => panic!("create should refuse the reparse point, got {result:?}"),
    }
    assert!(!temp.path().join(".rifts/source/child").exists());
    assert!(manager.list(&source).unwrap().is_empty());
    println!("refs create refused an unknown reparse tag and removed the partial child");
}

#[test]
fn production_unsupported_windows_volume_rejects_management() {
    if std::env::var_os("RIFT_REQUIRE_UNSUPPORTED_WINDOWS_TESTS").is_none() {
        return;
    }
    let temp = current_volume_temp();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file.txt"), "hello").unwrap();
    let mut manager = Manager::open(temp.path().join("registry.sqlite")).unwrap();

    match manager.init(&source) {
        Err(Error::CowUnavailable(message)) => {
            assert_dev_drive_guidance(&message);
            println!("init on NTFS: {message}");
        }
        result => panic!("init should fail closed on NTFS, got {result:?}"),
    }
    assert!(!source.join(".rift").exists());
    assert_no_probe_files(&source);
    assert!(manager.registry.active_paths().unwrap().is_empty());

    let storage = temp.path().join("storage");
    fs::create_dir(&storage).unwrap();
    let destination = storage.join("child");
    match default_strategy().copy_directory(&source, &destination, CopyMode::Filtered) {
        Err(Error::CowUnavailable(message)) => assert_dev_drive_guidance(&message),
        result => panic!("copy should fail closed on NTFS, got {result:?}"),
    }
    assert!(!destination.exists());
    assert_no_probe_files(&storage);
}

fn requires_refs_tests() -> bool {
    std::env::var_os("RIFT_REQUIRE_REFS_TESTS").is_some()
}

fn current_volume_temp() -> TempDir {
    Builder::new()
        .prefix(".rift-manager-test-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap()
}

fn assert_dev_drive_guidance(message: &str) {
    assert!(
        message.contains(" is on NTFS,")
            && message.contains("needs a ReFS volume such as a Dev Drive")
            && message.contains("https://learn.microsoft.com/windows/dev-drive/")
            && message.contains("move the project onto one"),
        "unexpected guidance: {message}"
    );
}

fn rich_git_workspace(root: &Path, outside: &Path) -> PathBuf {
    let source = root.join("source");
    let nested = source.join("nested");
    let deeper = nested.join("deeper");
    fs::create_dir_all(&deeper).unwrap();
    fs::write(deeper.join("leaf.txt"), "leaf").unwrap();
    fs::write(nested.join("file.txt"), "hello").unwrap();
    fs::hard_link(nested.join("file.txt"), nested.join("hard.txt")).unwrap();
    fs::write(source.join("empty.txt"), "").unwrap();
    fs::write(
        source.join("large.bin"),
        pseudo_random(LARGE_FILE_SIZE, 0x5eed),
    )
    .unwrap();
    fs::write(source.join("readonly.txt"), "read only").unwrap();
    let mut permissions = fs::metadata(source.join("readonly.txt"))
        .unwrap()
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(source.join("readonly.txt"), permissions).unwrap();
    fs::write(source.join("hidden.txt"), "hidden").unwrap();
    run(
        "attrib",
        &["+h".as_ref(), source.join("hidden.txt").as_os_str()],
    );
    fs::write(source.join("streams.txt"), "main stream").unwrap();
    fs::write(
        stream(&source.join("streams.txt"), "rift.test"),
        "alternate stream",
    )
    .unwrap();
    fs::write(source.join("ünïcødé-文件.txt"), "unicode").unwrap();
    let long = source
        .join("long")
        .join("a".repeat(120))
        .join("b".repeat(120));
    fs::create_dir_all(&long).unwrap();
    fs::write(long.join("file.txt"), "long path").unwrap();
    fs::create_dir_all(source.join("node_modules/pkg")).unwrap();
    fs::write(source.join("node_modules/pkg/index.js"), "module").unwrap();
    fs::write(source.join(".gitignore"), "*.log\nnode_modules/\n").unwrap();
    fs::write(source.join("dirty.txt"), "committed").unwrap();
    fs::write(source.join("staged.txt"), "committed").unwrap();

    git(&source, &["init", "-q"]);
    git(&source, &["config", "user.email", "test@example.com"]);
    git(&source, &["config", "user.name", "Test"]);
    git(&source, &["config", "core.autocrlf", "false"]);
    git(&source, &["config", "core.longpaths", "true"]);
    git(&source, &["add", "."]);
    git(&source, &["commit", "-q", "-m", "initial"]);
    fs::write(source.join("dirty.txt"), "dirty").unwrap();
    fs::write(source.join("staged.txt"), "staged").unwrap();
    git(&source, &["add", "staged.txt"]);
    fs::write(source.join("untracked.txt"), "untracked").unwrap();
    fs::write(source.join("debug.log"), "ignored").unwrap();

    symlink_file("file.txt", nested.join("link.txt")).unwrap();
    symlink_dir("nested", source.join("dirlink")).unwrap();
    junction(&source.join("junction-inside"), &deeper);
    junction(&source.join("junction-outside"), outside);
    source
}

fn assert_same_tree(source: &Path, clone: &Path) {
    let (expected, actual) = (tree(source), tree(clone));
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "the clone has different entries"
    );
    for (path, metadata) in &expected {
        let copied = &actual[path];
        assert_eq!(
            (
                copied.file_attributes(),
                copied.creation_time(),
                copied.last_write_time()
            ),
            (
                metadata.file_attributes(),
                metadata.creation_time(),
                metadata.last_write_time()
            ),
            "attributes or times differ for {}",
            path.display()
        );
        if metadata.file_type().is_symlink() {
            assert_eq!(
                fs::read_link(clone.join(path)).unwrap(),
                fs::read_link(source.join(path)).unwrap(),
                "link target differs for {}",
                path.display()
            );
        } else if metadata.is_file() {
            assert!(
                fs::read(clone.join(path)).unwrap() == fs::read(source.join(path)).unwrap(),
                "contents differ for {}",
                path.display()
            );
        }
    }
}

fn tree(root: &Path) -> BTreeMap<PathBuf, fs::Metadata> {
    WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "node_modules")
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.path().strip_prefix(root).unwrap().to_path_buf(),
                fs::symlink_metadata(entry.path()).unwrap(),
            )
        })
        .filter(|(path, _)| {
            !REWRITTEN_BY_CREATE
                .iter()
                .any(|rewritten| path.as_path() == Path::new(rewritten))
        })
        .collect()
}

fn assert_detached_git_copy(source: &Path, child: &Path) {
    let commit = git_output(source, &["rev-parse", "--verify", "HEAD^{commit}"]);
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(child)
            .args(["symbolic-ref", "-q", "HEAD"])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::read_to_string(child.join(".git/HEAD")).unwrap(),
        format!("{}\n", commit.trim())
    );
    assert_eq!(
        git_output(child, &["diff", "--cached", "--name-only"]),
        "staged.txt\n"
    );
}

fn file_id(path: &Path) -> (u64, [u8; 16]) {
    let file = File::open(path).unwrap();
    let mut id = FILE_ID_INFO::default();
    // SAFETY: `id` is a writable FILE_ID_INFO, the layout FileIdInfo expects.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut id).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    assert_ne!(succeeded, 0, "{}", io::Error::last_os_error());
    (id.VolumeSerialNumber, id.FileId.Identifier)
}

fn set_third_party_reparse_point(path: &Path) {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap();
    let reparse = REPARSE_GUID_DATA_BUFFER {
        ReparseTag: 0x1234,
        ReparseDataLength: 1,
        Reserved: 0,
        ReparseGuid: GUID::from_u128(0x5249_4654_0000_4000_8000_0000_0000_0001),
        GenericReparseBuffer: REPARSE_GUID_DATA_BUFFER_0 { DataBuffer: [1] },
    };
    let mut returned = 0;
    // SAFETY: `reparse` holds the 24-byte header and the one data byte passed, and the handle is
    // synchronous, so the call finishes before it returns.
    let succeeded = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            (&raw const reparse).cast(),
            25,
            null_mut(),
            0,
            &mut returned,
            null_mut(),
        )
    };
    assert_ne!(
        succeeded,
        0,
        "setting a third-party reparse point failed: {}",
        io::Error::last_os_error()
    );
}

fn junction(link: &Path, target: &Path) {
    run(
        "cmd",
        &[
            "/c".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            link.as_os_str(),
            target.as_os_str(),
        ],
    );
}

fn stream(path: &Path, name: &str) -> PathBuf {
    let mut path = path.as_os_str().to_owned();
    path.push(":");
    path.push(name);
    path.into()
}

// `update-index --refresh` exits non-zero while dirty.txt has unstaged changes.
fn refresh_git_index(path: &Path) {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["update-index", "-q", "--refresh"])
        .status()
        .unwrap();
}

fn git(path: &Path, args: &[&str]) {
    git_output(path, args);
}

fn git_output(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn run(program: &str, args: &[&std::ffi::OsStr]) {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_isolated(source: &Path, clone: &Path) {
    let original = fs::read(source).unwrap();
    overwrite(source, 4096, b"parent mutation");
    assert!(
        fs::read(clone).unwrap() == original,
        "writing {} changed its clone",
        source.display()
    );
    overwrite(clone, 8192, b"child mutation");
    let mut mutated = original;
    mutated[4096..4096 + 15].copy_from_slice(b"parent mutation");
    assert!(
        fs::read(source).unwrap() == mutated,
        "writing {} changed its source",
        clone.display()
    );
}

fn overwrite(path: &Path, offset: u64, bytes: &[u8]) {
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(bytes).unwrap();
}

fn pseudo_random(length: usize, mut state: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(length + 8);
    while bytes.len() < length {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.extend_from_slice(&state.to_le_bytes());
    }
    bytes.truncate(length);
    bytes
}

fn assert_no_probe_files(path: &Path) {
    let leftovers = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(".rift-refs-probe"))
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "probe files left in {}: {leftovers:?}",
        path.display()
    );
}
