#![cfg(unix)]
mod support;

use std::fs;
use support::filesystem::{CliFixture, supported_linux_filesystem_tests_required};

#[test]
fn shell_cwd_survives_hook_output_and_failure() {
    // `init` probes for copy-on-write support, so this needs APFS or one of
    // the prepared Linux filesystems in CI.
    if !(cfg!(target_os = "macos") || supported_linux_filesystem_tests_required()) {
        return;
    }
    let fixture = CliFixture::current_filesystem(".rift-cli-shell-cwd-");
    let source = fixture.root().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join(".rift.toml"),
        "version = 1\n[[hooks.postcreate]]\nrun = \"printf hook-output\"\n",
    )
    .unwrap();
    fixture.success(&source, ["init", "--here"]);

    let create = fixture.success(&source, ["--shell-cwd", "create", "--name", "child"]);
    let child = create.single_stdout_path();
    assert_eq!(
        child,
        fs::canonicalize(fixture.root())
            .unwrap()
            .join(".rifts/source/child")
    );
    assert!(create.stderr.contains("hook-output"));

    fs::write(
        child.join(".rift.toml"),
        "version = 1\n[[hooks.postremove]]\nrun = \"exit 23\"\n",
    )
    .unwrap();
    let remove = fixture.failure(&child, ["--shell-cwd", "remove"]);
    assert_eq!(
        remove.single_stdout_path(),
        fs::canonicalize(&source).unwrap()
    );
    assert!(!child.exists());
}
