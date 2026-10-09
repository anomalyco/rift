use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_RETRIEVAL_POINTERS_AND_REFCOUNT, STARTING_VCN_INPUT_BUFFER,
};

pub(crate) fn assert_shared_extents(source: &Path, clone: &Path) {
    for path in [source, clone] {
        let counts = reference_counts(path).unwrap();
        assert!(
            counts.iter().any(|count| *count > 1),
            "{} has no extent shared with another file: reference counts {counts:?}",
            path.display()
        );
    }
}

fn reference_counts(path: &Path) -> io::Result<Vec<u32>> {
    let file = File::open(path)?;
    let input = STARTING_VCN_INPUT_BUFFER { StartingVcn: 0 };
    let mut output = vec![0_u64; 4096];
    let mut returned = 0;
    // SAFETY: `input` and `output` live for the call with the sizes passed, and the handle is
    // synchronous, so the call finishes before it returns.
    let succeeded = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_GET_RETRIEVAL_POINTERS_AND_REFCOUNT,
            (&raw const input).cast(),
            size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
            output.as_mut_ptr().cast(),
            (output.len() * size_of::<u64>()) as u32,
            &mut returned,
            null_mut(),
        )
    };
    if succeeded == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
            return Err(error);
        }
    }
    // RETRIEVAL_POINTERS_AND_REFCOUNT_BUFFER: a u32 extent count, the starting VCN, then
    // extents of { NextVcn: i64, Lcn: i64, ReferenceCount: u32 } padded to three words.
    let extents = (output[0] & u64::from(u32::MAX)) as usize;
    Ok(output[2..]
        .as_chunks::<3>()
        .0
        .iter()
        .take(extents)
        .filter(|[_, lcn, _]| *lcn as i64 != -1)
        .map(|[_, _, count]| *count as u32)
        .collect())
}
