use crate::{CopyMode, InitProgress, Result};
#[cfg(any(test, not(any(target_os = "linux", target_os = "macos"))))]
use crate::{Error, filter::CopyFilter};
use std::fs;
use std::path::Path;

#[cfg(target_os = "macos")]
mod apfs;
#[cfg(target_os = "linux")]
mod btrfs;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod reflink;

pub(crate) trait Strategy {
    fn copy_directory(&self, from: &Path, to: &Path, mode: CopyMode) -> Result<()>;

    fn initialize_directory(
        &self,
        _path: &Path,
        _progress: &mut dyn FnMut(InitProgress),
    ) -> Result<StrategyInit> {
        Ok(StrategyInit::AlreadyNative)
    }

    fn remove_directory(&self, path: &Path) -> Result<()> {
        fs::remove_dir_all(path)?;
        Ok(())
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StrategyInit {
    AlreadyNative,
    Converted,
}

pub(crate) fn default_strategy() -> Box<dyn Strategy> {
    #[cfg(target_os = "linux")]
    return Box::new(linux::LinuxStrategy);

    #[cfg(target_os = "macos")]
    return Box::new(apfs::ApfsStrategy);

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Box::new(UnsupportedStrategy);
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct UnsupportedStrategy;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Strategy for UnsupportedStrategy {
    fn copy_directory(&self, _from: &Path, _to: &Path, _mode: CopyMode) -> Result<()> {
        Err(Error::CowUnavailable(
            "no copy-on-write strategy has been implemented for this platform".into(),
        ))
    }
}

#[cfg(all(test, unix))]
fn copy_symlink(from: &Path, to: &Path) -> Result<()> {
    std::os::unix::fs::symlink(fs::read_link(from)?, to)?;
    Ok(())
}

#[cfg(all(test, windows))]
fn copy_symlink(from: &Path, to: &Path) -> Result<()> {
    let target = fs::read_link(from)?;
    if fs::metadata(from)?.is_dir() {
        std::os::windows::fs::symlink_dir(target, to)?;
        return Ok(());
    }
    std::os::windows::fs::symlink_file(target, to)?;
    Ok(())
}

#[cfg(test)]
pub(crate) struct TestStrategy;

#[cfg(test)]
impl Strategy for TestStrategy {
    fn copy_directory(&self, from: &Path, to: &Path, mode: CopyMode) -> Result<()> {
        fs::create_dir(to)?;
        let filter = CopyFilter::for_source(from);
        for entry in walkdir::WalkDir::new(from)
            .min_depth(1)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                mode == CopyMode::All
                    || entry
                        .path()
                        .strip_prefix(from)
                        .map_or(true, |path| !filter.excludes(path))
            })
        {
            let entry = entry?;
            let destination = to.join(
                entry
                    .path()
                    .strip_prefix(from)
                    .map_err(|error| Error::Path(error.to_string()))?,
            );
            if entry.file_type().is_dir() {
                fs::create_dir(&destination)?;
                continue;
            }
            if entry.file_type().is_symlink() {
                copy_symlink(entry.path(), &destination)?;
                continue;
            }
            fs::copy(entry.path(), destination)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) struct FailureStrategy;

#[cfg(test)]
impl Strategy for FailureStrategy {
    fn copy_directory(&self, _from: &Path, _to: &Path, _mode: CopyMode) -> Result<()> {
        Err(Error::CowUnavailable("test failure".into()))
    }
}
