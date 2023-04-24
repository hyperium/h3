use std::task::Poll;

use bytes::{Buf, Bytes};
use futures_util::{future, ready, AsyncRead, AsyncWrite};

use crate::{
    quic::{self, SendStream as _},
    stream::BufRecvStream,
};

/// WebTransport receive stream
#[pin_project::pin_project]
pub struct RecvStream<S> {
    stream: BufRecvStream<S>,
}

impl<S> RecvStream<S> {
    #[allow(missing_docs)]
    pub fn new(stream: BufRecvStream<S>) -> Self {
        Self { stream }
    }
}

impl<S> quic::RecvStream for RecvStream<S>
where
    S: quic::RecvStream,
{
    type Buf = Bytes;

    type Error = S::Error;

    #[tracing::instrument(level = "info", skip_all)]
    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        tracing::info!("Polling RecvStream");
        self.stream.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.stream.stop_sending(error_code)
    }

    fn recv_id(&self) -> quic::StreamId {
        self.stream.recv_id()
    }
}

impl<S> AsyncRead for RecvStream<S>
where
    S: quic::RecvStream,
    S::Error: Into<std::io::Error>,
{
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        // If the buffer i empty, poll for more data
        if !self.stream.buf_mut().has_remaining() {
            let res = ready!(self.stream.poll_read(cx).map_err(Into::into))?;
            if res {
                return Poll::Ready(Ok(0));
            };
        }

        let bytes = self.stream.buf_mut();

        // Do not overfill
        if let Some(chunk) = bytes.take_chunk(buf.len()) {
            assert!(chunk.len() <= buf.len());
            let len = chunk.len().min(buf.len());
            buf[..len].copy_from_slice(&chunk);

            Poll::Ready(Ok(len))
        } else {
            Poll::Ready(Ok(0))
        }
    }
}

/// WebTransport send stream
#[pin_project::pin_project]
pub struct SendStream<S> {
    stream: BufRecvStream<S>,
}

impl<S> std::fmt::Debug for SendStream<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendStream")
            .field("stream", &self.stream)
            .finish()
    }
}

impl<S> SendStream<S> {
    #[allow(missing_docs)]
    pub fn new(stream: BufRecvStream<S>) -> Self {
        Self { stream }
    }
}

impl<S> SendStream<S>
where
    S: quic::SendStream,
{
    /// Write bytes to the stream.
    ///
    /// Returns the number of bytes written
    pub async fn write(&mut self, buf: &mut impl Buf) -> Result<usize, S::Error> {
        future::poll_fn(|cx| quic::SendStream::poll_send(self, cx, buf)).await
    }
}

impl<S> quic::SendStream for SendStream<S>
where
    S: quic::SendStream,
{
    type Error = S::Error;

    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, Self::Error>> {
        self.stream.poll_send(cx, buf)
    }

    fn poll_finish(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.stream.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.stream.reset(reset_code)
    }

    fn send_id(&self) -> quic::StreamId {
        self.stream.send_id()
    }
}

impl<S> AsyncWrite for SendStream<S>
where
    S: quic::SendStream,
    S::Error: Into<std::io::Error>,
{
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let res = self.poll_send(cx, &mut buf).map_err(Into::into);

        tracing::debug!("poll_write {res:?}");
        res
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.poll_finish(cx).map_err(Into::into)
    }
}
