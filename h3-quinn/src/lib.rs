//! QUIC Transport implementation with Quinn
//!
//! This module implements QUIC traits with Quinn.
use std::{
    error::Error,
    fmt::Display,
    pin::Pin,
    task::{self, Poll},
};

use bytes::{Buf, Bytes};
use futures::{ready, AsyncWrite, FutureExt, StreamExt};
pub use quinn;
use quinn::{
    ConnectionError, IncomingBiStreams, IncomingUniStreams, NewConnection, OpenBi, OpenUni, VarInt,
    WriteError,
};

use h3::quic;

pub struct Connection {
    conn: quinn::Connection,
    incoming_bi: IncomingBiStreams,
    opening_bi: Option<OpenBi>,
    incoming_uni: IncomingUniStreams,
    opening_uni: Option<OpenUni>,
}

impl Connection {
    pub fn new(new_conn: NewConnection) -> Self {
        let NewConnection {
            uni_streams,
            bi_streams,
            connection,
            ..
        } = new_conn;

        Self {
            conn: connection,
            incoming_bi: bi_streams,
            opening_bi: None,
            incoming_uni: uni_streams,
            opening_uni: None,
        }
    }
}

impl<B> quic::Connection<B> for Connection
where
    B: Buf,
{
    type SendStream = SendStream<B>;
    type RecvStream = RecvStream;
    type BidiStream = BidiStream<B>;
    type Error = ConnectionError;

    fn poll_accept_bidi_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::BidiStream>, Self::Error>> {
        let (send, recv) = match ready!(self.incoming_bi.next().poll_unpin(cx)) {
            Some(x) => x?,
            None => return Poll::Ready(Ok(None)),
        };
        Poll::Ready(Ok(Some(Self::BidiStream {
            send: Self::SendStream::new(send),
            recv: Self::RecvStream::new(recv),
        })))
    }

    fn poll_open_bidi_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, Self::Error>> {
        if self.opening_bi.is_none() {
            self.opening_bi = Some(self.conn.open_bi());
        }

        let (send, recv) = ready!(self.opening_bi.as_mut().unwrap().poll_unpin(cx))?;
        Poll::Ready(Ok(Self::BidiStream {
            send: Self::SendStream::new(send),
            recv: Self::RecvStream::new(recv),
        }))
    }

    fn poll_accept_recv_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::RecvStream>, Self::Error>> {
        let recv = match ready!(self.incoming_uni.poll_next_unpin(cx)) {
            Some(x) => x?,
            None => return Poll::Ready(Ok(None)),
        };
        Poll::Ready(Ok(Some(Self::RecvStream::new(recv))))
    }

    fn poll_open_send_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>> {
        if self.opening_uni.is_none() {
            self.opening_uni = Some(self.conn.open_uni());
        }

        let send = ready!(self.opening_uni.as_mut().unwrap().poll_unpin(cx))?;
        Poll::Ready(Ok(Self::SendStream::new(send)))
    }
}

pub struct BidiStream<B>
where
    B: Buf,
{
    send: SendStream<B>,
    recv: RecvStream,
}

impl<B> quic::BidiStream<B> for BidiStream<B>
where
    B: Buf,
{
    type SendStream = SendStream<B>;
    type RecvStream = RecvStream;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        (self.send, self.recv)
    }
}

impl<B> quic::RecvStream for BidiStream<B>
where
    B: Buf,
{
    type Buf = Bytes;
    type Error = ReadError;

    fn poll_data(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        self.recv.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.recv.stop_sending(error_code)
    }
}

impl<B> quic::SendStream<B> for BidiStream<B>
where
    B: Buf,
{
    type Error = SendStreamError;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.send.poll_ready(cx)
    }

    fn poll_finish(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.send.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.send.reset(reset_code)
    }

    fn send_data(&mut self, data: B) -> Result<(), Self::Error> {
        self.send.send_data(data)
    }

    fn id(&self) -> u64 {
        self.send.id()
    }
}

pub struct RecvStream {
    stream: quinn::RecvStream,
}

impl RecvStream {
    fn new(stream: quinn::RecvStream) -> Self {
        Self { stream }
    }
}

impl quic::RecvStream for RecvStream {
    type Buf = Bytes;
    type Error = ReadError;

    fn poll_data(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        Poll::Ready(Ok(ready!(self
            .stream
            .read_chunk(usize::MAX, true)
            .poll_unpin(cx))?
        .map(|c| (c.bytes))))
    }

    fn stop_sending(&mut self, error_code: u64) {
        let _ = self
            .stream
            .stop(VarInt::from_u64(error_code).expect("invalid error_code"));
    }
}

#[derive(Debug)]
pub struct ReadError {
    cause: quinn::ReadError,
}

impl Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cause.fmt(f)
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        // TODO: implement std::error::Error for quinn::ReadError
        None
    }
}

impl From<quinn::ReadError> for ReadError {
    fn from(e: quinn::ReadError) -> Self {
        Self { cause: e }
    }
}

pub struct SendStream<B: Buf> {
    stream: quinn::SendStream,
    writing: Option<B>,
}

impl<B> SendStream<B>
where
    B: Buf,
{
    fn new(stream: quinn::SendStream) -> SendStream<B> {
        Self {
            stream,
            writing: None,
        }
    }
}

impl<B> quic::SendStream<B> for SendStream<B>
where
    B: Buf,
{
    type Error = SendStreamError;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(ref mut data) = self.writing {
            while data.has_remaining() {
                match ready!(Pin::new(&mut self.stream).poll_write(cx, data.chunk())) {
                    Ok(n) => data.advance(n),
                    Err(err) => {
                        if let Some(e) = err.into_inner() {
                            return Poll::Ready(Err(SendStreamError::Write(e)));
                        } else {
                            return Poll::Ready(Err(SendStreamError::Unknown));
                        }
                    }
                }
            }
        }
        self.writing = None;
        Poll::Ready(Ok(()))
    }

    fn poll_finish(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.stream.finish().poll_unpin(cx).map_err(Into::into)
    }

    fn reset(&mut self, reset_code: u64) {
        let _ = self
            .stream
            .reset(VarInt::from_u64(reset_code).unwrap_or(VarInt::MAX));
    }

    fn send_data(&mut self, data: B) -> Result<(), Self::Error> {
        if self.writing.is_some() {
            return Err(Self::Error::NotReady);
        }
        self.writing = Some(data);
        Ok(())
    }

    fn id(&self) -> u64 {
        self.stream.id().0
    }
}

#[derive(Debug)]
pub enum SendStreamError {
    Write(Box<dyn std::error::Error + Send + Sync>),
    NotReady,
    Unknown, // this should not happen unless quinn changes
}

impl std::error::Error for SendStreamError {}

impl Display for SendStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl From<WriteError> for SendStreamError {
    fn from(e: WriteError) -> Self {
        Self::Write(Box::new(e))
    }
}
