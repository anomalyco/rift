use crate::strategy::default_strategy;
use crate::test_support::windows_extents::assert_shared_extents;
use crate::{CopyMode, Create, Error, InitOutcome, Manager};
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;
use tempfile::{Builder, TempDir};

const LARGE_FILE_SIZE: usize = (1 << 20) + 123;

#[test]
fn production_refs_volume_round_trip() {
    if !requires_refs_tests() {
        return;
    }
    let temp = current_volume_temp();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("nested/deeper")).unwrap();
    fs::write(source.join("empty.txt"), "").unwrap();
    fs::write(source.join("nested/deeper/leaf.txt"), "leaf").unwrap();
    let large = pseudo_random(LARGE_FILE_SIZE, 0x5eed);
    fs::write(source.join("large.bin"), &large).unwrap();
    let mut manager = Manager::open(temp.path().join("registry.sqlite")).unwrap();

    assert_eq!(manager.init(&source).unwrap(), InitOutcome::Registered);
    assert_no_probe_files(&source);
    let started = Instant::now();
    let child = manager
        .create(Create::new(source.clone()).named("child"))
        .unwrap();
    let elapsed = started.elapsed();
    assert_no_probe_files(child.parent().unwrap());

    assert_eq!(fs::read(child.join("empty.txt")).unwrap(), b"");
    assert_eq!(
        fs::read_to_string(child.join("nested/deeper/leaf.txt")).unwrap(),
        "leaf"
    );
    assert!(
        fs::read(child.join("large.bin")).unwrap() == large,
        "large.bin differs in the clone"
    );
    assert_shared_extents(&source.join("large.bin"), &child.join("large.bin"));
    assert_isolated(&source.join("large.bin"), &child.join("large.bin"));
    println!(
        "refs round trip: created {} in {elapsed:?}",
        child.display()
    );
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
