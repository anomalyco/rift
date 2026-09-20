#![cfg(unix)]
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use support::filesystem::{CliFixture, supported_linux_filesystem_tests_required};

/// `init` probes for copy-on-write support, so these need APFS or one of the
/// prepared Linux filesystems in CI.
fn cow_filesystem_available() -> bool {
    cfg!(target_os = "macos") || supported_linux_filesystem_tests_required()
}

#[test]
fn hook_stdout_does_not_reach_shell_cwd_output() {
    if !cow_filesystem_available() {
        return;
    }
    let fixture = CliFixture::current_filesystem(".rift-cli-hook-stdout-");
    let source = fixture.root().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join(".rift.toml"),
        "version = 1\n[[hooks.postcreate]]\nrun = \"printf hook-output\"\n",
    )
    .unwrap();
    fixture.success(&source, ["init", "--here"]);

    let create = fixture.success(&source, ["--shell-cwd", "create", "--name", "child"]);

    assert_eq!(
        create.single_stdout_path(),
        canonical(fixture.root()).join(".rifts/source/child")
    );
    assert!(create.stderr.contains("hook-output"));
}

#[test]
fn shell_cwd_leaves_removed_child_after_failed_postremove() {
    if !cow_filesystem_available() {
        return;
    }
    let (fixture, source, child) = source_with_child(".rift-cli-postremove-");
    fs::write(child.join(".rift.toml"), FAILING_POSTREMOVE).unwrap();

    let remove = fixture.failure(&child, ["--shell-cwd", "remove"]);

    assert_eq!(remove.single_stdout_path(), canonical(&source));
    assert!(!child.exists());
}

#[test]
fn shell_cwd_leaves_removed_children_after_failed_postremove() {
    if !cow_filesystem_available() {
        return;
    }
    let (fixture, source, child) = source_with_child(".rift-cli-postremove-children-");
    fs::write(source.join(".rift.toml"), FAILING_POSTREMOVE).unwrap();

    let remove = fixture.failure(
        &child,
        [
            "--shell-cwd",
            "remove",
            "--children",
            source.to_str().unwrap(),
        ],
    );

    assert_eq!(remove.single_stdout_path(), canonical(&source));
    assert!(!child.exists());
}

const FAILING_POSTREMOVE: &str = "version = 1\n[[hooks.postremove]]\nrun = \"exit 23\"\n";

fn source_with_child(prefix: &str) -> (CliFixture, PathBuf, PathBuf) {
    let fixture = CliFixture::current_filesystem(prefix);
    let source = fixture.root().join("source");
    fs::create_dir_all(&source).unwrap();
    fixture.success(&source, ["init", "--here"]);
    let child = fixture
        .success(&source, ["create", "--name", "child"])
        .single_stdout_path();
    (fixture, source, child)
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap()
}
