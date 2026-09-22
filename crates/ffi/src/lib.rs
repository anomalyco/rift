use std::ffi::{CStr, CString, c_char};

unsafe fn response(input: *const c_char) -> String {
    if input.is_null() {
        return rift::rpc::error("invalid_request", "rift_ffi_call received a null request");
    }
    // SAFETY: null was checked above. The caller promises any non-null input
    // points to a valid null-terminated request buffer for this call.
    let input = unsafe { CStr::from_ptr(input) };
    match input.to_str() {
        Ok(input) => rift::rpc::call(input),
        Err(error) => rift::rpc::error("invalid_request", error.to_string()),
    }
}

/// # Safety
///
/// If `input` is non-null, it must point to a valid null-terminated byte
/// buffer for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rift_ffi_call(input: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from `rift_ffi_call`'s caller contract.
    response_string_into_raw(unsafe { response(input) })
}

fn response_string_into_raw(output: String) -> *mut c_char {
    match CString::new(output) {
        Ok(output) => output.into_raw(),
        Err(_) => c"{\"status\":\"error\",\"error\":{\"code\":\"serialization\",\"message\":\"response contained an interior null byte\"}}"
            .to_owned()
            .into_raw(),
    }
}

/// # Safety
///
/// `output` must be a pointer previously returned by `rift_ffi_call` that has
/// not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rift_ffi_free(output: *mut c_char) {
    if !output.is_null() {
        // SAFETY: the caller promises `output` came from `CString::into_raw`
        // in `rift_ffi_call`, and this function takes back ownership once.
        unsafe {
            drop(CString::from_raw(output));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_allocation_handles_interior_nulls() {
        let output = response_string_into_raw("bad\0json".into());
        // SAFETY: `response_string_into_raw` returns a valid C string pointer
        // that remains allocated until `rift_ffi_free` takes it back below.
        let response = unsafe { CStr::from_ptr(output).to_string_lossy().into_owned() };

        assert!(response.contains("interior null byte"));

        // SAFETY: `output` came from `response_string_into_raw` and has not
        // been freed yet.
        unsafe {
            rift_ffi_free(output);
        }
    }
}
