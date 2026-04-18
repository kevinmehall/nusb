use std::{
    borrow::Borrow,
    ffi::{OsStr, OsString},
    fmt::{Display, Write},
    ops::Deref,
    os::windows::prelude::{
        AsHandle, AsRawHandle, HandleOrInvalid, OsStrExt, OsStringExt, OwnedHandle, RawHandle,
    },
    ptr::{self, null},
    slice,
};

use windows_sys::Win32::{
    Foundation::{GetLastError, GENERIC_READ, GENERIC_WRITE, HANDLE, WIN32_ERROR},
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
};

pub const DEFAULT_TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Wrapper around `CreateFile`
pub fn create_file(path: &WCStr) -> Result<OwnedHandle, WIN32_ERROR> {
    unsafe {
        let r = CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            ptr::null_mut(),
        );
        HandleOrInvalid::from_raw_handle(r as RawHandle)
            .try_into()
            .map_err(|_| GetLastError())
    }
}

pub fn raw_handle(h: impl AsHandle) -> HANDLE {
    h.as_handle().as_raw_handle() as HANDLE
}

/// A utf-16 owned null-terminated string
#[repr(transparent)]
pub struct WCString(Vec<u16>);

impl From<&OsStr> for WCString {
    fn from(s: &OsStr) -> Self {
        WCString(s.encode_wide().chain(Some(0)).collect())
    }
}

impl Borrow<WCStr> for WCString {
    fn borrow(&self) -> &WCStr {
        self
    }
}

impl Deref for WCString {
    type Target = WCStr;

    fn deref(&self) -> &Self::Target {
        unsafe { WCStr::from_slice_unchecked(&self.0) }
    }
}

impl From<WCString> for OsString {
    fn from(s: WCString) -> Self {
        (&*s).into()
    }
}

impl Display for WCString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.deref().fmt(f)
    }
}

/// A utf-16 borrowed null-terminated string
#[repr(transparent)]
pub struct WCStr([u16]);

impl WCStr {
    pub unsafe fn from_ptr<'a>(ptr: *const u16) -> &'a WCStr {
        let mut len = 0;
        while ptr.add(len).read() != 0 {
            len += 1;
        }
        Self::from_slice_unchecked(slice::from_raw_parts(ptr, len + 1))
    }

    unsafe fn from_slice_unchecked(s: &[u16]) -> &WCStr {
        debug_assert_eq!(
            s.last().copied(),
            Some(0),
            "string should be null-terminated"
        );
        let p: *const [u16] = s;
        unsafe { &*(p as *const WCStr) }
    }

    pub fn from_slice_until_nul(s: &[u16]) -> &WCStr {
        let nul = s
            .iter()
            .copied()
            .position(|x| x == 0)
            .expect("string should be null-terminated");
        unsafe { Self::from_slice_unchecked(&s[..nul + 1]) }
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.0
    }

    pub fn as_slice_without_nul(&self) -> &[u16] {
        &self.0[..self.0.len() - 1]
    }

    pub fn as_ptr(&self) -> *const u16 {
        self.0.as_ptr()
    }
}

impl ToOwned for WCStr {
    type Owned = WCString;

    fn to_owned(&self) -> Self::Owned {
        WCString(self.0.to_owned())
    }
}

impl From<&WCStr> for OsString {
    fn from(s: &WCStr) -> Self {
        debug_assert_eq!(
            s.0.last().copied(),
            Some(0),
            "string should be null-terminated"
        );
        OsString::from_wide(&s.0[..s.0.len() - 1])
    }
}

impl Display for WCStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.as_slice_without_nul();
        for c in char::decode_utf16(s.iter().copied()) {
            f.write_char(c.unwrap_or(char::REPLACEMENT_CHARACTER))?;
        }
        Ok(())
    }
}

pub struct NulSepList(pub Vec<u16>);

impl NulSepList {
    pub fn iter(&self) -> NulSepListIter {
        NulSepListIter(&self.0)
    }
}

pub struct NulSepListIter<'a>(pub &'a [u16]);

impl<'a> Iterator for NulSepListIter<'a> {
    type Item = &'a WCStr;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(next_nul) = self.0.iter().copied().position(|x| x == 0) {
            let (i, next) = self.0.split_at(next_nul + 1);
            self.0 = next;

            if i.len() <= 1 {
                // Empty element (double `\0`) terminates the list
                None
            } else {
                Some(unsafe { WCStr::from_slice_unchecked(i) })
            }
        } else {
            None
        }
    }
}
