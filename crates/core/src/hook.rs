use crate::config::{Postcreate, Postremove};
use crate::id::RiftId;
use crate::{Error, Result};
use std::path::Path;
use std::process::Command;

pub(crate) fn run_postcreate(
    steps: &[Postcreate],
    source: &Path,
    destination: &Path,
    id: &RiftId,
    parent_id: &RiftId,
) -> Result<()> {
    steps
        .iter()
        .map(Postcreate::run)
        .try_for_each(|command| run_step(command, source, destination, id, parent_id))
}

pub(crate) fn run_postremove(
    steps: &[Postremove],
    source: &Path,
    destination: &Path,
    id: &RiftId,
    parent_id: &RiftId,
) -> Result<()> {
    steps
        .iter()
        .map(Postremove::run)
        .try_for_each(|command| run_step(command, source, destination, id, parent_id))
}

fn run_step(
    command: &str,
    source: &Path,
    destination: &Path,
    id: &RiftId,
    parent_id: &RiftId,
) -> Result<()> {
    let status = shell_command(command)
        .current_dir(destination)
        .env("RIFT_SOURCE", source)
        .env("RIFT_DESTINATION", destination)
        .env("RIFT_ID", id.as_str())
        .env("RIFT_PARENT_ID", parent_id.as_str())
        .status()
        .map_err(|error| hook_failed(destination, command, format!("failed to start: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(hook_failed(
            destination,
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

fn hook_failed(path: &Path, command: &str, message: String) -> Error {
    Error::HookFailed {
        path: path.to_path_buf(),
        command: command.to_owned(),
        message,
    }
}
