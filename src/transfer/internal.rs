use std::{
    collections::VecDeque,
    future::Future,
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::{addr_of_mut, NonNull},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll, Waker},
    thread::{self, Thread},
};

use crate::MaybeFuture;

pub struct Notify {
    state: Mutex<NotifyState>,
}

pub enum NotifyState {
    None,
    Waker(Waker),
    Thread(Thread),
}

impl AsRef<Notify> for Notify {
    fn as_ref(&self) -> &Notify {
        self
    }
}

impl Notify {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(NotifyState::None),
        }
    }

    pub fn subscribe(&self, cx: &mut Context) {
        *self.state.lock().unwrap() = NotifyState::Waker(cx.waker().clone());
    }

    pub fn wait(&self) {
        *self.state.lock().unwrap() = NotifyState::Thread(thread::current());
        thread::park();
    }

    pub fn notify(&self) {
        match &mut *self.state.lock().unwrap() {
            NotifyState::None => {}
            NotifyState::Waker(waker) => waker.wake_by_ref(),
            NotifyState::Thread(thread) => thread.unpark(),
        }
    }
}

#[repr(C)]
struct TransferInner<P> {
    /// Platform-specific data.
    platform_data: P,

    /// One of the `STATE_*` constants below, used to synchronize
    /// the state.
    state: AtomicU8,

    /// Object notified when transfer completes.
    notify: Arc<dyn AsRef<Notify> + Send + Sync>,
}

/// Either the transfer has not yet been submitted, or it has been completed.
/// The inner data may be accessed mutably.
const STATE_IDLE: u8 = 0;

/// The transfer has been or is about to be submitted to the kernel and
/// completion has not yet been handled. The buffer cannot necessarily be
/// accessed by userspace. There is a future or queue waiting for its completion.
const STATE_PENDING: u8 = 1;

/// Like PENDING, but there is no one waiting for completion. The completion
/// handler will drop the buffer and transfer.
const STATE_ABANDONED: u8 = 2;

/// Handle to a transfer that is known to be idle.
pub(crate) struct Idle<P>(Box<TransferInner<P>>);

impl<P> Idle<P> {
    /// Create a new transfer and get a handle.
    pub(crate) fn new(notify: Arc<dyn AsRef<Notify> + Send + Sync>, inner: P) -> Idle<P> {
        Idle(Box::new(TransferInner {
            platform_data: inner,
            state: AtomicU8::new(STATE_IDLE),
            notify,
        }))
    }

    /// Mark the transfer as pending. The caller must submit the transfer to the kernel
    /// and arrange for `notify_completion` to be called on the returned value.
    pub(crate) fn pre_submit(self) -> Pending<P> {
        // It's the syscall that submits the transfer that actually performs the
        // release ordering.
        let prev = self.0.state.swap(STATE_PENDING, Ordering::Relaxed);
        assert_eq!(prev, STATE_IDLE, "Transfer should be idle when submitted");
        Pending {
            ptr: unsafe { NonNull::new_unchecked(Box::into_raw(self.0)) },
        }
    }

    pub(crate) fn notify_eq<T>(&self, other: &Arc<T>) -> bool {
        Arc::as_ptr(&self.0.notify) as *const () == Arc::as_ptr(other) as *const ()
    }
}

impl<P> Deref for Idle<P> {
    type Target = P;
    fn deref(&self) -> &Self::Target {
        &self.0.platform_data
    }
}

impl<P> DerefMut for Idle<P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.platform_data
    }
}

/// Handle to a transfer that may be pending.
pub(crate) struct Pending<P> {
    ptr: NonNull<TransferInner<P>>,
}

unsafe impl<P: Send> Send for Pending<P> {}
unsafe impl<P: Sync> Sync for Pending<P> {}

impl<P> Pending<P> {
    pub fn as_ptr(&self) -> *mut P {
        // first member of repr(C) struct
        self.ptr.as_ptr().cast()
    }

    fn state(&self) -> &AtomicU8 {
        // Get state without dereferencing as `TransferInner`, because
        // its `platform_data` may be mutably aliased.
        unsafe { &*(addr_of_mut!((*self.ptr.as_ptr()).state)) }
    }

    pub fn is_complete(&self) -> bool {
        match self.state().load(Ordering::Acquire) {
            STATE_PENDING => false,
            STATE_IDLE => true,
            s => panic!("Polling transfer in unexpected state {s}"),
        }
    }

    /// SAFETY: is_complete must have returned `true`
    pub unsafe fn into_idle(self) -> Idle<P> {
        debug_assert!(self.is_complete());
        let transfer = ManuallyDrop::new(self);
        Idle(Box::from_raw(transfer.ptr.as_ptr()))
    }
}

pub fn take_completed_from_queue<P>(queue: &mut VecDeque<Pending<P>>) -> Option<Idle<P>> {
    if queue.front().expect("no transfer pending").is_complete() {
        Some(unsafe { queue.pop_front().unwrap().into_idle() })
    } else {
        None
    }
}

pub fn take_completed_from_option<P>(option: &mut Option<Pending<P>>) -> Option<Idle<P>> {
    // TODO: use Option::take_if once supported by MSRV
    if option.as_mut().map_or(false, |next| next.is_complete()) {
        option.take().map(|t| unsafe { t.into_idle() })
    } else {
        None
    }
}

impl<P> Drop for Pending<P> {
    fn drop(&mut self) {
        match self.state().swap(STATE_ABANDONED, Ordering::Acquire) {
            STATE_PENDING => { /* handler responsible for dropping */ }
            STATE_IDLE => {
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
pub(crate) unsafe fn notify_completion<P>(transfer: *mut P) {
    unsafe {
        let transfer = transfer as *mut TransferInner<P>;
        let notify = (*transfer).notify.clone();
        match (*transfer).state.swap(STATE_IDLE, Ordering::Release) {
            STATE_PENDING => (*notify).as_ref().notify(),
            STATE_ABANDONED => {
                drop(Box::from_raw(transfer));
            }
            s => panic!("Completing transfer in unexpected state {s}"),
        }
    }
}

pub(crate) struct TransferFuture<D> {
    transfer: Option<Pending<D>>,
    notify: Arc<Notify>,
}

impl<D> TransferFuture<D> {
    pub(crate) fn new(transfer: D, submit: impl FnOnce(Idle<D>) -> Pending<D>) -> Self {
        let notify = Arc::new(Notify::new());
        let transfer = submit(Idle::new(notify.clone(), transfer));
        Self {
            transfer: Some(transfer),
            notify,
        }
    }
}

impl<D> Future for TransferFuture<D> {
    type Output = Idle<D>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        self.notify.subscribe(cx);
        take_completed_from_option(&mut self.transfer).map_or(Poll::Pending, Poll::Ready)
    }
}

impl<D> MaybeFuture for TransferFuture<D> {
    fn wait(mut self) -> Self::Output {
        loop {
            if let Some(transfer) = take_completed_from_option(&mut self.transfer) {
                return transfer;
            }
            self.notify.wait();
        }
    }
}
