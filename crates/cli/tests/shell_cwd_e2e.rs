#[cfg(unix)]
#[allow(dead_code)]
mod support;

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use support::filesystem::CliFixture;
#[cfg(target_os = "linux")]
use support::filesystem::supported_linux_filesystem_tests_required;

/// Unregistering a root needs no copy, so this runs on every filesystem.
#[cfg(unix)]
#[test]
fn hook_stdout_does_not_reach_shell_cwd_output() {
    let fixture = CliFixture::current_filesystem(".rift-cli-hook-stdout-");
    let source = fixture.root().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join(".rift.toml"),
        "version = 1\n[[hooks.postremove]]\nrun = \"printf hook-output\"\n",
    )
    .unwrap();
    fixture.success(&source, ["init", "--here"]);

    let remove = fixture.success(&source, ["--shell-cwd", "remove", "-f"]);

    assert_eq!(remove.single_stdout_path(), canonical(&source));
    assert!(remove.stderr.contains("hook-output"));
}

/// Creating and removing a child needs copy-on-write, so this only runs on the
/// prepared Linux filesystems in CI.
#[cfg(target_os = "linux")]
#[test]
fn shell_cwd_leaves_removed_child_after_failed_postremove() {
    if !supported_linux_filesystem_tests_required() {
        return;
    }
    let fixture = CliFixture::current_filesystem(".rift-cli-postremove-");
    let source = fixture.root().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join(".rift.toml"),
        "version = 1\n[[hooks.postcreate]]\nrun = \"printf hook-output\"\n",
    )
    .unwrap();
    fixture.success(&source, ["init", "--here"]);

    let child = fixture
        .success(&source, ["--shell-cwd", "create", "--name", "child"])
        .single_stdout_path();
    assert!(child.exists());

    fs::write(
        child.join(".rift.toml"),
        "version = 1\n[[hooks.postremove]]\nrun = \"exit 23\"\n",
    )
    .unwrap();
    let remove = fixture.failure(&child, ["--shell-cwd", "remove"]);

    assert_eq!(remove.single_stdout_path(), canonical(&source));
    assert!(!child.exists());
}

#[cfg(unix)]
fn canonical(path: &Path) -> std::path::PathBuf {
    fs::canonicalize(path).unwrap()
}
