use std::{marker::PhantomData, task::Poll};

use bytes::{Buf, Bytes};
use futures_util::{future, ready, AsyncRead};

use crate::{
    buf::BufList,
    proto::varint::UnexpectedEnd,
    quic::{self, SendStream as QSendStream},
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
pub struct SendStream<S, B> {
    stream: S,
    _marker: PhantomData<B>,
}

impl<S, B> SendStream<S, B> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream,
            _marker: PhantomData,
        }
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

impl<S, B> QSendStream<B> for SendStream<S, B>
where
    S: QSendStream<B>,
    B: Buf,
{
    type Error = S::Error;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn send_data<T: Into<quic::WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        todo!()
    }

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
