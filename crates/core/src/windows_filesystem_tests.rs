use crate::strategy::default_strategy;
use crate::{CopyMode, Error, Manager};
use std::fs;
use std::path::Path;
use tempfile::{Builder, TempDir};

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
        Err(Error::CowUnavailable(message)) => assert_dev_drive_guidance(&message),
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
