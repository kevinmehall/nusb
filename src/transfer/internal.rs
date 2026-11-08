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
    time::{Duration, Instant},
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

impl NotifyState {
    fn take(&mut self) -> Self {
        std::mem::replace(self, NotifyState::None)
    }

    fn notify(self) {
        match self {
            NotifyState::None => {}
            NotifyState::Waker(waker) => waker.wake(),
            NotifyState::Thread(thread) => thread.unpark(),
        }
    }
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

    pub fn wait<T>(&self, mut check: impl FnMut() -> Option<T>) -> T {
        loop {
            *self.state.lock().unwrap() = NotifyState::Thread(thread::current());
            if let Some(result) = check() {
                return result;
            }
            thread::park();
        }
    }

    pub fn wait_timeout<T>(
        &self,
        timeout: Duration,
        mut check: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        let start = Instant::now();
        loop {
            *self.state.lock().unwrap() = NotifyState::Thread(thread::current());
            if let Some(result) = check() {
                return Some(result);
            }
            let remaining = timeout.checked_sub(start.elapsed())?;
            thread::park_timeout(remaining);
        }
    }

    fn take_notify_state(&self) -> NotifyState {
        self.state.lock().unwrap().take()
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
pub(crate) struct Idle<P> {
    ptr: NonNull<TransferInner<P>>,
}

/// Reusable native descriptors, bounded by the caller's reserved concurrency.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct IdlePool<P> {
    transfers: Vec<Idle<P>>,
    limit: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl<P> IdlePool<P> {
    /// Retains one completed descriptor unless a larger window is reserved.
    pub(crate) fn new() -> Self {
        Self {
            transfers: Vec::with_capacity(1),
            limit: 1,
        }
    }

    /// Prepares a total descriptor window without replacing pending native owners.
    pub(crate) fn reserve(
        &mut self,
        count: usize,
        pending: usize,
        mut create: impl FnMut() -> Idle<P>,
    ) {
        if count == 0 {
            return;
        }
        self.limit = self.limit.max(count);
        self.transfers.reserve(self.limit - self.transfers.len());
        let additional = self
            .limit
            .saturating_sub(pending)
            .saturating_sub(self.transfers.len());
        for _ in 0..additional {
            self.transfers.push(create());
        }
    }

    /// Reuses an idle descriptor; exceeding the prepared window may allocate.
    pub(crate) fn take_or_create(&mut self, create: impl FnOnce() -> Idle<P>) -> Idle<P> {
        self.transfers.pop().unwrap_or_else(create)
    }

    /// Retains a terminal descriptor only while reserved cache capacity remains.
    pub(crate) fn recycle(&mut self, transfer: Idle<P>) {
        if self.transfers.len() < self.limit {
            self.transfers.push(transfer);
        }
    }
}

impl<P> Idle<P> {
    /// Create a new transfer and get a handle.
    pub(crate) fn new(notify: Arc<dyn AsRef<Notify> + Send + Sync>, inner: P) -> Idle<P> {
        let b = Box::new(TransferInner {
            platform_data: inner,
            state: AtomicU8::new(STATE_IDLE),
            notify,
        });
        Idle {
            // SAFETY: Box pointer is non-null
            ptr: unsafe { NonNull::new_unchecked(Box::into_raw(b)) },
        }
    }

    fn inner(&self) -> &TransferInner<P> {
        // SAFETY: Idle state means there is no concurrent access
        unsafe { self.ptr.as_ref() }
    }

    fn inner_mut(&mut self) -> &mut TransferInner<P> {
        // SAFETY: Idle state means there is no concurrent access
        unsafe { self.ptr.as_mut() }
    }

    #[allow(unused)]
    pub fn as_ptr(&self) -> *mut P {
        // first member of repr(C) struct
        self.ptr.as_ptr().cast()
    }

    /// Mark the transfer as pending. The caller must submit the transfer to the kernel
    /// and arrange for `notify_completion` to be called on the returned value.
    pub(crate) fn pre_submit(self) -> Pending<P> {
        // It's the syscall that submits the transfer that actually performs the
        // release ordering.
        let prev = self.inner().state.swap(STATE_PENDING, Ordering::Relaxed);
        assert_eq!(prev, STATE_IDLE, "Transfer should be idle when submitted");
        let transfer = ManuallyDrop::new(self);
        Pending { ptr: transfer.ptr }
    }

    pub(crate) fn simulate_complete(self) -> Pending<P> {
        let transfer = ManuallyDrop::new(self);
        Pending { ptr: transfer.ptr }
    }
}

unsafe impl<P: Send> Send for Idle<P> {}
unsafe impl<P: Sync> Sync for Idle<P> {}

impl<P> Deref for Idle<P> {
    type Target = P;
    fn deref(&self) -> &Self::Target {
        &self.inner().platform_data
    }
}

impl<P> DerefMut for Idle<P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner_mut().platform_data
    }
}

impl<P> Drop for Idle<P> {
    fn drop(&mut self) {
        // SAFETY: state means there is no concurrent access
        unsafe { drop(Box::from_raw(self.ptr.as_ptr())) }
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
        unsafe { &*addr_of_mut!((*self.ptr.as_ptr()).state) }
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
        Idle { ptr: transfer.ptr }
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
    if option.as_mut().is_some_and(|next| next.is_complete()) {
        option.take().map(|t| unsafe { t.into_idle() })
    } else {
        None
    }
}

impl<P> Drop for Pending<P> {
    fn drop(&mut self) {
        match self.state().swap(STATE_ABANDONED, Ordering::AcqRel) {
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
        match (*transfer).state.swap(STATE_IDLE, Ordering::AcqRel) {
            STATE_PENDING => {
                let wake = notify.deref().as_ref().take_notify_state();
                drop(notify);
                wake.notify();
            }
            STATE_ABANDONED => {
                drop(Box::from_raw(transfer));
            }
            s => panic!("Completing transfer in unexpected state {s}"),
        }
    }
}

pub(crate) struct TransferFuture<C, T> {
    transfer: Option<Pending<T>>,
    inner: Arc<TransferFutureInner<C>>,
}

struct TransferFutureInner<C> {
    notify: Notify,

    /// Context such as an Arc<Device> that is kept alive as long as the transfer is pending.
    context: C,
}

impl<C> AsRef<Notify> for TransferFutureInner<C> {
    fn as_ref(&self) -> &Notify {
        &self.notify
    }
}

impl<C: Send + Sync + 'static, T> TransferFuture<C, T> {
    pub(crate) fn new(
        context: C,
        transfer: T,
        submit: impl FnOnce(&C, Idle<T>) -> Pending<T>,
    ) -> Self {
        let notify = Notify::new();
        let inner = Arc::new(TransferFutureInner { notify, context });
        let transfer = Some(submit(&inner.context, Idle::new(inner.clone(), transfer)));
        Self { transfer, inner }
    }
}

impl<C, T> Future for TransferFuture<C, T> {
    type Output = Idle<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        self.inner.notify.subscribe(cx);
        take_completed_from_option(&mut self.transfer).map_or(Poll::Pending, Poll::Ready)
    }
}

impl<C, T> MaybeFuture for TransferFuture<C, T>
where
    C: Send + Sync + 'static,
    T: Send,
{
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(mut self) -> Self::Output {
        self.inner
            .notify
            .wait(|| take_completed_from_option(&mut self.transfer))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[derive(Default)]
    struct Counts {
        created: AtomicUsize,
        dropped: AtomicUsize,
    }

    struct Descriptor(Arc<Counts>);

    impl Drop for Descriptor {
        fn drop(&mut self) {
            self.0.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn descriptor(notify: &Arc<Notify>, counts: &Arc<Counts>) -> Idle<Descriptor> {
        counts.created.fetch_add(1, Ordering::Relaxed);
        Idle::new(notify.clone(), Descriptor(counts.clone()))
    }

    #[test]
    fn prepared_descriptors_survive_bursts_without_reallocation() {
        let notify = Arc::new(Notify::new());
        let counts = Arc::new(Counts::default());
        let mut pool = IdlePool::new();
        pool.reserve(8, 0, || descriptor(&notify, &counts));
        for (concurrent, created, dropped) in [(8, 8, 0), (8, 8, 0), (10, 10, 2), (8, 10, 2)] {
            let mut pending = VecDeque::with_capacity(concurrent);
            for _ in 0..concurrent {
                pending.push_back(
                    pool.take_or_create(|| descriptor(&notify, &counts))
                        .pre_submit(),
                );
            }
            assert!(take_completed_from_queue(&mut pending).is_none());
            assert_eq!(counts.created.load(Ordering::Relaxed), created);
            for transfer in pending.iter().rev() {
                // SAFETY: the scripted native completion owns this pending descriptor and ends
                // all native access before publishing terminal completion.
                unsafe { notify_completion(transfer.as_ptr()) };
            }
            while !pending.is_empty() {
                pool.recycle(
                    take_completed_from_queue(&mut pending).expect("terminal native descriptor"),
                );
            }
            assert_eq!(counts.dropped.load(Ordering::Relaxed), dropped);
        }
        drop(pool);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn reservation_counts_pending_owners_and_abandonment_waits_for_completion() {
        let notify = Arc::new(Notify::new());
        let counts = Arc::new(Counts::default());
        let mut pool = IdlePool::new();
        let first = pool
            .take_or_create(|| descriptor(&notify, &counts))
            .pre_submit();
        let second = pool
            .take_or_create(|| descriptor(&notify, &counts))
            .pre_submit();
        pool.reserve(8, 2, || descriptor(&notify, &counts));
        assert_eq!(counts.created.load(Ordering::Relaxed), 8);
        drop(pool);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 6);
        let first_pointer = first.as_ptr();
        drop(first);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 6);
        // SAFETY: this callback is the final native access to the abandoned descriptor.
        unsafe { notify_completion(first_pointer) };
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 7);
        // SAFETY: the scripted native owner has completed its second transfer.
        unsafe { notify_completion(second.as_ptr()) };
        // SAFETY: notify_completion published terminal completion above.
        let completed = unsafe { second.into_idle() };
        drop(completed);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 8);
    }
}
