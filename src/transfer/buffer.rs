use std::{
    fmt::Debug,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::Arc,
};

pub(crate) enum Allocator {
    Default,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Mmap,
    External(Arc<dyn ExternalBufferOwner>),
}

/// Owns memory exposed as a [`Buffer`] by an external raw-USB backend.
///
/// The owner keeps the allocation mapped and returns it to its producer when the buffer is
/// dropped. This permits a completion to refer directly to shared memory.
pub trait ExternalBufferOwner: Send + Sync {
    /// Release one externally allocated buffer.
    ///
    /// # Safety
    ///
    /// `ptr` and `capacity` are the exact values previously passed to
    /// [`Buffer::from_external_parts`].
    unsafe fn release(&self, ptr: NonNull<u8>, capacity: usize);
}

/// Buffer for bulk and interrupt transfers.
///
/// The fixed-capacity buffer can be backed either by the system allocator or a
/// platform-specific way of allocating memory for zero-copy transfers.
///
/// * For OUT transfers, fill the buffer with data prior to submitting it.
///   The `len` is the number of initialized bytes which will be sent when
///   submitted. `requested_len` is not used.
///
/// * For IN transfers, the `requested_len` specifies the number of bytes
///   requested from the device. It must be a multiple of the endpoint's maximum
///   packet size. The `len` and contents are ignored when the transfer is
///   submitted. When the transfer is completed, the `len` is set to the number
///   of bytes actually received. The `requested_len` is unmodified, so the same
///   buffer can be submitted again to perform another transfer of the same
///   length.
pub struct Buffer {
    /// Data pointer
    pub(crate) ptr: *mut u8,

    /// Initialized bytes
    pub(crate) len: u32,

    /// Requested length for IN transfer
    pub(crate) requested_len: u32,

    /// Allocated memory at `ptr`
    pub(crate) capacity: u32,

    /// Whether the system allocator or a special allocator was used
    pub(crate) allocator: Allocator,
}

impl Buffer {
    /// Allocate a new bufffer with the default allocator.
    ///
    /// This buffer will not support [zero-copy
    /// transfers][`crate::Endpoint::allocate`], but can be cheaply converted to
    /// a `Vec<u8>`.
    ///
    /// The passed size will be used as the `requested_len`, and the `capacity`
    /// will be at least that large.
    ///
    /// ### Panics
    /// * If the requested length is greater than `u32::MAX`.
    #[inline]
    pub fn new(requested_len: usize) -> Self {
        let len_u32 = requested_len.try_into().expect("length overflow");
        let mut vec = ManuallyDrop::new(Vec::with_capacity(requested_len));
        Buffer {
            ptr: vec.as_mut_ptr(),
            len: 0,
            requested_len: len_u32,
            capacity: vec.capacity().try_into().expect("capacity overflow"),
            allocator: Allocator::Default,
        }
    }

    /// Wrap initialized memory owned by an external raw-USB backend.
    ///
    /// The returned buffer reads directly from `ptr`. Dropping it invokes `owner.release` with the
    /// same pointer and capacity.
    ///
    /// # Safety
    ///
    /// `ptr..ptr + capacity` must remain mapped and initialized for reads through the lifetime of
    /// the returned buffer. No writer may modify those bytes until `owner.release` is invoked.
    /// `len` and `requested_len` must not exceed `capacity`.
    pub unsafe fn from_external_parts(
        ptr: NonNull<u8>,
        len: usize,
        requested_len: usize,
        capacity: usize,
        owner: Arc<dyn ExternalBufferOwner>,
    ) -> Self {
        assert!(len <= capacity, "length exceeds capacity");
        assert!(
            requested_len <= capacity,
            "requested length exceeds capacity"
        );
        Buffer {
            ptr: ptr.as_ptr(),
            len: len.try_into().expect("length overflow"),
            requested_len: requested_len.try_into().expect("requested length overflow"),
            capacity: capacity.try_into().expect("capacity overflow"),
            allocator: Allocator::External(owner),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn mmap(
        fd: &std::os::unix::prelude::OwnedFd,
        len: usize,
    ) -> Result<Buffer, rustix::io::Errno> {
        let len_u32 = len.try_into().expect("length overflow");

        let ptr = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
                rustix::mm::MapFlags::SHARED,
                fd,
                0,
            )
        }?;

        Ok(Buffer {
            ptr: ptr as *mut u8,
            len: 0,
            requested_len: len_u32,
            capacity: len_u32,
            allocator: Allocator::Mmap,
        })
    }

    /// Get the number of initialized bytes in the buffer.
    ///
    /// For OUT transfers, this is the amount of data written to the buffer which will be sent when the buffer is submitted.
    /// For IN transfers, this is the amount of data received from the device. This length is updated when the transfer is returned.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns `true` if the buffer is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Requested length for IN transfer or actual length for OUT transfer.
    #[inline]
    pub fn requested_len(&self) -> usize {
        self.requested_len as usize
    }

    /// Set the requested length for an IN transfer.
    ///
    /// ### Panics
    /// * If the requested length is greater than the capacity.
    #[inline]
    pub fn set_requested_len(&mut self, len: usize) {
        assert!(len <= self.capacity as usize, "length exceeds capacity");
        self.requested_len = len.try_into().expect("requested_len overflow");
    }

    /// Number of allocated bytes.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity as usize
    }

    /// Get the number of bytes that can be written to the buffer.
    ///
    /// This is a convenience method for `capacity() - len()`.
    #[inline]
    pub fn remaining_capacity(&self) -> usize {
        self.capacity() - self.len()
    }

    /// Clear the buffer.
    ///
    /// This sets `len` to 0, but does not change the `capacity` or `requested_len`.
    /// This is useful for reusing the buffer for a new transfer.
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Extend the buffer by initializing `len` bytes to `value`, and get a
    /// mutable slice to the newly initialized bytes.
    ///
    /// # Panics
    /// * If the resulting length exceeds the buffer's capacity.
    pub fn extend_fill(&mut self, len: usize, value: u8) -> &mut [u8] {
        assert!(len <= self.remaining_capacity(), "length exceeds capacity");
        unsafe {
            std::ptr::write_bytes(self.ptr.add(self.len()), value, len);
        }
        self.len += len as u32;
        unsafe { std::slice::from_raw_parts_mut(self.ptr.add(self.len() - len), len) }
    }

    /// Append a slice of bytes to the buffer.
    ///
    /// # Panics
    /// * If the resulting length exceeds the buffer's capacity.
    pub fn extend_from_slice(&mut self, slice: &[u8]) {
        assert!(
            slice.len() <= self.remaining_capacity(),
            "length exceeds capacity"
        );
        unsafe {
            std::ptr::copy_nonoverlapping(slice.as_ptr(), self.ptr.add(self.len()), slice.len());
        }
        self.len += slice.len() as u32;
    }

    /// Returns whether the buffer is specially-allocated for zero-copy IO.
    pub fn is_zero_copy(&self) -> bool {
        !matches!(&self.allocator, Allocator::Default)
    }

    /// Convert the buffer into a `Vec<u8>`.
    ///
    /// This is zero-cost if the buffer was allocated with the default allocator
    /// (if [`is_zero_copy()`][Self::is_zero_copy] returns false), otherwise it will copy the data
    /// into a new `Vec<u8>`.
    pub fn into_vec(self) -> Vec<u8> {
        match &self.allocator {
            Allocator::Default => {
                let buf = ManuallyDrop::new(self);
                unsafe { Vec::from_raw_parts(buf.ptr, buf.len as usize, buf.capacity as usize) }
            }
            #[allow(unreachable_patterns)]
            _ => self[..].to_vec(),
        }
    }
}

unsafe impl Send for Buffer {}
unsafe impl Sync for Buffer {}

/// A `Vec<u8>` can be converted to a `Buffer` cheaply.
///
/// The Vec's `len` will be used for both the `len` and `requested_len`.
impl From<Vec<u8>> for Buffer {
    fn from(vec: Vec<u8>) -> Self {
        let mut vec = ManuallyDrop::new(vec);
        Buffer {
            ptr: vec.as_mut_ptr(),
            len: vec.len().try_into().expect("len overflow"),
            requested_len: vec.len().try_into().expect("len overflow"),
            capacity: vec.capacity().try_into().expect("capacity overflow"),
            allocator: Allocator::Default,
        }
    }
}

/// Convenience implementation for copying contents of
/// slice into a `Vec`, then creating a `Buffer` from that.
impl From<&[u8]> for Buffer {
    fn from(slice: &[u8]) -> Self {
        Self::from(slice.to_vec())
    }
}

/// Convenience implementation for copying contents of
/// array into a `Vec`, then creating a `Buffer` from that.
impl<const N: usize> From<[u8; N]> for Buffer {
    fn from(array: [u8; N]) -> Self {
        Self::from(array.to_vec())
    }
}

/// A `Vec<MaybeUninit<u8>>` can be converted to a `Buffer` cheaply.
///
/// The Vec's `len` will be used for the `requested_len`, and the `len` will be 0.
impl From<Vec<MaybeUninit<u8>>> for Buffer {
    fn from(vec: Vec<MaybeUninit<u8>>) -> Self {
        let mut vec = ManuallyDrop::new(vec);
        Buffer {
            ptr: vec.as_mut_ptr().cast(),
            len: 0,
            requested_len: vec.len().try_into().expect("len overflow"),
            capacity: vec.capacity().try_into().expect("capacity overflow"),
            allocator: Allocator::Default,
        }
    }
}

impl Deref for Buffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len as usize) }
    }
}

impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len as usize) }
    }
}

impl Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer")
            .field("len", &self.len)
            .field("requested_len", &self.requested_len)
            .field("data", &format_args!("{:02x?}", &self[..]))
            .finish()
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        match &self.allocator {
            Allocator::Default => unsafe {
                drop(Vec::from_raw_parts(
                    self.ptr,
                    self.len as usize,
                    self.capacity as usize,
                ));
            },
            #[cfg(any(target_os = "linux", target_os = "android"))]
            Allocator::Mmap => unsafe {
                rustix::mm::munmap(self.ptr as *mut _, self.capacity as usize).unwrap();
            },
            Allocator::External(owner) => unsafe {
                // SAFETY: construction stored the exact external pointer, capacity, and owner.
                owner.release(NonNull::new_unchecked(self.ptr), self.capacity as usize);
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct TestOwner {
        _bytes: Box<[u8; 4]>,
        expected: usize,
        releases: AtomicUsize,
    }

    impl ExternalBufferOwner for TestOwner {
        unsafe fn release(&self, ptr: NonNull<u8>, capacity: usize) {
            assert_eq!(ptr.as_ptr() as usize, self.expected);
            assert_eq!(capacity, 4);
            self.releases.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn external_buffer_reads_owned_memory_and_releases_once() {
        let mut bytes = Box::new([1, 2, 3, 4]);
        let ptr = NonNull::from(&mut bytes[0]);
        let owner = Arc::new(TestOwner {
            _bytes: bytes,
            expected: ptr.as_ptr() as usize,
            releases: AtomicUsize::new(0),
        });
        // SAFETY: `owner` retains the exact four-byte allocation until release.
        let buffer = unsafe { Buffer::from_external_parts(ptr, 3, 4, 4, owner.clone()) };
        assert_eq!(&buffer[..], &[1, 2, 3]);
        assert_eq!(buffer.requested_len(), 4);
        drop(buffer);
        assert_eq!(owner.releases.load(Ordering::Relaxed), 1);
    }
}
