use std::{
    error::Error,
    io::{BufRead, Read},
    time::Duration,
};

#[cfg(any(feature = "tokio", feature = "smol"))]
use std::{
    pin::Pin,
    task::{ready, Context, Poll},
};

use crate::{
    transfer::{Buffer, BulkOrInterrupt, In, TransferError},
    Endpoint,
};

/// Wrapper for a Bulk or Interrupt IN [`Endpoint`](crate::Endpoint) that
/// manages transfers to provide a higher-level buffered API.
///
/// Most of the functionality of this type is provided through standard IO
/// traits; you'll want to use one of the following:
///
/// * [`std::io::Read`](std::io::Read) and [`BufRead`](std::io::BufRead) for
///   blocking IO.
/// * With the `tokio` cargo feature,
///   [`tokio::io::AsyncRead`](tokio::io::AsyncRead) and
///   [`AsyncBufRead`](tokio::io::AsyncBufRead) for async IO. Tokio also
///   provides `AsyncReadExt` and `AsyncBufReadExt` with additional methods.
/// * With the `smol` cargo feature,
///   [`futures_io::AsyncRead`](futures_io::AsyncRead) and
///   [`AsyncBufRead`](futures_io::AsyncBufRead) for async IO.
///   `futures_lite` provides `AsyncReadExt` and `AsyncBufReadExt` with
///   additional methods.
///
/// By default, this type ignores USB packet lengths and boundaries. For protocols
/// that use short or zero-length packets as delimiters, you can use the
/// [`until_short_packet()`](Self::until_short_packet) method to get an
/// [`EndpointReadUntilShortPacket`](EndpointReadUntilShortPacket) adapter
/// that observes these delimiters.
pub struct EndpointRead<EpType: BulkOrInterrupt> {
    endpoint: Endpoint<EpType, In>,
    reading: Option<ReadBuffer>,
    num_transfers: usize,
    transfer_size: usize,
    read_timeout: Duration,
}

struct ReadBuffer {
    pos: usize,
    buf: Buffer,
    status: Result<(), TransferError>,
}

impl ReadBuffer {
    #[inline]
    fn error(&self) -> Option<TransferError> {
        self.status.err().filter(|e| *e != TransferError::Cancelled)
    }

    #[inline]
    fn has_remaining(&self) -> bool {
        self.pos < self.buf.len() || self.error().is_some()
    }

    #[inline]
    fn has_remaining_or_short_end(&self) -> bool {
        self.pos < self.buf.requested_len() || self.error().is_some()
    }

    #[inline]
    fn clear_short_packet(&mut self) {
        self.pos = usize::MAX
    }

    #[inline]
    fn remaining(&self) -> Result<&[u8], std::io::Error> {
        let remaining = &self.buf[self.pos..];
        match (remaining.len(), self.error()) {
            (0, Some(e)) => Err(e.into()),
            _ => Ok(remaining),
        }
    }

    #[inline]
    fn consume(&mut self, len: usize) {
        let remaining = self.buf.len().saturating_sub(self.pos);
        assert!(len <= remaining, "consumed more than available");
        self.pos += len;
    }
}

fn copy_min(dest: &mut [u8], src: &[u8]) -> usize {
    let len = dest.len().min(src.len());
    dest[..len].copy_from_slice(&src[..len]);
    len
}

impl<EpType: BulkOrInterrupt> EndpointRead<EpType> {
    /// Create a new `EndpointRead` wrapping the given endpoint.
    ///
    /// The `transfer_size` parameter is the size of the buffer passed to the OS
    /// for each transfer. It will be rounded up to the next multiple of the
    /// endpoint's max packet size.
    pub fn new(endpoint: Endpoint<EpType, In>, transfer_size: usize) -> Self {
        let packet_size = endpoint.max_packet_size();
        let transfer_size = (transfer_size.div_ceil(packet_size)).max(1) * packet_size;

        Self {
            endpoint,
            reading: None,
            num_transfers: 1,
            transfer_size,
            read_timeout: Duration::MAX,
        }
    }

    /// Set the number of concurrent transfers.
    ///
    /// A value of 1 (default) means that transfers will only be submitted when
    /// calling `read()` or `fill_buf()` and the buffer is empty. To maximize
    /// throughput, a value of 2 or more is recommended for applications that
    /// stream data continuously so that the host controller can continue to
    /// receive data while the application processes the data from a completed
    /// transfer.
    ///
    /// A value of 0 means no further transfers will be submitted. Existing
    /// transfers will complete normally, and subsequent calls to `read()` and
    /// `fill_buf()` will return zero bytes (EOF).
    ///
    /// This submits more transfers when increasing the number, but does not
    /// [cancel transfers](Self::cancel_all) when decreasing it.
    pub fn set_num_transfers(&mut self, num_transfers: usize) {
        self.num_transfers = num_transfers;

        // Leave the last transfer to be submitted by `read` such that
        // a value of `1` only has transfers pending within `read` calls.
        while self.endpoint.pending() < num_transfers.saturating_sub(1) {
            let buf = self.endpoint.allocate(self.transfer_size);
            self.endpoint.submit(buf);
        }
    }

    /// Set the number of concurrent transfers.
    ///
    /// See [Self::set_num_transfers] (this version is for method chaining).
    pub fn with_num_transfers(mut self, num_transfers: usize) -> Self {
        self.set_num_transfers(num_transfers);
        self
    }

    /// Set the timeout for waiting for a transfer in the blocking `read` APIs.
    ///
    /// This affects the `std::io::Read` and `std::io::BufRead` implementations
    /// only, and not the async trait implementations.
    ///
    /// When a timeout occurs, the call fails but the transfer is not cancelled
    /// and may complete later if the read is retried.
    pub fn set_read_timeout(&mut self, timeout: Duration) {
        self.read_timeout = timeout;
    }

    /// Set the timeout for an individual transfer for the blocking `read` APIs.
    ///
    /// See [Self::set_read_timeout] -- this is for method chaining with `EndpointWrite::new()`.
    pub fn with_read_timeout(mut self, timeout: Duration) -> Self {
        self.set_read_timeout(timeout);
        self
    }

    /// Cancel all pending transfers.
    ///
    /// This sets [`num_transfers`](Self::set_num_transfers) to 0, so no further
    /// transfers will be submitted. Any data buffered before the transfers are cancelled
    /// can be read, and then the read methods will return 0 bytes (EOF).
    ///
    /// Call [`num_transfers`](Self::set_num_transfers) with a non-zero value
    /// to resume receiving data.
    pub fn cancel_all(&mut self) {
        self.num_transfers = 0;
        self.endpoint.cancel_all();
    }

    /// Destroy this `EndpointRead` and return the underlying [`Endpoint`].
    ///
    /// Any pending transfers are not cancelled.
    pub fn into_inner(self) -> Endpoint<EpType, In> {
        self.endpoint
    }

    /// Get an [`EndpointReadUntilShortPacket`] adapter that will read only until
    /// the end of a short or zero-length packet.
    ///
    /// Some USB protocols use packets shorter than the endpoint's max packet size
    /// as a delimiter marking the end of a message. By default, [`EndpointRead`]
    /// ignores packet boundaries, but this adapter allows you to observe these
    /// delimiters.
    pub fn until_short_packet(&mut self) -> EndpointReadUntilShortPacket<'_, EpType> {
        EndpointReadUntilShortPacket { reader: self }
    }

    #[inline]
    fn has_data(&self) -> bool {
        self.reading.as_ref().is_some_and(|r| r.has_remaining())
    }

    #[inline]
    fn has_data_or_short_end(&self) -> bool {
        self.reading
            .as_ref()
            .is_some_and(|r| r.has_remaining_or_short_end())
    }

    fn resubmit(&mut self) {
        if let Some(c) = self.reading.take() {
            debug_assert!(!c.has_remaining());
            self.endpoint.submit(c.buf);
        }
    }

    fn start_read(&mut self) -> bool {
        if self.endpoint.pending() < self.num_transfers {
            // Re-use the last completed buffer if available
            self.resubmit();
            while self.endpoint.pending() < self.num_transfers {
                // Allocate more buffers for any remaining transfers
                let buf = self.endpoint.allocate(self.transfer_size);
                self.endpoint.submit(buf);
            }
        }

        // If num_transfers is 0 and all transfers are complete
        self.endpoint.pending() > 0
    }

    #[inline]
    fn remaining(&self) -> Result<&[u8], std::io::Error> {
        self.reading.as_ref().unwrap().remaining()
    }

    #[inline]
    fn consume(&mut self, len: usize) {
        if let Some(ref mut c) = self.reading {
            c.consume(len);
        } else {
            assert!(len == 0, "consumed more than available");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn wait(&mut self) -> Result<bool, std::io::Error> {
        if self.start_read() {
            let c = self.endpoint.wait_next_complete(self.read_timeout);
            let c = c.ok_or(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timeout waiting for read",
            ))?;
            self.reading = Some(ReadBuffer {
                pos: 0,
                buf: c.buffer,
                status: c.status,
            });
            Ok(true)
        } else {
            Ok(false)
        }
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<bool> {
        if self.start_read() {
            let c = ready!(self.endpoint.poll_next_complete(cx));
            self.reading = Some(ReadBuffer {
                pos: 0,
                buf: c.buffer,
                status: c.status,
            });
            Poll::Ready(true)
        } else {
            Poll::Ready(false)
        }
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    #[inline]
    fn poll_fill_buf(&mut self, cx: &mut Context<'_>) -> Poll<Result<&[u8], std::io::Error>> {
        while !self.has_data() {
            if !ready!(self.poll(cx)) {
                return Poll::Ready(Ok(&[]));
            }
        }
        Poll::Ready(self.remaining())
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    #[inline]
    fn poll_fill_buf_until_short(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<&[u8], std::io::Error>> {
        while !self.has_data_or_short_end() {
            if !ready!(self.poll(cx)) {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "ended without short packet",
                )));
            }
        }
        Poll::Ready(self.remaining())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<EpType: BulkOrInterrupt> Read for EndpointRead<EpType> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let remaining = self.fill_buf()?;
        let len = copy_min(buf, remaining);
        self.consume(len);
        Ok(len)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<EpType: BulkOrInterrupt> BufRead for EndpointRead<EpType> {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8], std::io::Error> {
        while !self.has_data() {
            if !self.wait()? {
                return Ok(&[]);
            }
        }
        self.remaining()
    }

    #[inline]
    fn consume(&mut self, len: usize) {
        self.consume(len);
    }
}

#[cfg(feature = "tokio")]
impl<EpType: BulkOrInterrupt> tokio::io::AsyncRead for EndpointRead<EpType> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let this = Pin::into_inner(self);
        let remaining = ready!(this.poll_fill_buf(cx))?;
        let len = remaining.len().min(buf.remaining());
        buf.put_slice(&remaining[..len]);
        this.consume(len);
        Poll::Ready(Ok(()))
    }
}

#[cfg(feature = "tokio")]
impl<EpType: BulkOrInterrupt> tokio::io::AsyncBufRead for EndpointRead<EpType> {
    fn poll_fill_buf(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<&[u8], std::io::Error>> {
        Pin::into_inner(self).poll_fill_buf(cx)
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        Pin::into_inner(self).consume(amt);
    }
}

#[cfg(feature = "smol")]
impl<EpType: BulkOrInterrupt> futures_io::AsyncRead for EndpointRead<EpType> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let this = Pin::into_inner(self);
        let remaining = ready!(this.poll_fill_buf(cx))?;
        let len = copy_min(buf, remaining);
        this.consume(len);
        Poll::Ready(Ok(len))
    }
}

#[cfg(feature = "smol")]
impl<EpType: BulkOrInterrupt> futures_io::AsyncBufRead for EndpointRead<EpType> {
    fn poll_fill_buf(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<&[u8], std::io::Error>> {
        Pin::into_inner(self).poll_fill_buf(cx)
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        Pin::into_inner(self).consume(amt);
    }
}

/// Adapter for [`EndpointRead`] that ends after a short or zero-length packet.
///
/// This can be obtained from [`EndpointRead::until_short_packet()`]. It does
/// have any state other than that of the underlying [`EndpointRead`], so
/// dropping and re-creating with another call to
/// [`EndpointRead::until_short_packet()`] has no effect.
///
/// This implements the same traits as `EndpointRead` but observes packet
/// boundaries instead of ignoring them.
pub struct EndpointReadUntilShortPacket<'a, EpType: BulkOrInterrupt> {
    reader: &'a mut EndpointRead<EpType>,
}

/// Error returned by [`EndpointReadUntilShortPacket::consume_end()`]
/// when the reader is not at the end of a short packet.
#[derive(Debug)]
pub struct ExpectedShortPacket;

impl std::fmt::Display for ExpectedShortPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "expected short packet")
    }
}

impl Error for ExpectedShortPacket {}

impl<EpType: BulkOrInterrupt> EndpointReadUntilShortPacket<'_, EpType> {
    /// Check if the endpoint has reached the end of a short packet.
    ///
    /// Upon reading the end of a short packet, the next `read()` or
    /// `fill_buf()` will return 0 bytes (EOF) and this method will return
    /// `true`. To begin reading the next message, call `consume_end()`.
    pub fn is_end(&self) -> bool {
        self.reader
            .reading
            .as_ref()
            .is_some_and(|r| !r.has_remaining() && r.has_remaining_or_short_end())
    }

    /// Consume the end of a short packet.
    ///
    /// Use this after `read()` or `fill_buf()` have returned EOF to reset the reader
    /// to read the next message.
    ///
    /// Returns an error and does nothing if the reader [is not at the end of a short packet](Self::is_end).
    pub fn consume_end(&mut self) -> Result<(), ExpectedShortPacket> {
        if self.is_end() {
            self.reader.reading.as_mut().unwrap().clear_short_packet();
            Ok(())
        } else {
            Err(ExpectedShortPacket)
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<EpType: BulkOrInterrupt> Read for EndpointReadUntilShortPacket<'_, EpType> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let remaining = self.fill_buf()?;
        let len = copy_min(buf, remaining);
        self.reader.consume(len);
        Ok(len)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<EpType: BulkOrInterrupt> BufRead for EndpointReadUntilShortPacket<'_, EpType> {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8], std::io::Error> {
        while !self.reader.has_data_or_short_end() {
            if !self.reader.wait()? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "ended without short packet",
                ));
            }
        }
        self.reader.remaining()
    }

    #[inline]
    fn consume(&mut self, len: usize) {
        if self.reader.has_data_or_short_end() {
            assert!(len == 0, "consumed more than available");
        } else {
            self.reader.consume(len);
        }
    }
}

#[cfg(feature = "tokio")]
impl<EpType: BulkOrInterrupt> tokio::io::AsyncRead for EndpointReadUntilShortPacket<'_, EpType> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let this = Pin::into_inner(self);
        let remaining = ready!(this.reader.poll_fill_buf_until_short(cx))?;
        let len = remaining.len().min(buf.remaining());
        buf.put_slice(&remaining[..len]);
        this.reader.consume(len);
        Poll::Ready(Ok(()))
    }
}

#[cfg(feature = "tokio")]
impl<EpType: BulkOrInterrupt> tokio::io::AsyncBufRead for EndpointReadUntilShortPacket<'_, EpType> {
    fn poll_fill_buf(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<&[u8], std::io::Error>> {
        Pin::into_inner(self).reader.poll_fill_buf(cx)
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        Pin::into_inner(self).reader.consume(amt);
    }
}

#[cfg(feature = "smol")]
impl<EpType: BulkOrInterrupt> futures_io::AsyncRead for EndpointReadUntilShortPacket<'_, EpType> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let this = Pin::into_inner(self);
        let remaining = ready!(this.reader.poll_fill_buf_until_short(cx))?;
        let len = copy_min(buf, remaining);
        this.reader.consume(len);
        Poll::Ready(Ok(len))
    }
}

#[cfg(feature = "smol")]
impl<EpType: BulkOrInterrupt> futures_io::AsyncBufRead
    for EndpointReadUntilShortPacket<'_, EpType>
{
    fn poll_fill_buf(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<&[u8], std::io::Error>> {
        Pin::into_inner(self).reader.poll_fill_buf(cx)
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        Pin::into_inner(self).reader.consume(amt);
    }
}
