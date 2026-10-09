use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_RETRIEVAL_POINTERS_AND_REFCOUNT, RETRIEVAL_POINTERS_AND_REFCOUNT_BUFFER,
    RETRIEVAL_POINTERS_AND_REFCOUNT_BUFFER_0, STARTING_VCN_INPUT_BUFFER,
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
    // SAFETY: `output` is aligned for the ioctl structs and holds the reply. The slice length
    // stays inside that allocation.
    let extents = unsafe {
        let header = output
            .as_ptr()
            .cast::<RETRIEVAL_POINTERS_AND_REFCOUNT_BUFFER>();
        let count = (*header).ExtentCount as usize;
        let extents_at = std::mem::offset_of!(RETRIEVAL_POINTERS_AND_REFCOUNT_BUFFER, Extents);
        let available = (output.len() * size_of::<u64>()).saturating_sub(extents_at)
            / size_of::<RETRIEVAL_POINTERS_AND_REFCOUNT_BUFFER_0>();
        std::slice::from_raw_parts(
            output
                .as_ptr()
                .cast::<u8>()
                .add(extents_at)
                .cast::<RETRIEVAL_POINTERS_AND_REFCOUNT_BUFFER_0>(),
            count.min(available),
        )
    };
    Ok(extents
        .iter()
        .filter(|extent| extent.Lcn != -1)
        .map(|extent| extent.ReferenceCount)
        .collect())
}
