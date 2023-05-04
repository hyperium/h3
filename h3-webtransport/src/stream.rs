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
    pub struct RecvStream<S,B> {
        stream: BufRecvStream<S, B>,
    }
}

impl<S, B> RecvStream<S, B> {
    #[allow(missing_docs)]
    pub fn new(stream: BufRecvStream<S, B>) -> Self {
        Self { stream }
    }
}

impl<S, B> quic::RecvStream for RecvStream<S, B>
where
    S: quic::RecvStream,
    B: Buf,
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
            // self.send_data(buf.into())?;

            // ready!(self.poll_ready(cx)?);

            let len = buf.len();
            Poll::Ready(Ok(len))
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

impl<S, B> AsyncRead for RecvStream<S, B>
where
    S: quic::RecvStream,
    S::Error: Into<std::io::Error>,
    B: Buf,
{
    async_read!(&mut [u8]);
}

pin_project! {
    /// WebTransport send stream
    pub struct SendStream<S,B> {
        stream: BufRecvStream<S ,B>,
    }
}

impl<S, B> std::fmt::Debug for SendStream<S, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendStream")
            .field("stream", &self.stream)
            .finish()
    }
}

impl<S, B> SendStream<S, B> {
    #[allow(missing_docs)]
    pub(crate) fn new(stream: BufRecvStream<S, B>) -> Self {
        Self { stream }
    }
}

impl<S, B> SendStream<S, B>
where
    S: quic::SendStream<B>,
    B: Buf,
{
    /// Write bytes to the stream.
    ///
    /// Returns the number of bytes written
    pub async fn write(&mut self, buf: impl Buf) -> Result<(), S::Error> {
        todo!();
        // self.send_data(buf.into())?;
        future::poll_fn(|cx| quic::SendStream::poll_ready(self, cx)).await?;
        Ok(())
    }
}

impl<S, B> quic::SendStream<B> for SendStream<S, B>
where
    S: quic::SendStream<B>,
    B: Buf,
{
    type Error = S::Error;

    fn poll_finish(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.stream.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.stream.reset(reset_code)
    }

    fn send_id(&self) -> quic::StreamId {
        self.stream.send_id()
    }

    fn send_data<T: Into<h3::stream::WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        todo!()
    }

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }
}

impl<S, B> AsyncWrite for SendStream<S, B>
where
    S: quic::SendStream<B>,
    B: Buf,
    S::Error: Into<std::io::Error>,
{
    async_write!(&[u8]);
}

pin_project! {
    /// Combined send and receive stream.
    ///
    /// Can be split into a [`RecvStream`] and [`SendStream`] if the underlying QUIC implementation
    /// supports it.
    pub struct BidiStream<S, B> {
        stream: BufRecvStream<S, B>,
    }
}

impl<S, B> BidiStream<S, B> {
    pub(crate) fn new(stream: BufRecvStream<S, B>) -> Self {
        Self { stream }
    }
}

impl<S, B> quic::SendStream<B> for BidiStream<S, B>
where
    S: quic::SendStream<B>,
    B: Buf,
{
    type Error = S::Error;

    fn poll_finish(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.stream.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.stream.reset(reset_code)
    }

    fn send_id(&self) -> quic::StreamId {
        self.stream.send_id()
    }

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn send_data<T: Into<h3::stream::WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        todo!()
    }
}

impl<S: quic::RecvStream, B> quic::RecvStream for BidiStream<S, B> {
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

impl<S, B> quic::BidiStream<B> for BidiStream<S, B>
where
    S: quic::BidiStream<B>,
    B: Buf,
{
    type SendStream = SendStream<S::SendStream, B>;

    type RecvStream = RecvStream<S::RecvStream, B>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        let (send, recv) = self.stream.split();
        (SendStream::new(send), RecvStream::new(recv))
    }
}

impl<S, B> AsyncRead for BidiStream<S, B>
where
    S: quic::RecvStream,
    S::Error: Into<std::io::Error>,
    B: Buf,
{
    async_read!(&mut [u8]);
}

impl<S, B> AsyncWrite for BidiStream<S, B>
where
    S: quic::SendStream<B>,
    S::Error: Into<std::io::Error>,
    B: Buf,
{
    async_write!(&[u8]);
}
