use crate::{
    transfer::{Buffer, BulkOrInterrupt, Completion, Out},
    Endpoint,
};
use std::{
    future::poll_fn,
    io::{Error, ErrorKind, Write},
    task::{Context, Poll},
    time::Duration,
};

#[cfg(any(feature = "tokio", feature = "smol"))]
use std::{pin::Pin, task::ready};

/// Wrapper for a Bulk or Interrupt OUT [`Endpoint`](crate::Endpoint) that
/// manages transfers to provide a higher-level buffered API.
///
/// Most of the functionality of this type is provided through standard IO
/// traits; you'll want to use one of the following:
///
/// * [`std::io::Write`](std::io::Write) for blocking IO.
/// * With the `tokio` cargo feature,
///   [`tokio::io::AsyncWrite`](tokio::io::AsyncWrite). Tokio also provides
///   `AsyncWriteExt` with additional methods.
/// * With the `smol` cargo feature,
///   [`futures_io::AsyncWrite`](futures_io::AsyncWrite) for async IO.
///   `futures_lite` provides `AsyncWriteExt` with additional methods.
///
/// Written data is buffered and may not be sent until the buffer is full or
/// [`submit`](Self::submit) / [`submit_end`](Self::submit_end) or
/// [`flush`](Self::flush) / [`flush_end`](Self::flush_end) are called.
pub struct EndpointWrite<EpType: BulkOrInterrupt> {
    endpoint: Endpoint<EpType, Out>,
    writing: Option<Buffer>,
    transfer_size: usize,
    num_transfers: usize,
    write_timeout: Duration,
}

impl<EpType: BulkOrInterrupt> EndpointWrite<EpType> {
    /// Create a new `EndpointWrite` wrapping the given endpoint.
    ///
    /// The `transfer_size` parameter is the size of the buffer passed to the OS
    /// for each transfer. It will be rounded up to the next multiple of the
    /// endpoint's max packet size. Data will be buffered and sent in chunks of
    /// this size, unless `flush` or [`submit`](Self::submit) are
    /// called to force sending a partial buffer immediately.
    pub fn new(endpoint: Endpoint<EpType, Out>, transfer_size: usize) -> Self {
        let packet_size = endpoint.max_packet_size();
        let transfer_size = (transfer_size.div_ceil(packet_size)).max(1) * packet_size;

        Self {
            endpoint,
            writing: None,
            transfer_size,
            num_transfers: 1,
            write_timeout: Duration::MAX,
        }
    }

    /// Set the maximum number of transfers that can be queued with the OS
    /// before backpressure is applied.
    ///
    /// If more than `num_transfers` transfers are pending, calls to `write`
    /// will block or async methods will return `Pending` until a transfer
    /// completes.
    ///
    /// Panics if `num_transfers` is zero.
    pub fn set_num_transfers(&mut self, num_transfers: usize) {
        assert!(num_transfers > 0, "num_transfers must be greater than zero");
        self.num_transfers = num_transfers;
    }

    /// Set the maximum number of transfers that can be queued with the OS
    /// before backpressure is applied.
    ///
    /// See [Self::set_num_transfers] -- this is for method chaining with `EndpointWrite::new()`.
    pub fn with_num_transfers(mut self, num_transfers: usize) -> Self {
        self.set_num_transfers(num_transfers);
        self
    }

    /// Set the timeout for a transfer in the blocking `write` APIs.
    ///
    /// This affects the `std::io::Write` implementation only, and not the async
    /// trait implementations.
    ///
    /// When a timeout occurs, writing new data fails but transfers for
    /// previously-written data are not cancelled. The data passed in the failed
    /// `write` call is not written to the buffer, though note that functions
    /// like `write_all` that call `write` multiple times may have successfully
    /// written some of the data.
    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = timeout;
    }

    /// Set the timeout for an individual transfer for the blocking `write` APIs.
    ///
    /// See [Self::set_write_timeout] -- this is for method chaining with `EndpointWrite::new()`.
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.set_write_timeout(timeout);
        self
    }

    /// Destroy this `EndpointWrite` and return the underlying [`Endpoint`].
    ///
    /// Any pending transfers are not cancelled.
    pub fn into_inner(self) -> Endpoint<EpType, Out> {
        self.endpoint
    }

    fn handle_completion(&mut self, c: Completion) -> Result<(), Error> {
        debug_assert_eq!(self.writing.as_ref().map_or(0, |b| b.len()), 0);
        let mut buf = c.buffer;
        if buf.capacity() > 0 && self.endpoint.pending() < self.num_transfers {
            debug_assert!(buf.capacity() == self.transfer_size);
            buf.clear();
            self.writing = Some(buf);
        }
        Ok(c.status?)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn wait_one(&mut self) -> Result<(), Error> {
        let t = self.endpoint.wait_next_complete(self.write_timeout);
        let t = t.ok_or_else(|| Error::new(ErrorKind::TimedOut, "write timeout"))?;
        self.handle_completion(t)
    }

    fn poll_one(&mut self, cx: &mut Context) -> Poll<Result<(), Error>> {
        self.endpoint
            .poll_next_complete(cx)
            .map(|c| self.handle_completion(c))
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    fn poll_write(&mut self, cx: &mut Context<'_>, src: &[u8]) -> Poll<Result<usize, Error>> {
        let buf = loop {
            if let Some(buf) = self.writing.as_mut() {
                break buf;
            }
            if self.endpoint.pending() < self.num_transfers {
                self.writing = Some(self.endpoint.allocate(self.transfer_size));
            } else {
                ready!(self.poll_one(cx))?;
            }
        };

        let len = src.len().min(buf.remaining_capacity());
        buf.extend_from_slice(&src[..len]);

        if buf.remaining_capacity() == 0 {
            self.endpoint.submit(self.writing.take().unwrap());
        }

        Poll::Ready(Ok(len))
    }

    /// Submit any buffered data to the OS immediately.
    ///
    /// This submits the current buffer even if it not full, but does not wait
    /// for the transfer to complete or confirm that it was successful (see
    /// [Write::flush]). If the buffer is empty, this does nothing.
    pub fn submit(&mut self) {
        if self.writing.as_ref().is_some_and(|b| !b.is_empty()) {
            self.endpoint.submit(self.writing.take().unwrap())
        }
    }

    /// Submit any buffered data to the OS immediately, terminating with a short
    /// or zero-length packet.
    ///
    /// Some USB protocols use packets shorter than the endpoint's max packet
    /// size as a delimiter marking the end of a message. This method forces
    /// such a delimiter by adding a zero-length packet if the current buffer is
    /// a multiple of the endpoint's max packet size.
    ///
    /// This does not wait for the transfer to complete or confirm that it was
    /// successful (see [Self::flush_end]). If the buffer is empty, this sends a
    /// zero-length packet.
    pub fn submit_end(&mut self) {
        let zlp = if let Some(t) = self.writing.take() {
            let len = t.len();
            self.endpoint.submit(t);
            len != 0 && len % self.endpoint.max_packet_size() == 0
        } else {
            true
        };

        if zlp {
            self.endpoint.submit(Buffer::new(0));
        }
    }

    /// Submit any buffered data immediately and wait for all pending transfers
    /// to complete or fail.
    #[cfg(not(target_arch = "wasm32"))]
    fn flush_blocking(&mut self) -> Result<(), Error> {
        self.submit();
        while self.endpoint.pending() > 0 {
            self.wait_one()?;
        }
        Ok(())
    }

    /// Submit any buffered data immediately, terminating with a short or
    /// zero-length packet, and wait for all pending transfers to complete or
    /// fail.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn flush_end(&mut self) -> Result<(), Error> {
        self.submit_end();
        while self.endpoint.pending() > 0 {
            self.wait_one()?;
        }
        Ok(())
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.submit();
        while self.endpoint.pending() > 0 {
            ready!(self.poll_one(cx))?;
        }
        Poll::Ready(Ok(()))
    }

    /// Submit any buffered data immediately, terminating with a short or zero-length
    /// packet, and wait for all pending transfers to complete.
    ///
    /// Async version of [Self::flush_end].
    pub async fn flush_end_async(&mut self) -> Result<(), Error> {
        self.submit_end();
        while self.endpoint.pending() > 0 {
            poll_fn(|cx| self.poll_one(cx)).await?;
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<EpType: BulkOrInterrupt> Write for EndpointWrite<EpType> {
    /// Write data to the endpoint.
    ///
    /// Data is buffered and not written until the buffer is full or `submit()`
    /// or `flush()` are called. Writing will block if there are already too
    /// many transfers pending, as configured by
    /// [`set_num_transfers`][EndpointWrite::set_num_transfers].
    fn write(&mut self, src: &[u8]) -> std::io::Result<usize> {
        let buf = loop {
            if let Some(buf) = self.writing.as_mut() {
                break buf;
            }
            if self.endpoint.pending() < self.num_transfers {
                self.writing = Some(self.endpoint.allocate(self.transfer_size));
            } else {
                self.wait_one()?
            }
        };

        let len = src.len().min(buf.remaining_capacity());
        buf.extend_from_slice(&src[..len]);

        if buf.remaining_capacity() == 0 {
            self.endpoint.submit(self.writing.take().unwrap());
        }

        Ok(len)
    }

    /// Submit any buffered data immediately and wait for all pending transfers
    /// to complete or fail.
    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_blocking()
    }
}

#[cfg(feature = "smol")]
impl<EpType: BulkOrInterrupt> futures_io::AsyncWrite for EndpointWrite<EpType> {
    /// Write data to the endpoint.
    ///
    /// Data is buffered and not written until the buffer is full or `submit()`
    /// or `flush()` are called. Writing will return [`Poll::Pending`] if there
    /// are already too many transfers pending, as configured by
    /// [`set_num_transfers`][EndpointWrite::set_num_transfers].
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::into_inner(self).poll_write(cx, buf)
    }

    /// Submit any buffered data immediately and wait for all pending transfers
    /// to complete or fail.
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::into_inner(self).poll_flush(cx)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::into_inner(self).poll_flush(cx)
    }
}

#[cfg(feature = "tokio")]
impl<EpType: BulkOrInterrupt> tokio::io::AsyncWrite for EndpointWrite<EpType> {
    /// Write data to the endpoint.
    ///
    /// Data is buffered and not written until the buffer is full or `submit()`
    /// or `flush()` are called. Writing will return [`Poll::Pending`] if there
    /// are already too many transfers pending, as configured by
    /// [`set_num_transfers`][EndpointWrite::set_num_transfers].
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::into_inner(self).poll_write(cx, buf)
    }

    /// Submit any buffered data immediately and wait for all pending transfers
    /// to complete or fail.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::into_inner(self).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::into_inner(self).poll_flush(cx)
    }
}
