use std::{
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
/// implements [`Read`](std::io::Read) and [`BufRead`](std::io::BufRead) and
/// their `tokio` and `smol` async equivalents.
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
            num_transfers: 0,
            transfer_size,
            read_timeout: Duration::MAX,
        }
    }

    /// Set the number of transfers to maintain pending at all times.
    ///
    /// A value of 0 means that transfers will only be submitted when calling
    /// `read()` or `fill_buf()` and the buffer is empty. To maximize throughput,
    /// a value of 2 or more is recommended for applications that stream data
    /// continuously.
    ///
    /// This submits more transfers when increasing the number, but does not
    /// cancel transfers when decreasing it.
    pub fn set_num_transfers(&mut self, num_transfers: usize) {
        self.num_transfers = num_transfers;

        while self.endpoint.pending() < num_transfers {
            let buf = self.endpoint.allocate(self.transfer_size);
            self.endpoint.submit(buf);
        }
    }

    /// Set the number of transfers to maintain pending at all times.
    ///
    /// See [Self::set_num_transfers] -- this is for method chaining with `EndpointRead::new()`.
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
    /// They will be re-submitted on the next read.
    pub fn cancel_all(&mut self) {
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
    pub fn until_short_packet(&mut self) -> EndpointReadUntilShortPacket<EpType> {
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

    fn start_read(&mut self) {
        let t = usize::max(1, self.num_transfers);
        if self.endpoint.pending() < t {
            self.resubmit();
            while self.endpoint.pending() < t {
                let buf = self.endpoint.allocate(self.transfer_size);
                self.endpoint.submit(buf);
            }
        }
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

    fn wait(&mut self) -> Result<(), std::io::Error> {
        self.start_read();
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
        Ok(())
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.start_read();
        let c = ready!(self.endpoint.poll_next_complete(cx));
        self.reading = Some(ReadBuffer {
            pos: 0,
            buf: c.buffer,
            status: c.status,
        });
        Poll::Ready(())
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    #[inline]
    fn poll_fill_buf(&mut self, cx: &mut Context<'_>) -> Poll<Result<&[u8], std::io::Error>> {
        while !self.has_data() {
            ready!(self.poll(cx));
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
            ready!(self.poll(cx));
        }
        Poll::Ready(self.remaining())
    }
}

impl<EpType: BulkOrInterrupt> Read for EndpointRead<EpType> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let remaining = self.fill_buf()?;
        let len = copy_min(buf, remaining);
        self.consume(len);
        Ok(len)
    }
}

impl<EpType: BulkOrInterrupt> BufRead for EndpointRead<EpType> {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8], std::io::Error> {
        while !self.has_data() {
            self.wait()?;
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
pub struct EndpointReadUntilShortPacket<'a, EpType: BulkOrInterrupt> {
    reader: &'a mut EndpointRead<EpType>,
}

impl<EpType: BulkOrInterrupt> EndpointReadUntilShortPacket<'_, EpType> {
    /// Check if the underlying endpoint has reached the end of a short packet.
    ///
    /// Upon reading the end of a short packet, the next `read()` or `fill_buf()`
    /// will return 0 bytes (EOF). To read the next message, call `consume_end()`.
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
    pub fn consume_end(&mut self) -> Result<(), ()> {
        if self.is_end() {
            self.reader.reading.as_mut().unwrap().clear_short_packet();
            Ok(())
        } else {
            Err(())
        }
    }
}

impl<EpType: BulkOrInterrupt> Read for EndpointReadUntilShortPacket<'_, EpType> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let remaining = self.fill_buf()?;
        let len = copy_min(buf, remaining);
        self.reader.consume(len);
        Ok(len)
    }
}

impl<EpType: BulkOrInterrupt> BufRead for EndpointReadUntilShortPacket<'_, EpType> {
    #[inline]
    fn fill_buf(&mut self) -> Result<&[u8], std::io::Error> {
        while !self.reader.has_data_or_short_end() {
            self.reader.wait()?;
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
