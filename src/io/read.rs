use std::{
    io::{BufRead, Read},
    pin::Pin,
    task::{ready, Context, Poll},
    time::Duration,
};

use crate::{
    transfer::{Buffer, BulkOrInterrupt, In, TransferError},
    Endpoint,
};

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
    fn has_remaining(&self) -> bool {
        self.pos < self.buf.len() || self.error().is_some()
    }

    #[inline]
    fn error(&self) -> Option<TransferError> {
        self.status.err().filter(|e| *e != TransferError::Cancelled)
    }

    #[inline]
    fn is_short_packet(&self) -> bool {
        self.buf.len() < self.buf.requested_len()
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
        let remaining = self.buf.len() - self.pos;
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
    pub fn set_num_transfers(&mut self, num_transfers: usize) {
        self.num_transfers = num_transfers;
    }

    pub fn with_num_transfers(mut self, num_transfers: usize) -> Self {
        self.set_num_transfers(num_transfers);
        self
    }

    pub fn cancel_all(&mut self) {
        self.endpoint.cancel_all();
    }

    pub fn into_inner(self) -> Endpoint<EpType, In> {
        self.endpoint
    }

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
            .is_some_and(|r| r.has_remaining() || r.is_short_packet())
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

    #[inline]
    fn poll_fill_buf(&mut self, cx: &mut Context<'_>) -> Poll<Result<&[u8], std::io::Error>> {
        while !self.has_data() {
            ready!(self.poll(cx));
        }
        Poll::Ready(self.remaining())
    }

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
        self: std::pin::Pin<&mut Self>,
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
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<&[u8], std::io::Error>> {
        Pin::into_inner(self).poll_fill_buf(cx)
    }

    fn consume(self: std::pin::Pin<&mut Self>, amt: usize) {
        Pin::into_inner(self).consume(amt);
    }
}

#[cfg(feature = "smol")]
impl<EpType: BulkOrInterrupt> futures_io::AsyncRead for EndpointRead<EpType> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
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
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<&[u8], std::io::Error>> {
        Pin::into_inner(self).poll_fill_buf(cx)
    }

    fn consume(self: std::pin::Pin<&mut Self>, amt: usize) {
        Pin::into_inner(self).consume(amt);
    }
}

pub struct EndpointReadUntilShortPacket<'a, EpType: BulkOrInterrupt> {
    reader: &'a mut EndpointRead<EpType>,
}

impl<EpType: BulkOrInterrupt> EndpointReadUntilShortPacket<'_, EpType> {
    pub fn is_end(&self) -> bool {
        self.reader
            .reading
            .as_ref()
            .is_some_and(|r| !r.has_remaining() && r.is_short_packet())
    }

    pub fn consume_end(&mut self) {
        assert!(self.is_end(), "not at end of short packet");
        self.reader.resubmit();
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
        self: std::pin::Pin<&mut Self>,
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
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<&[u8], std::io::Error>> {
        Pin::into_inner(self).reader.poll_fill_buf(cx)
    }

    fn consume(self: std::pin::Pin<&mut Self>, amt: usize) {
        Pin::into_inner(self).reader.consume(amt);
    }
}

#[cfg(feature = "smol")]
impl<EpType: BulkOrInterrupt> futures_io::AsyncRead for EndpointReadUntilShortPacket<'_, EpType> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
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
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<&[u8], std::io::Error>> {
        Pin::into_inner(self).reader.poll_fill_buf(cx)
    }

    fn consume(self: std::pin::Pin<&mut Self>, amt: usize) {
        Pin::into_inner(self).reader.consume(amt);
    }
}
