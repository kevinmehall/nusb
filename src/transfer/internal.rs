use std::{
    cell::UnsafeCell,
    ffi::c_void,
    ptr::NonNull,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use atomic_waker::AtomicWaker;

use super::Completion;

#[cfg(not(target_arch = "wasm32"))]
pub trait PlatformTransfer: Send {
    /// Request cancellation of a transfer that may or may not currently be
    /// pending.
    fn cancel(&self);
}

/// A transfer specific to the platform.
///
/// NOTE: For WebUSB we cannot implement Send as the raw pointers used by the WASM FFI are not Send.
///
/// Furthermore, it is not currently possible to cancel requests in WebUSB (see https://github.com/WICG/webusb/issues/25).
#[cfg(target_arch = "wasm32")]
pub trait PlatformTransfer {}

pub trait TransferRequest {
    type Response;
}

pub trait PlatformSubmit<D: TransferRequest>: PlatformTransfer {
    /// Fill the transfer with the data from `data` and submit it to the kernel.
    /// Arrange for `notify_completion(transfer)` to be called once the transfer
    /// has completed.
    ///
    /// SAFETY(caller): transfer is in an idle state
    unsafe fn submit(&mut self, data: D, transfer: *mut c_void);

    /// SAFETY(caller): `transfer` is in a completed state
    unsafe fn take_completed(&mut self) -> Completion<D::Response>;
}

pub(crate) struct TransferInner<P: PlatformTransfer> {
    /// Platform-specific data.
    ///
    /// In an `UnsafeCell` because we provide `&mut` when the
    /// state guarantees us exclusive access
    platform_data: UnsafeCell<P>,

    /// One of the `STATE_*` constants below, used to synchronize
    /// the state.
    state: AtomicU8,

    /// Waker that is notified when transfer completes.
    waker: Arc<AtomicWaker>,
}

impl<P: PlatformTransfer> TransferInner<P> {
    pub(crate) fn platform_data(&mut self) -> &mut P {
        unsafe { &mut *self.platform_data.get() }
    }
}

/// Handle to a transfer.
///
/// Cancels the transfer and arranges for memory to be freed
/// when dropped.
pub(crate) struct TransferHandle<P: PlatformTransfer> {
    ptr: NonNull<TransferInner<P>>,
}

unsafe impl<P: PlatformTransfer> Send for TransferHandle<P> {}
unsafe impl<P: PlatformTransfer> Sync for TransferHandle<P> {}

/// The transfer has not been submitted. The buffer is not valid.
const STATE_IDLE: u8 = 0;

/// The transfer has been or is about to be submitted to the kernel and
/// completion has not yet been handled. The buffer points to valid memory but
/// cannot necessarily be accessed by userspace. There is a future or queue
/// waiting for it completion.
const STATE_PENDING: u8 = 1;

/// Like PENDING, but there is no one waiting for completion. The completion
/// handler will drop the buffer and transfer.
const STATE_ABANDONED: u8 = 2;

/// The transfer completion has been handled on the event loop thread. The
/// buffer is valid and may be accessed by the `TransferHandle`.
const STATE_COMPLETED: u8 = 3;

impl<P: PlatformTransfer> TransferHandle<P> {
    /// Create a new transfer and get a handle.
    pub(crate) fn new(inner: P) -> TransferHandle<P> {
        let b = Box::new(TransferInner {
            platform_data: UnsafeCell::new(inner),
            state: AtomicU8::new(STATE_IDLE),
            waker: Arc::new(AtomicWaker::new()),
        });

        TransferHandle {
            ptr: Box::leak(b).into(),
        }
    }

    fn inner(&self) -> &TransferInner<P> {
        // SAFETY: while `TransferHandle` is alive, its `TransferInner` is alive
        // (it may be shared by `notify_completion` on the event thread, so can't be &mut)
        unsafe { self.ptr.as_ref() }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn platform_data(&self) -> &P {
        // SAFETY: while `TransferHandle` is alive, the only mutable access to `platform_data`
        // is via this `TransferHandle`.
        unsafe { &*self.inner().platform_data.get() }
    }

    pub(crate) fn submit<D>(&mut self, data: D)
    where
        D: TransferRequest,
        P: PlatformSubmit<D>,
    {
        let inner = self.inner();

        // It's the syscall that submits the transfer that actually performs the
        // release ordering.
        let prev = self.inner().state.swap(STATE_PENDING, Ordering::Relaxed);
        assert_eq!(prev, STATE_IDLE, "Transfer should be idle when submitted");

        // SAFETY: while `TransferHandle` is alive, the only mutable access to `platform_data`
        // is via this `TransferHandle`. Verified that it is idle.
        unsafe {
            let p = &mut *inner.platform_data.get();
            p.submit(data, self.ptr.as_ptr() as *mut c_void);
        }
    }

    /// Note: This is a nop on WebUSB because of https://github.com/WICG/webusb/issues/25.
    pub(crate) fn cancel(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.platform_data().cancel();
    }

    fn poll_completion_generic(&mut self, cx: &Context) -> Poll<&mut P> {
        let inner = self.inner();
        inner.waker.register(cx.waker());
        match inner.state.load(Ordering::Acquire) {
            STATE_PENDING => Poll::Pending,
            STATE_COMPLETED => {
                // Relaxed because this doesn't synchronize with anything,
                // just marks that we no longer need to drop the buffer
                inner.state.store(STATE_IDLE, Ordering::Relaxed);

                // SAFETY: while `TransferHandle` is alive, the only mutable access to `platform_data`
                // is via this `TransferHandle`.
                Poll::Ready(unsafe { &mut *inner.platform_data.get() })
            }
            s => panic!("Polling transfer in unexpected state {s}"),
        }
    }

    pub fn poll_completion<D>(&mut self, cx: &Context) -> Poll<Completion<D::Response>>
    where
        D: TransferRequest,
        P: PlatformSubmit<D>,
    {
        // SAFETY: `poll_completion_generic` checks that it is completed
        self.poll_completion_generic(cx)
            .map(|u| unsafe { u.take_completed() })
    }
}

impl<P: PlatformTransfer> Drop for TransferHandle<P> {
    fn drop(&mut self) {
        match self.inner().state.swap(STATE_ABANDONED, Ordering::Acquire) {
            STATE_PENDING => {
                self.cancel();
                /* handler responsible for dropping */
            }
            STATE_IDLE | STATE_COMPLETED => {
                // SAFETY: state means there is no concurrent access
                unsafe { drop(Box::from_raw(self.ptr.as_ptr())) }
            }
            s => panic!("Dropping transfer in unexpected state {s}"),
        }
    }
}

/// Notify that a transfer has completed.
///
/// SAFETY: `transfer` must be a pointer previously passed to `submit`, and
/// the caller / kernel must no longer dereference it or its buffer.
pub(crate) unsafe fn notify_completion<P: PlatformTransfer>(transfer: *mut c_void) {
    unsafe {
        let transfer = transfer as *mut TransferInner<P>;
        let waker = (*transfer).waker.clone();
        match (*transfer).state.swap(STATE_COMPLETED, Ordering::Release) {
            STATE_PENDING => waker.wake(),
            STATE_ABANDONED => {
                drop(Box::from_raw(transfer));
            }
            s => panic!("Completing transfer in unexpected state {s}"),
        }
    }
}
