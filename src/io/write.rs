use crate::{
    transfer::{Buffer, BulkOrInterrupt, Completion, Out},
    Endpoint,
};
use std::{
    future::poll_fn,
    io::{Error, ErrorKind, Write},
    pin::Pin,
    task::{ready, Context, Poll},
    time::Duration,
};

pub struct EndpointWrite<EpType: BulkOrInterrupt> {
    endpoint: Endpoint<EpType, Out>,
    writing: Option<Buffer>,
    transfer_size: usize,
    num_transfers: usize,
    write_timeout: Duration,
}

impl<EpType: BulkOrInterrupt> EndpointWrite<EpType> {
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

    pub fn set_num_transfers(&mut self, num_transfers: usize) {
        self.num_transfers = num_transfers;
    }

    pub fn with_num_transfers(mut self, num_transfers: usize) -> Self {
        self.set_num_transfers(num_transfers);
        self
    }

    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = timeout;
    }

    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.set_write_timeout(timeout);
        self
    }

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

    pub fn submit(&mut self) {
        if self.writing.as_ref().is_some_and(|b| !b.is_empty()) {
            self.endpoint.submit(self.writing.take().unwrap())
        }
    }

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

    pub fn flush(&mut self) -> Result<(), Error> {
        self.submit();
        while self.endpoint.pending() > 0 {
            self.wait_one()?;
        }
        Ok(())
    }

    pub fn flush_end(&mut self) -> Result<(), Error> {
        self.submit_end();
        while self.endpoint.pending() > 0 {
            self.wait_one()?;
        }
        Ok(())
    }

    fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.submit();
        while self.endpoint.pending() > 0 {
            ready!(self.poll_one(cx))?;
        }
        Poll::Ready(Ok(()))
    }

    pub async fn flush_end_async(&mut self) -> Result<(), Error> {
        self.submit_end();
        while self.endpoint.pending() > 0 {
            poll_fn(|cx| self.poll_one(cx)).await?;
        }
        Ok(())
    }
}

impl<EpType: BulkOrInterrupt> Write for EndpointWrite<EpType> {
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

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush()
    }
}

#[cfg(feature = "smol")]
impl<EpType: BulkOrInterrupt> futures_io::AsyncWrite for EndpointWrite<EpType> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::into_inner(self).poll_write(cx, buf)
    }

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
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::into_inner(self).poll_write(cx, buf)
    }

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
