use std::{marker::PhantomData, task::Poll};

use bytes::{Buf, Bytes};
use futures_util::{future, ready, AsyncRead};

use crate::{
    buf::BufList,
    proto::varint::UnexpectedEnd,
    quic::{self, RecvStream as _, SendStream as _},
};

use super::SessionId;

/// WebTransport receive stream
pub struct RecvStream<S> {
    buf: BufList<Bytes>,
    stream: S,
}

impl<S> RecvStream<S> {
    pub(crate) fn new(buf: BufList<Bytes>, stream: S) -> Self {
        Self { buf, stream }
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
        if let Some(chunk) = self.buf.take_first_chunk() {
            if chunk.remaining() > 0 {
                return Poll::Ready(Ok(Some(chunk)));
            }
        }

        match ready!(self.stream.poll_data(cx)?) {
            Some(mut buf) => Poll::Ready(Ok(Some(buf.copy_to_bytes(buf.remaining())))),
            None => Poll::Ready(Ok(None)),
        }
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.stream.stop_sending(error_code)
    }

    fn recv_id(&self) -> quic::StreamId {
        self.stream.recv_id()
    }
}

/// WebTransport send stream
pub struct SendStream<S> {
    stream: S,
}

impl<S> SendStream<S> {
    pub(crate) fn new(stream: S) -> Self {
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

    /// Writes the entire buffer to the stream
    pub async fn write_all(&mut self, mut buf: impl Buf) -> Result<(), S::Error> {
        while buf.has_remaining() {
            self.write(&mut buf).await?;
        }

        Ok(())
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

/// A biderectional webtransport stream.
///
/// May be split into a sender and a receiver part
pub struct BidiStream<S> {
    send: SendStream<S>,
    recv: RecvStream<S>,
}

impl<S> quic::BidiStream for BidiStream<S>
where
    S: quic::SendStream + quic::RecvStream,
{
    type SendStream = SendStream<S>;
    type RecvStream = RecvStream<S>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        (self.send, self.recv)
    }
}

impl<S> quic::SendStream for BidiStream<S>
where
    S: quic::SendStream,
{
    type Error = S::Error;

    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, Self::Error>> {
        self.send.poll_send(cx, buf)
    }

    fn poll_finish(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.send.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.send.reset(reset_code)
    }

    fn send_id(&self) -> quic::StreamId {
        self.send.send_id()
    }
}

impl<S> quic::RecvStream for BidiStream<S>
where
    S: quic::RecvStream,
{
    type Buf = Bytes;

    type Error = S::Error;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        self.recv.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.recv.stop_sending(error_code)
    }

    fn recv_id(&self) -> quic::StreamId {
        self.recv.recv_id()
    }
}
