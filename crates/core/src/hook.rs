use crate::config::Hook;
use crate::id::RiftId;
use crate::{Error, Result};
use std::path::Path;
use std::process::Command;

pub(crate) fn run(
    name: &str,
    steps: &[Hook],
    current_dir: &Path,
    source: &Path,
    destination: &Path,
    id: &RiftId,
    parent_id: &RiftId,
) -> Result<()> {
    steps.iter().map(Hook::run).try_for_each(|command| {
        run_step(
            name,
            command,
            current_dir,
            source,
            destination,
            id,
            parent_id,
        )
    })
}

fn run_step(
    name: &str,
    command: &str,
    current_dir: &Path,
    source: &Path,
    destination: &Path,
    id: &RiftId,
    parent_id: &RiftId,
) -> Result<()> {
    let status = shell_command(command)
        .current_dir(current_dir)
        .env("RIFT_SOURCE", source)
        .env("RIFT_DESTINATION", destination)
        .env("RIFT_ID", id.as_str())
        .env("RIFT_PARENT_ID", parent_id.as_str())
        .status()
        .map_err(|error| {
            hook_failed(
                name,
                current_dir,
                command,
                format!("failed to start: {error}"),
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(hook_failed(
            name,
            current_dir,
            command,
            format!("exited with {status}"),
        ))
    }
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.args(["/C", command]);
    shell
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.args(["-c", command]);
    shell
}

fn hook_failed(name: &str, path: &Path, command: &str, message: String) -> Error {
    Error::HookFailed {
        hook: name.to_owned(),
        path: path.to_path_buf(),
        command: command.to_owned(),
        message,
    }
}
