use std::{
    collections::VecDeque,
    future::{poll_fn, Future},
    marker::PhantomData,
    sync::Arc,
    task::{Context, Poll},
};

use crate::{platform, Error};

use super::{Completion, EndpointType, PlatformSubmit, TransferHandle, TransferRequest};

/// Manages a stream of transfers on an endpoint.
///
/// A `Queue` optimizes a common pattern when streaming data to or from a USB
/// endpoint: To maximize throughput and minimize latency, the host controller
/// needs to attempt a transfer in every possible frame. That requires always
/// having a transfer request pending with the kernel by submitting multiple
/// transfer requests and re-submitting them as they complete.
///
/// Use the methods on [`Interface`][`crate::Interface`] to obtain a `Queue`.
///
/// When the `Queue` is dropped, all pending transfers are cancelled.
///
/// ### Why use a `Queue` instead of submitting multiple transfers individually with the methods on [`Interface`][`crate::Interface`]?
///
///  * Individual transfers give you individual `Future`s, which you then have
///    to keep track of and poll using something like `FuturesUnordered`.
///  * A `Queue` provides better cancellation semantics than `Future`'s
///    cancel-on-drop.
///     * After dropping a [`TransferFuture`][super::TransferFuture], you lose
///       the ability to get the status of the cancelled transfer and see if it
///       may have been partially or fully completed.
///     * When cancelling multiple transfers, it's important to do so in reverse
///       order so that subsequent pending transfers can't end up executing.
///       When managing a collection of `TransferFuture`s it's tricky to
///       guarantee drop order, while `Queue` always cancels its contained
///       transfers in reverse order.
///     * The `TransferFuture` methods on `Interface` are not [cancel-safe],
///       meaning they cannot be used in `select!{}` or similar patterns,
///       because dropping the Future has side effects and can lose data. The
///       Future returned from [`Queue::next_complete`] is cancel-safe because
///       it merely waits for completion, while the `Queue` owns the pending
///       transfers.
///  * A queue caches the internal transfer data structures of the last
///    completed transfer, meaning that if you re-use the data buffer there is
///    no memory allocation involved in continued streaming.
///
/// [cancel-safe]: https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety
/// ### Example (read from an endpoint)
///
/// ```no_run
/// # #[pollster::main]
/// # async fn main() {
///     use nusb::transfer::RequestBuffer;
///     # let di = nusb::list_devices().await.unwrap().next().unwrap();
///     # let device = di.open().await.unwrap();
///     # let interface = device.claim_interface(0).await.unwrap();
///     # fn handle_data(_: &[u8]) {}
///     let mut queue = interface.bulk_in_queue(0x81);
///
///     let n_transfers = 8;
///     let transfer_size = 256;
///
///     while queue.pending() < n_transfers {
///         queue.submit(RequestBuffer::new(transfer_size));
///     }
///
///     loop {
///         let completion = queue.next_complete().await;
///         handle_data(&completion.data); // your function
///
///         if completion.status.is_err() {
///             break;
///         }
///         
///         queue.submit(RequestBuffer::reuse(completion.data, transfer_size))
///     }
/// # }
/// ```
///
/// ### Example (write to an endpoint)
/// ```no_run
/// # #[pollster::main]
/// # async fn main() {
///     use std::mem;
///     # let di = nusb::list_devices().await.unwrap().next().unwrap();
///     # let device = di.open().await.unwrap();
///     # let interface = device.claim_interface(0).await.unwrap();
///     # fn fill_data(_: &mut Vec<u8>) {}
///     # fn data_confirmed_sent(_: usize) {}
///     let mut queue = interface.bulk_out_queue(0x02);
///
///     let n_transfers = 8;
///
///     let mut next_buf = Vec::new();
///
///     loop {
///         while queue.pending() < n_transfers {
///             let mut buf = mem::replace(&mut next_buf, Vec::new());
///             fill_data(&mut buf); // your function
///             queue.submit(buf);
///         }
///
///         let completion = queue.next_complete().await;
///         data_confirmed_sent(completion.data.actual_length()); // your function
///         next_buf = completion.data.reuse();
///         if completion.status.is_err() {
///             break;
///         }
///     }
/// # }
/// ```
pub struct Queue<R: TransferRequest> {
    interface: Arc<platform::Interface>,
    endpoint: u8,
    endpoint_type: EndpointType,

    /// A queue of pending transfers, expected to complete in order
    pending: VecDeque<TransferHandle<platform::TransferData>>,

    /// An idle transfer that recently completed for re-use.
    cached: Option<TransferHandle<platform::TransferData>>,

    bufs: PhantomData<R>,
}

impl<R> Queue<R>
where
    R: TransferRequest + crate::maybe::MaybeSend + crate::maybe::MaybeSync,
    platform::TransferData: PlatformSubmit<R>,
{
    pub(crate) fn new(
        interface: Arc<platform::Interface>,
        endpoint: u8,
        endpoint_type: EndpointType,
    ) -> Queue<R> {
        Queue {
            interface,
            endpoint,
            endpoint_type,
            pending: VecDeque::new(),
            cached: None,
            bufs: PhantomData,
        }
    }

    /// Submit a new transfer on the endpoint.
    ///
    /// For an `IN` endpoint, pass a [`RequestBuffer`][`super::RequestBuffer`].\
    /// For an `OUT` endpoint, pass a [`Vec<u8>`].
    pub fn submit(&mut self, data: R) {
        let mut transfer = self.cached.take().unwrap_or_else(|| {
            self.interface
                .make_transfer(self.endpoint, self.endpoint_type)
        });
        transfer.submit(data);
        self.pending.push_back(transfer);
    }

    /// Return a `Future` that waits for the next pending transfer to complete, and yields its
    /// buffer and status.
    ///
    /// For an `IN` endpoint, the completion contains a [`Vec<u8>`].\
    /// For an `OUT` endpoint, the completion contains a [`ResponseBuffer`][`super::ResponseBuffer`].
    ///
    /// This future is cancel-safe: it can be cancelled and re-created without
    /// side effects, enabling its use in `select!{}` or similar.
    ///
    /// Panics if there are no transfers pending.
    pub fn next_complete(
        &mut self,
    ) -> impl Future<Output = Completion<R::Response>>
           + Unpin
           + crate::maybe::MaybeSend
           + crate::maybe::MaybeSync
           + '_ {
        poll_fn(|cx| self.poll_next(cx))
    }

    /// Get the next pending transfer if one has completed, or register the
    /// current task for wakeup when the next transfer completes.
    ///
    /// For an `IN` endpoint, the completion contains a [`Vec<u8>`].\
    /// For an `OUT` endpoint, the completion contains a
    /// [`ResponseBuffer`][`super::ResponseBuffer`].
    ///
    /// Panics if there are no transfers pending.
    pub fn poll_next(&mut self, cx: &mut Context) -> Poll<Completion<R::Response>> {
        let res = self
            .pending
            .front_mut()
            .expect("queue should have pending transfers when calling next_complete")
            .poll_completion::<R>(cx);
        if res.is_ready() {
            self.cached = self.pending.pop_front();
        }
        res
    }

    /// Get the number of transfers that have been submitted with `submit` that
    /// have not yet been returned from `next_complete`.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Request cancellation of all pending transfers.
    ///
    /// The transfers will still be returned from subsequent calls to
    /// `next_complete` so you can tell which were completed,
    /// partially-completed, or cancelled.
    pub fn cancel_all(&mut self) {
        // Cancel transfers in reverse order to ensure subsequent transfers
        // can't complete out of order while we're going through them.
        for transfer in self.pending.iter_mut().rev() {
            transfer.cancel();
        }
    }

    /// Clear the endpoint's halt / stall condition.
    ///
    /// Sends a `CLEAR_FEATURE` `ENDPOINT_HALT` control transfer to tell the
    /// device to reset the endpoint's data toggle and clear the halt / stall
    /// condition, and resets the host-side data toggle.
    ///
    /// Use this after receiving
    /// [`TransferError::Stall`][crate::transfer::TransferError::Stall] to clear
    /// the error and resume use of the endpoint.
    ///
    /// This should not be called when transfers are pending on the endpoint.
    pub async fn clear_halt(&mut self) -> Result<(), Error> {
        self.interface.clear_halt(self.endpoint).await
    }
}

impl<R: TransferRequest> Drop for Queue<R> {
    fn drop(&mut self) {
        // Cancel transfers in reverse order to ensure subsequent transfers
        // can't complete out of order while we're going through them.
        self.pending.drain(..).rev().for_each(drop)
    }
}
