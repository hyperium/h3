use std::task::Poll;

use bytes::Buf;
use h3::{
    quic::{self, StreamErrorIncoming},
    stream::BufRecvStream,
};
use pin_project_lite::pin_project;
use tokio::io::ReadBuf;

pin_project! {
    /// WebTransport receive stream
    pub struct RecvStream<S, B, R> {
        #[pin]
        stream: BufRecvStream<S, B, R>,
    }
}

impl<S, B, R> RecvStream<S, B, R> {
    #[allow(missing_docs)]
    pub fn new(stream: BufRecvStream<S, B, R>) -> Self {
        Self { stream }
    }
}

impl<S, B, R> quic::RecvStream for RecvStream<S, B, R>
where
    S: quic::RecvStream<Buf = R>,
    R: Buf,
{
    type Buf = R;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        self.stream.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.stream.stop_sending(error_code)
    }

    fn recv_id(&self) -> quic::StreamId {
        self.stream.recv_id()
    }
}

impl<S, B, R> futures_util::io::AsyncRead for RecvStream<S, B, R>
where
    BufRecvStream<S, B, R>: futures_util::io::AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let p = self.project();
        p.stream.poll_read(cx, buf)
    }
}

impl<S, B, R> tokio::io::AsyncRead for RecvStream<S, B, R>
where
    BufRecvStream<S, B, R>: tokio::io::AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_read(cx, buf)
    }
}

pin_project! {
    /// WebTransport send stream
    pub struct SendStream<S, B, R> {
        #[pin]
        stream: BufRecvStream<S, B, R>
    }
}

impl<S, B, R> std::fmt::Debug for SendStream<S, B, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendStream")
            .field("stream", &self.stream)
            .finish()
    }
}

impl<S, B, R> SendStream<S, B, R> {
    #[allow(missing_docs)]
    pub(crate) fn new(stream: BufRecvStream<S, B, R>) -> Self {
        Self { stream }
    }
}

impl<S, B, R> quic::SendStreamUnframed<B> for SendStream<S, B, R>
where
    S: quic::SendStreamUnframed<B>,
    B: Buf,
{
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, StreamErrorIncoming>> {
        self.stream.poll_send(cx, buf)
    }
}

impl<S, B, R> quic::SendStream<B> for SendStream<S, B, R>
where
    S: quic::SendStream<B>,
    B: Buf,
{
    fn poll_finish(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), StreamErrorIncoming>> {
        self.stream.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.stream.reset(reset_code)
    }

    fn send_id(&self) -> quic::StreamId {
        self.stream.send_id()
    }

    fn send_data<T: Into<h3::stream::WriteBuf<B>>>(
        &mut self,
        data: T,
    ) -> Result<(), StreamErrorIncoming> {
        self.stream.send_data(data)
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), StreamErrorIncoming>> {
        self.stream.poll_ready(cx)
    }
}

impl<S, B, R> futures_util::io::AsyncWrite for SendStream<S, B, R>
where
    BufRecvStream<S, B, R>: futures_util::io::AsyncWrite,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let p = self.project();
        p.stream.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_flush(cx)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_close(cx)
    }
}

impl<S, B, R> tokio::io::AsyncWrite for SendStream<S, B, R>
where
    BufRecvStream<S, B, R>: tokio::io::AsyncWrite,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let p = self.project();
        p.stream.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_shutdown(cx)
    }
}

pin_project! {
    /// Combined send and receive stream.
    ///
    /// Can be split into a [`RecvStream`] and [`SendStream`] if the underlying QUIC implementation
    /// supports it.
    pub struct BidiStream<S, B, R> {
        #[pin]
        stream: BufRecvStream<S, B, R>,
    }
}

impl<S, B, R> BidiStream<S, B, R> {
    pub(crate) fn new(stream: BufRecvStream<S, B, R>) -> Self {
        Self { stream }
    }
}

impl<S, B, R> quic::SendStream<B> for BidiStream<S, B, R>
where
    S: quic::SendStream<B>,
    B: Buf,
{
    fn poll_finish(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), StreamErrorIncoming>> {
        self.stream.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.stream.reset(reset_code)
    }

    fn send_id(&self) -> quic::StreamId {
        self.stream.send_id()
    }

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), StreamErrorIncoming>> {
        self.stream.poll_ready(cx)
    }

    fn send_data<T: Into<h3::stream::WriteBuf<B>>>(
        &mut self,
        data: T,
    ) -> Result<(), StreamErrorIncoming> {
        self.stream.send_data(data)
    }
}

impl<S, B, R> quic::SendStreamUnframed<B> for BidiStream<S, B, R>
where
    S: quic::SendStreamUnframed<B>,
    B: Buf,
    R: Buf,
{
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, StreamErrorIncoming>> {
        self.stream.poll_send(cx, buf)
    }
}

impl<S: quic::RecvStream<Buf = R>, B, R: Buf> quic::RecvStream for BidiStream<S, B, R> {
    type Buf = R;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        self.stream.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.stream.stop_sending(error_code)
    }

    fn recv_id(&self) -> quic::StreamId {
        self.stream.recv_id()
    }
}

impl<S, B, R> quic::BidiStream<B> for BidiStream<S, B, R>
where
    S: quic::BidiStream<B, RecvStream: quic::RecvStream<Buf = R>> + quic::RecvStream<Buf = R>,
    B: Buf,
    R: Buf,
{
    type SendStream = SendStream<S::SendStream, B, R>;

    type RecvStream = RecvStream<S::RecvStream, B, R>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        let (send, recv) = self.stream.split();
        (SendStream::new(send), RecvStream::new(recv))
    }
}

impl<S, B, R> futures_util::io::AsyncRead for BidiStream<S, B, R>
where
    BufRecvStream<S, B, R>: futures_util::io::AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let p = self.project();
        p.stream.poll_read(cx, buf)
    }
}

impl<S, B, R> futures_util::io::AsyncWrite for BidiStream<S, B, R>
where
    BufRecvStream<S, B, R>: futures_util::io::AsyncWrite,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let p = self.project();
        p.stream.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_flush(cx)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_close(cx)
    }
}

impl<S, B, R> tokio::io::AsyncRead for BidiStream<S, B, R>
where
    BufRecvStream<S, B, R>: tokio::io::AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_read(cx, buf)
    }
}

impl<S, B, R> tokio::io::AsyncWrite for BidiStream<S, B, R>
where
    BufRecvStream<S, B, R>: tokio::io::AsyncWrite,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let p = self.project();
        p.stream.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let p = self.project();
        p.stream.poll_shutdown(cx)
    }
}
