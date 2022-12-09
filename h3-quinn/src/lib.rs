//! [`h3::quic`] traits implemented with Quinn

#![deny(missing_docs)]

use std::{
    convert::TryInto,
    fmt::{self, Display},
    future,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::{Buf, Bytes, BytesMut};
use futures_util::{ready, Future, FutureExt};
use h3::quic::{self, Error, StreamId, WriteBuf};
use pin_project::pin_project;
use quinn::{AcceptBi, AcceptUni, OpenBi, OpenUni, VarInt};

pub use quinn::{self};

/// QUIC connection
///
/// A [`quic::Connection`] backed by [`quinn::Connection`].
pub struct Connection {
    conn: quinn::Connection,
}

impl Connection {
    /// Create a [`Connection`] from a [`quinn::Connection`]
    pub fn new(conn: quinn::Connection) -> Self {
        Self { conn }
    }
}

/// todo: doc
#[pin_project]
pub struct AcceptBidiStream<'a, B> {
    #[pin]
    uni_fut: AcceptBi<'a>,
    e: Option<B>,
}

impl<'a, B: Buf> Future for AcceptBidiStream<'a, B> {
    type Output = Result<BidiStream<B>, ConnectionError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().uni_fut.poll_unpin(cx) {
            Poll::Ready(s) => match s {
                Ok((a, b)) => {
                    return Poll::Ready(Ok(BidiStream::new(SendStream::new(a), RecvStream::new(b))))
                }
                Err(err) => Poll::Ready(Err(ConnectionError(err))),
            },
            Poll::Pending => return Poll::Pending,
        }
    }
}

#[pin_project]
pub struct AcceptRecvStream<'a> {
    #[pin]
    uni_fut: AcceptUni<'a>,
}

impl<'a> Future for AcceptRecvStream<'a> {
    type Output = Result<RecvStream, ConnectionError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().uni_fut.poll_unpin(cx) {
            Poll::Ready(s) => match s {
                Ok(a) => return Poll::Ready(Ok(RecvStream::new(a))),
                Err(err) => Poll::Ready(Err(ConnectionError(err))),
            },
            Poll::Pending => return Poll::Pending,
        }
    }
}

#[pin_project]
pub struct OpenBidiStream<'a, B> {
    #[pin]
    uni_fut: OpenBi<'a>,
    e: Option<B>,
}

impl<'a, B: Buf> Future for OpenBidiStream<'a, B> {
    type Output = Result<BidiStream<B>, ConnectionError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().uni_fut.poll_unpin(cx) {
            Poll::Ready(s) => match s {
                Ok((a, b)) => {
                    return Poll::Ready(Ok(BidiStream::new(SendStream::new(a), RecvStream::new(b))))
                }
                Err(err) => Poll::Ready(Err(ConnectionError(err))),
            },
            Poll::Pending => return Poll::Pending,
        }
    }
}

#[pin_project]
pub struct OpenSendStream<'a, B> {
    #[pin]
    uni_fut: OpenUni<'a>,
    e: Option<B>,
}

impl<'a, B: Buf> Future for OpenSendStream<'a, B> {
    type Output = Result<SendStream<B>, ConnectionError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().uni_fut.poll_unpin(cx) {
            Poll::Ready(s) => match s {
                Ok(a) => return Poll::Ready(Ok(SendStream::new(a))),
                Err(err) => Poll::Ready(Err(ConnectionError(err))),
            },
            Poll::Pending => return Poll::Pending,
        }
    }
}

impl<B: Buf> quic::Connection<B> for Connection {
    type BidiStream = BidiStream<B>;
    type OpenStreams = OpenStreams;
    type SendStream = SendStream<B>;
    type RecvStream = RecvStream;
    type Error = ConnectionError;
    type BidiStreamFuture<'a> = AcceptBidiStream<'a, B> where Self: 'a;
    type AcceptRecvFuture<'a> = AcceptRecvStream<'a>
    where
        Self: 'a;
    type OpenBidiFuture<'a> = OpenBidiStream<'a, B>
    where
        Self: 'a;

    type OpenSendFuture<'a> = OpenSendStream<'a, B>
    where
        Self: 'a;

    fn poll_accept_bidi<'a>(&'a mut self) -> Self::BidiStreamFuture<'a> {
        let accept_bidi = self.conn.accept_bi();
        AcceptBidiStream {
            uni_fut: accept_bidi,
            e: None,
        }
    }

    fn poll_accept_recv<'a>(&mut self) -> Self::AcceptRecvFuture<'a> {
        let accept_recv = self.conn.accept_uni();
        AcceptRecvStream {
            uni_fut: accept_recv,
        }
    }

    fn poll_open_bidi<'a>(&mut self) -> Self::OpenBidiFuture<'a> {
        let open_bidi = self.conn.open_bi();
        OpenBidiStream {
            uni_fut: open_bidi,
            e: None,
        }
    }

    fn poll_open_send<'a>(&mut self) -> Self::OpenSendFuture<'a> {
        OpenSendStream {
            uni_fut: self.conn.open_uni(),
            e: None,
        }
    }

    fn opener(&self) -> Self::OpenStreams {
        Self::OpenStreams {
            conn: self.conn.clone(),
        }
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        self.conn.close(
            VarInt::from_u64(code.value()).expect("Invalid error code"),
            reason,
        );
    }
}

/// Stream opener
///
/// Implements [`quic::OpenStreams`].
pub struct OpenStreams {
    conn: quinn::Connection,
}

impl<B: Buf> quic::OpenStreams<B> for OpenStreams {
    type BidiStream = BidiStream<B>;
    type SendStream = SendStream<B>;
    type RecvStream = RecvStream;
    type Error = ConnectionError;

    fn poll_open_bidi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, Self::Error>> {
        Poll::Ready(match ready!(Box::pin(self.conn.open_bi()).poll_unpin(cx)) {
            Ok((send, recv)) => Ok(Self::BidiStream::new(
                Self::SendStream::new(send),
                Self::RecvStream::new(recv),
            )),
            Err(e) => Err(ConnectionError(e)),
        })
    }

    fn poll_open_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>> {
        Poll::Ready(
            match ready!(Box::pin(self.conn.open_uni()).poll_unpin(cx)) {
                Ok(send) => Ok(Self::SendStream::new(send)),
                Err(e) => Err(ConnectionError(e)),
            },
        )
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        self.conn.close(
            VarInt::from_u64(code.value()).expect("Invalid error code"),
            reason,
        );
    }
}

impl Clone for OpenStreams {
    fn clone(&self) -> Self {
        Self {
            conn: self.conn.clone(),
        }
    }
}

/// Stream that can be used to send and receive data
///
/// A [`quic::BidiStream`], which can be split into one send-only and
/// one receive-only stream.
pub struct BidiStream<B: Buf> {
    send: SendStream<B>,
    recv: RecvStream,
}

impl<B: Buf> BidiStream<B> {
    fn new(send: SendStream<B>, recv: RecvStream) -> Self {
        Self { send, recv }
    }
}

impl<B: Buf> quic::BidiStream<B> for BidiStream<B> {
    type SendStream = SendStream<B>;
    type RecvStream = RecvStream;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        (self.send, self.recv)
    }
}

impl<B: Buf> quic::SendStream<B> for BidiStream<B> {
    type Error = SendError;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.send.poll_ready(cx)
    }

    fn send_data<T: Into<WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        self.send.send_data(data)
    }

    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.send.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.send.reset(reset_code)
    }

    fn id(&self) -> StreamId {
        self.send.id()
    }
}

impl<B: Buf> quic::RecvStream for BidiStream<B> {
    type Buf = Bytes;
    type Error = RecvError;

    fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        self.recv.poll_data(cx)
    }

    fn stop_sending(&mut self, err_code: u64) {
        self.recv.stop_sending(err_code)
    }
}

/// Send-only stream
///
/// A [`quic::SendStream`] backed by [`quinn::SendStream`].
pub struct SendStream<B: Buf> {
    stream: quinn::SendStream,
    writing: Option<WriteBuf<B>>,
}

impl<B: Buf> SendStream<B> {
    fn new(stream: quinn::SendStream) -> Self {
        Self {
            stream,
            writing: None,
        }
    }
}

impl<B: Buf> quic::SendStream<B> for SendStream<B> {
    type Error = SendError;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(ref mut data) = self.writing {
            while data.has_remaining() {
                match ready!({
                    let mut write_fut = Box::pin(self.stream.write(data.chunk()));
                    write_fut.poll_unpin(cx)
                }) {
                    Ok(cnt) => data.advance(cnt),
                    Err(e) => return Poll::Ready(Err(Self::Error::Write(e))),
                }
            }
        }

        self.writing = None;
        Poll::Ready(Ok(()))
    }

    fn send_data<T: Into<WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        match self.writing {
            Some(_) => Err(Self::Error::NotReady),
            None => {
                self.writing = Some(data.into());
                Ok(())
            }
        }
    }

    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Box::pin(self.stream.finish())
            .poll_unpin(cx)
            .map_err(Into::into)
    }

    fn reset(&mut self, reset_code: u64) {
        let _ = self
            .stream
            .reset(VarInt::from_u64(reset_code).unwrap_or(VarInt::MAX));
    }

    fn id(&self) -> StreamId {
        self.stream.id().0.try_into().expect("Invalid stream id")
    }
}

/// Receive-only stream
///
/// A [`quic::RecvStream`] backed by [`quinn::RecvStream`].
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
    type Error = RecvError;

    fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        let data = ready!(Box::pin(self.stream.read_chunk(usize::MAX, true)).poll_unpin(cx))?;
        Poll::Ready(Ok(data.map(|ch| ch.bytes)))
    }

    fn stop_sending(&mut self, err_code: u64) {
        let _ = self
            .stream
            .stop(VarInt::from_u64(err_code).expect("Invalid error code"));
    }
}

/// The error type for [`quic::Connection::Error`]
///
/// Used by [`Connection`].
#[derive(Debug)]
pub struct ConnectionError(quinn::ConnectionError);

impl Error for ConnectionError {
    fn is_timeout(&self) -> bool {
        matches!(self.0, quinn::ConnectionError::TimedOut)
    }

    fn err_code(&self) -> Option<u64> {
        match self.0 {
            quinn::ConnectionError::ApplicationClosed(quinn_proto::ApplicationClose {
                error_code,
                ..
            }) => Some(error_code.into_inner()),
            _ => None,
        }
    }
}

impl std::error::Error for ConnectionError {}

impl Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<quinn::ConnectionError> for ConnectionError {
    fn from(e: quinn::ConnectionError) -> Self {
        Self(e)
    }
}

/// The error type for [`quic::SendStream::Error`]
///
/// Used by [`SendStream`] and [`BidiStream`].
#[derive(Debug)]
pub enum SendError {
    /// For write errors, wrapping a [`quinn::WriteError`]
    Write(quinn::WriteError),
    /// For trying to send when stream is not ready, because it is
    /// still sending data from the previous call
    NotReady,
}

impl Error for SendError {
    fn is_timeout(&self) -> bool {
        matches!(
            self,
            Self::Write(quinn::WriteError::ConnectionLost(
                quinn::ConnectionError::TimedOut
            ))
        )
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            Self::Write(quinn::WriteError::Stopped(error_code)) => Some(error_code.into_inner()),
            Self::Write(quinn::WriteError::ConnectionLost(
                quinn::ConnectionError::ApplicationClosed(quinn_proto::ApplicationClose {
                    error_code,
                    ..
                }),
            )) => Some(error_code.into_inner()),
            _ => None,
        }
    }
}

impl std::error::Error for SendError {}

impl Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl From<quinn::WriteError> for SendError {
    fn from(e: quinn::WriteError) -> Self {
        Self::Write(e)
    }
}

/// The error type for [`quic::RecvStream::Error`]
///
/// Used by [`RecvStream`] and [`BidiStream`].
#[derive(Debug)]
pub struct RecvError(quinn::ReadError);

impl Error for RecvError {
    fn is_timeout(&self) -> bool {
        matches!(
            self.0,
            quinn::ReadError::ConnectionLost(quinn::ConnectionError::TimedOut),
        )
    }

    fn err_code(&self) -> Option<u64> {
        match self.0 {
            quinn::ReadError::ConnectionLost(quinn::ConnectionError::ApplicationClosed(
                quinn_proto::ApplicationClose { error_code, .. },
            )) => Some(error_code.into_inner()),
            quinn::ReadError::Reset(error_code) => Some(error_code.into_inner()),
            _ => None,
        }
    }
}

impl std::error::Error for RecvError {}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<RecvError> for Arc<dyn Error> {
    fn from(e: RecvError) -> Self {
        Arc::new(e)
    }
}

impl From<quinn::ReadError> for RecvError {
    fn from(e: quinn::ReadError) -> Self {
        Self(e)
    }
}
