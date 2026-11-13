use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    println!("cargo:rerun-if-changed=../../.git/index");
    let read = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| value.trim().to_owned())
    };
    let commit = read(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unavailable".into());
    let dirty = read(&["status", "--porcelain", "--untracked-files=normal"])
        .is_none_or(|status| !status.is_empty());
    println!("cargo:rustc-env=RIFT_SOURCE_COMMIT={commit}");
    println!("cargo:rustc-env=RIFT_SOURCE_DIRTY={dirty}");
}
