use std::{
    ffi::{OsStr, OsString},
    os::windows::prelude::{
        AsHandle, AsRawHandle, HandleOrInvalid, OsStrExt, OsStringExt, OwnedHandle, RawHandle,
    },
    ptr::null,
};

use windows_sys::Win32::{
    Foundation::{GetLastError, GENERIC_WRITE, HANDLE, WIN32_ERROR},
    Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_WRITE, OPEN_EXISTING},
};

/// Wrapper around `CreateFile`
pub fn create_file(path: &OsStr) -> Result<OwnedHandle, WIN32_ERROR> {
    let wide_name: Vec<u16> = path.encode_wide().chain(Some(0)).collect();

    unsafe {
        let r = CreateFileW(
            wide_name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            0,
        );
        HandleOrInvalid::from_raw_handle(r as RawHandle)
            .try_into()
            .map_err(|_| GetLastError())
    }
}

pub fn raw_handle(h: impl AsHandle) -> HANDLE {
    h.as_handle().as_raw_handle() as HANDLE
}

pub fn from_wide_with_nul(s: &[u16]) -> OsString {
    assert_eq!(
        s.last().copied(),
        Some(0),
        "string should be null-terminated"
    );
    OsString::from_wide(&s[..s.len() - 1])
}
