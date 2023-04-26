use std::task::Poll;

use bytes::{Buf, Bytes};
use futures_util::{future, ready, AsyncRead, AsyncWrite};
use h3::{
    quic::{self, SendStream as _},
    stream::BufRecvStream,
};
use pin_project_lite::pin_project;

pin_project! {
    /// WebTransport receive stream
    pub struct RecvStream<S> {
        stream: BufRecvStream<S>,
    }
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

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        self.stream.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.stream.stop_sending(error_code)
    }

    fn recv_id(&self) -> quic::StreamId {
        self.stream.recv_id()
    }
}

macro_rules! async_read {
    ($buf: ty) => {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: $buf,
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
    };
}

macro_rules! async_write {
    ($buf: ty) => {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            mut buf: $buf,
        ) -> Poll<std::io::Result<usize>> {
            self.poll_send(cx, &mut buf).map_err(Into::into)
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
    };
}

impl<S> AsyncRead for RecvStream<S>
where
    S: quic::RecvStream,
    S::Error: Into<std::io::Error>,
{
    async_read!(&mut [u8]);
}

pin_project! {
    /// WebTransport send stream
    pub struct SendStream<S> {
        stream: BufRecvStream<S>,
    }
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
    async_write!(&[u8]);
}

pin_project! {
    /// Combined send and receive stream.
    ///
    /// Can be split into a [`RecvStream`] and [`SendStream`] if the underlying QUIC implementation
    /// supports it.
    pub struct BidiStream<S> {
        stream: BufRecvStream<S>,
    }
}

impl<S> BidiStream<S> {
    pub(crate) fn new(stream: BufRecvStream<S>) -> Self {
        Self { stream }
    }
}

impl<S: quic::SendStream> quic::SendStream for BidiStream<S> {
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

impl<S: quic::RecvStream> quic::RecvStream for BidiStream<S> {
    type Buf = Bytes;

    type Error = S::Error;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        self.stream.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.stream.stop_sending(error_code)
    }

    fn recv_id(&self) -> quic::StreamId {
        self.stream.recv_id()
    }
}

impl<S: quic::BidiStream> quic::BidiStream for BidiStream<S> {
    type SendStream = SendStream<S::SendStream>;

    type RecvStream = RecvStream<S::RecvStream>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        let (send, recv) = self.stream.split();
        (SendStream::new(send), RecvStream::new(recv))
    }
}

impl<S> AsyncRead for BidiStream<S>
where
    S: quic::RecvStream,
    S::Error: Into<std::io::Error>,
{
    async_read!(&mut [u8]);
}

impl<S> AsyncWrite for BidiStream<S>
where
    S: quic::SendStream,
    S::Error: Into<std::io::Error>,
{
    async_write!(&[u8]);
}
