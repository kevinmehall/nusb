use std::{
    cell::UnsafeCell,
    future::Future,
    mem::{self, ManuallyDrop},
    ptr::{null_mut, NonNull},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use atomic_waker::AtomicWaker;
use rustix::io::Errno;

use crate::{transfer::EndpointType, Completion, TransferStatus};

use super::{
    usbfs::{
        Urb, USBDEVFS_URB_TYPE_BULK, USBDEVFS_URB_TYPE_CONTROL, USBDEVFS_URB_TYPE_INTERRUPT,
        USBDEVFS_URB_TYPE_ISO,
    },
    Interface,
};

#[repr(C)]
pub(crate) struct TransferInner {
    urb: UnsafeCell<Urb>,
    state: AtomicU8,
    waker: AtomicWaker,
    interface: Arc<Interface>,
}

impl TransferInner {
    /// Transfer ownership of `buf` into the transfer's `urb`.
    /// SAFETY: requires that there is no concurrent access to  `urb`
    unsafe fn put_buffer(&self, buf: Vec<u8>) {
        unsafe {
            let mut buf = ManuallyDrop::new(buf);
            let urb = &mut *self.urb.get();
            urb.buffer = buf.as_mut_ptr();
            assert!(buf.len() < i32::MAX as usize, "Buffer too large");
            urb.actual_length = buf.len() as i32;
            assert!(buf.capacity() < i32::MAX as usize, "Buffer too large");
            urb.buffer_length = buf.capacity() as i32;
        }
    }

    /// Transfer ownership of transfer's `urb` buffer back to a `Vec`.
    /// SAFETY: requires that the the buffer is present and there is no concurrent
    /// access to `urb`. Invalidates the buffer.
    unsafe fn take_buffer(&self) -> Vec<u8> {
        unsafe {
            let urb = &mut *self.urb.get();
            Vec::from_raw_parts(
                mem::replace(&mut urb.buffer, null_mut()),
                urb.actual_length as usize,
                urb.buffer_length as usize,
            )
        }
    }

    /// Get the transfer status
    /// SAFETY: requires that there is no concurrent access to  `urb`
    unsafe fn status(&self) -> TransferStatus {
        let status = unsafe { (&*self.urb.get()).status };

        if status == 0 {
            return TransferStatus::Complete;
        }

        // It's sometimes positive, sometimes negative, but rustix panics if negative.
        match Errno::from_raw_os_error(status.abs()) {
            Errno::NODEV | Errno::SHUTDOWN => TransferStatus::Disconnected,
            Errno::PIPE => TransferStatus::Stall,
            Errno::NOENT | Errno::CONNRESET => TransferStatus::Cancelled,
            Errno::PROTO | Errno::ILSEQ | Errno::OVERFLOW | Errno::COMM | Errno::TIME => {
                TransferStatus::Fault
            }
            _ => TransferStatus::UnknownError,
        }
    }
}

pub struct Transfer {
    ptr: NonNull<TransferInner>,
}

/// The transfer has not been submitted. The buffer is not valid.
const STATE_IDLE: u8 = 0;

/// The transfer has been submitted to the kernel and completion has not yet
/// been handled. The buffer points to valid memory but cannot be accessed by
/// userspace. There is a future or queue waiting for it completion.
const STATE_PENDING: u8 = 1;

/// Like PENDING, but there is no one waiting for completion. The completion
/// handler will drop the buffer and transfer.
const STATE_ABANDONED: u8 = 3;

/// The transfer completion has been handled. The buffer is valid and may
/// be accessed.
const STATE_COMPLETED: u8 = 3;

impl Transfer {
    pub(crate) fn new(interface: Arc<Interface>, endpoint: u8, ep_type: EndpointType) -> Transfer {
        let ep_type = match ep_type {
            EndpointType::Control => USBDEVFS_URB_TYPE_CONTROL,
            EndpointType::Interrupt => USBDEVFS_URB_TYPE_INTERRUPT,
            EndpointType::Bulk => USBDEVFS_URB_TYPE_BULK,
            EndpointType::Isochronous => USBDEVFS_URB_TYPE_ISO,
        };

        let b = Box::new(TransferInner {
            urb: UnsafeCell::new(Urb {
                ep_type,
                endpoint,
                status: 0,
                flags: 0,
                buffer: null_mut(),
                buffer_length: 0,
                actual_length: 0,
                start_frame: 0,
                number_of_packets_or_stream_id: 0,
                error_count: 0,
                signr: 0,
                usercontext: null_mut(),
            }),
            state: AtomicU8::new(STATE_IDLE),
            waker: AtomicWaker::new(),
            interface,
        });

        Transfer {
            ptr: Box::leak(b).into(),
        }
    }

    fn inner(&self) -> &TransferInner {
        // Safety: while Transfer is alive, its TransferInner is alive
        unsafe { self.ptr.as_ref() }
    }

    /// Prepare the transfer for submission by filling the buffer fields
    /// and setting the state to PENDING. Returns a `*mut TransferInner`
    /// that must later be passed to `complete`.
    ///
    /// Panics if the transfer has already been submitted.
    pub(crate) fn submit(&mut self, data: Vec<u8>) {
        let inner = self.inner();
        assert_eq!(
            inner.state.load(Ordering::Acquire),
            STATE_IDLE,
            "Transfer should be idle when submitted"
        );
        unsafe {
            // SAFETY: invariants guaranteed by being in state IDLE
            inner.put_buffer(data);
        }
        inner.state.store(STATE_PENDING, Ordering::Release);
        unsafe {
            inner.interface.submit_transfer(self.ptr.as_ptr());
        }
    }

    pub(crate) fn cancel(&mut self) {
        let inner = self.inner();
        unsafe {
            inner.interface.cancel_transfer(self.ptr.as_ptr());
        }
    }

    pub fn poll_completion(&self, cx: &Context) -> Poll<Completion> {
        let inner = self.inner();
        inner.waker.register(cx.waker());
        match inner.state.load(Ordering::Acquire) {
            STATE_PENDING => Poll::Pending,
            STATE_COMPLETED => {
                // SAFETY: state means we have exclusive access
                // and the buffer is valid.
                inner.state.store(STATE_IDLE, Ordering::Relaxed);
                unsafe {
                    let data = inner.take_buffer();
                    let status = inner.status();
                    Poll::Ready(Completion { data, status })
                }
            }
            s => panic!("Polling transfer in unexpected state {s}"),
        }
    }

    pub(crate) unsafe fn notify_completion(transfer: *mut TransferInner) {
        unsafe {
            let waker = (*transfer).waker.take();
            match (*transfer).state.swap(STATE_COMPLETED, Ordering::Release) {
                STATE_PENDING => {
                    if let Some(waker) = waker {
                        waker.wake()
                    }
                }
                STATE_ABANDONED => {
                    let b = Box::from_raw(transfer);
                    drop(b.take_buffer());
                    drop(b);
                }
                s => panic!("Completing transfer in unexpected state {s}"),
            }
        }
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        match self.inner().state.swap(STATE_ABANDONED, Ordering::Acquire) {
            STATE_PENDING => {
                self.cancel();
                /* handler responsible for dropping */
            }
            STATE_IDLE => {
                // SAFETY: state means there is no concurrent access
                unsafe { drop(Box::from_raw(self.ptr.as_ptr())) }
            }
            STATE_COMPLETED => {
                // SAFETY: state means buffer is valid and there is no concurrent access
                unsafe {
                    let b = Box::from_raw(self.ptr.as_ptr());
                    drop(b.take_buffer());
                    drop(b);
                }
            }
            s => panic!("Dropping transfer in unexpected state {s}"),
        }
    }
}

impl Future for Transfer {
    type Output = Completion;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.as_mut().poll_completion(cx)
    }
}
