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

use crate::{
    control::{ControlIn, ControlOut},
    Completion, EndpointType,
};

pub(crate) trait Platform {
    /// Platform-specific per-transfer data.
    type TransferData: Send;

    /// Get a `TransferData`.
    fn make_transfer_data(&self, endpoint: u8, ep_type: EndpointType) -> Self::TransferData;

    /// Request cancellation of a transfer that may or may not currently be
    /// pending.
    fn cancel(&self, transfer: &Self::TransferData);
}

pub trait TransferRequest {
    type Response;
}

pub(crate) trait PlatformSubmit<D: TransferRequest>: Platform {
    /// Fill the transfer with the data from `data` and submit it to the kernel.
    /// Arrange for `notify_completion(transfer)` to be called once the transfer
    /// has completed.
    ///
    /// SAFETY(caller): transfer is in an idle state
    unsafe fn submit(&self, data: D, platform: &mut Self::TransferData, transfer: *mut c_void);

    /// SAFETY(caller): `transfer` is in a completed state
    unsafe fn take_completed(transfer: &mut Self::TransferData) -> Completion<D::Response>;
}

impl TransferRequest for Vec<u8> {
    type Response = Vec<u8>;
}

impl TransferRequest for ControlIn {
    type Response = Vec<u8>;
}

impl TransferRequest for ControlOut<'_> {
    type Response = usize;
}

struct TransferInner<P: Platform> {
    /// Platform-specific data.
    ///
    /// In an `UnsafeCell` because we provide `&mut` when the
    /// state gurantees us exclusive access
    platform_data: UnsafeCell<P::TransferData>,

    /// One of the `STATE_*` constants below, used to synchronize
    /// the state.
    state: AtomicU8,

    /// Waker that is notified when transfer completes.
    waker: AtomicWaker,

    /// Platform
    interface: Arc<P>,
}

/// Handle to a transfer.
///
/// Cancels the transfer and arranges for memory to be freed
/// when dropped.
pub(crate) struct TransferHandle<P: Platform> {
    ptr: NonNull<TransferInner<P>>,
}

unsafe impl<P: Platform> Send for TransferHandle<P> {}
unsafe impl<P: Platform> Sync for TransferHandle<P> {}

/// The transfer has not been submitted. The buffer is not valid.
const STATE_IDLE: u8 = 0;

/// The transfer has been or is about to be submitted to the kernel and
/// completion has not yet been handled. The buffer points to valid memory but
/// cannot necessarily be accessed by userspace. There is a future or queue
/// waiting for it completion.
const STATE_PENDING: u8 = 1;

/// Like PENDING, but there is no one waiting for completion. The completion
/// handler will drop the buffer and transfer.
const STATE_ABANDONED: u8 = 3;

/// The transfer completion has been handled on the event loop thread. The
/// buffer is valid and may be accessed by the `TransferHandle`.
const STATE_COMPLETED: u8 = 3;

impl<P: Platform> TransferHandle<P> {
    /// Create a new transfer and get a handle.
    pub(crate) fn new(interface: Arc<P>, endpoint: u8, ep_type: EndpointType) -> TransferHandle<P> {
        let b = Box::new(TransferInner {
            platform_data: UnsafeCell::new(interface.make_transfer_data(endpoint, ep_type)),
            state: AtomicU8::new(STATE_IDLE),
            waker: AtomicWaker::new(),
            interface,
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

    fn platform_data(&self) -> &P::TransferData {
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
            inner.interface.submit(
                data,
                &mut *inner.platform_data.get(),
                self.ptr.as_ptr() as *mut c_void,
            );
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.inner().interface.cancel(self.platform_data());
    }

    fn poll_completion_generic(&mut self, cx: &Context) -> Poll<&mut P::TransferData> {
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

    pub fn poll_completion<D: TransferRequest>(
        &mut self,
        cx: &Context,
    ) -> Poll<Completion<D::Response>>
    where
        D: TransferRequest,
        P: PlatformSubmit<D>,
    {
        // SAFETY: `poll_completion_generic` checks that it is completed
        self.poll_completion_generic(cx)
            .map(|u| unsafe { P::take_completed(u) })
    }
}

impl<P: Platform> Drop for TransferHandle<P> {
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
pub(crate) unsafe fn notify_completion<P: Platform>(transfer: *mut c_void) {
    unsafe {
        let transfer = transfer as *mut TransferInner<P>;
        let waker = (*transfer).waker.take();
        match (*transfer).state.swap(STATE_COMPLETED, Ordering::Release) {
            STATE_PENDING => {
                if let Some(waker) = waker {
                    waker.wake()
                }
            }
            STATE_ABANDONED => {
                drop(Box::from_raw(transfer));
            }
            s => panic!("Completing transfer in unexpected state {s}"),
        }
    }
}
