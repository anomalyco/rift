#[cfg(target_os = "linux")]
pub(crate) mod linux_extents;
#[cfg(windows)]
pub(crate) mod windows_extents;
