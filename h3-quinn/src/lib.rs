//! QUIC Transport implementation with Quinn
//!
//! This module implements QUIC traits with Quinn.
#![deny(missing_docs)]

use std::{
    convert::TryInto,
    fmt::{self, Display},
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{self, Poll},
};

use bytes::{Buf, Bytes, BytesMut};

use futures::{
    io::Read,
    ready,
    stream::{self, select, BoxStream, Select},
    StreamExt,
};
use quinn::ReadDatagram;
pub use quinn::{self, AcceptBi, AcceptUni, Endpoint, OpenBi, OpenUni, VarInt, WriteError};

use h3::{
    ext::Datagram,
    quic::{
        self, ConnectionErrorIncoming, Error, IncomingStreamType, StreamErrorIncoming, StreamId,
        WriteBuf,
    },
};
use tokio::sync::mpsc;
use tokio_util::sync::ReusableBoxFuture;

/// A QUIC connection backed by Quinn
///
/// Implements a [`quic::Connection`] backed by a [`quinn::Connection`].
pub struct Connection<B>
where
    B: Buf,
{
    // error: Option<ConnectionError>,
    // error_tx: mpsc::Sender<quinn::ConnectionError>,
    // error_rx: mpsc::Receiver<quinn::ConnectionError>,
    conn: quinn::Connection,
    opening_bi: Option<BoxStream<'static, <OpenBi<'static> as Future>::Output>>,
    incoming: Select<
        BoxStream<
            'static,
            Result<IncomingStreamType<BidiStream<B>, RecvStream, B>, quinn::ConnectionError>,
        >,
        BoxStream<
            'static,
            Result<IncomingStreamType<BidiStream<B>, RecvStream, B>, quinn::ConnectionError>,
        >,
    >,
    opening_uni: Option<BoxStream<'static, <OpenUni<'static> as Future>::Output>>,
    datagrams: BoxStream<'static, <ReadDatagram<'static> as Future>::Output>,
}

impl<B> Connection<B>
where
    B: Buf,
{
    /// Create a [`Connection`] from a [`quinn::Connection`]
    pub fn new(conn: quinn::Connection) -> Self {
        let incoming_uni = Box::pin(stream::unfold(conn.clone(), |conn| async {
            Some((
                conn.accept_uni().await.map(|recv_stream| {
                    IncomingStreamType::<BidiStream<B>, RecvStream, B>::Unidirectional(
                        RecvStream::new(recv_stream),
                    )
                }),
                conn,
            ))
        }));
        let incoming_bi = Box::pin(stream::unfold(conn.clone(), |conn| async {
            Some((
                conn.accept_bi().await.map(|bidi_stream| {
                    IncomingStreamType::<BidiStream<B>, RecvStream, B>::Bidirectional(
                        BidiStream {
                            send: SendStream::new(bidi_stream.0),
                            recv: RecvStream::new(bidi_stream.1),
                        },
                        std::marker::PhantomData,
                    )
                }),
                conn,
            ))
        }));

        Self {
            // error: None,
            // error_tx,
            // error_rx,
            conn: conn.clone(),
            opening_bi: None,
            incoming: select(incoming_uni, incoming_bi),
            opening_uni: None,
            datagrams: Box::pin(stream::unfold(conn, |conn| async {
                Some((conn.read_datagram().await, conn))
            })),
        }
    }
}

/// The error type for [`Connection`]
///
/// Wraps reasons a Quinn connection might be lost.
#[derive(Debug, Clone)]
pub struct LegacyConnectionError(quinn::ConnectionError);

/// Error Wrapper to implement traits
#[derive(Debug, Clone)]
pub struct ConnectionError(quinn::ConnectionError);

impl From<ConnectionError> for ConnectionErrorIncoming {
    fn from(e: ConnectionError) -> Self {
        match e.0 {
            quinn::ConnectionError::ApplicationClosed(quinn_proto::ApplicationClose {
                error_code,
                ..
            }) => ConnectionErrorIncoming::ApplicationClose {
                error_code: error_code.into_inner(),
            },
            quinn::ConnectionError::ConnectionClosed(quinn_proto::ConnectionClose {
                error_code,
                ..
            }) => ConnectionErrorIncoming::ConnectionClosed {
                error_code: error_code.into(),
            },
            quinn::ConnectionError::TimedOut => ConnectionErrorIncoming::Timeout,
            _ => ConnectionErrorIncoming::Timeout,
        }
    }
}

impl fmt::Display for LegacyConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for LegacyConnectionError {}

impl Error for LegacyConnectionError {
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

impl From<quinn::ConnectionError> for LegacyConnectionError {
    fn from(e: quinn::ConnectionError) -> Self {
        Self(e)
    }
}

/// Types of errors when sending a datagram.
#[derive(Debug)]
pub enum SendDatagramError {
    /// Datagrams are not supported by the peer
    UnsupportedByPeer,
    /// Datagrams are locally disabled
    Disabled,
    /// The datagram was too large to be sent.
    TooLarge,
    /// Network error
    ConnectionLost(Box<dyn Error>),
}

impl fmt::Display for SendDatagramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendDatagramError::UnsupportedByPeer => write!(f, "datagrams not supported by peer"),
            SendDatagramError::Disabled => write!(f, "datagram support disabled"),
            SendDatagramError::TooLarge => write!(f, "datagram too large"),
            SendDatagramError::ConnectionLost(_) => write!(f, "connection lost"),
        }
    }
}

impl std::error::Error for SendDatagramError {}

impl Error for SendDatagramError {
    fn is_timeout(&self) -> bool {
        false
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            Self::ConnectionLost(err) => err.err_code(),
            _ => None,
        }
    }
}

impl From<quinn::SendDatagramError> for SendDatagramError {
    fn from(value: quinn::SendDatagramError) -> Self {
        match value {
            quinn::SendDatagramError::UnsupportedByPeer => Self::UnsupportedByPeer,
            quinn::SendDatagramError::Disabled => Self::Disabled,
            quinn::SendDatagramError::TooLarge => Self::TooLarge,
            quinn::SendDatagramError::ConnectionLost(err) => {
                Self::ConnectionLost(LegacyConnectionError::from(err).into())
            }
        }
    }
}

impl<B> quic::Connection<B> for Connection<B>
where
    B: Buf,
{
    type SendStream = SendStream<B>;
    type RecvStream = RecvStream;
    type BidiStream = BidiStream<B>;
    type OpenStreams = OpenStreams;

    // fn error(&self) -> Option<ErrorIncoming> {
    //     if let Ok(o) = self.error_rx.try_recv() {
    //         let err: ConnectionError = o.into();
    //         self.error = Some(err.clone());
    //         return Some(ErrorIncoming::ConnectionClosed {
    //             error_code: err.err_code().unwrap_or(0),
    //         });
    //     }

    //     if let Some(e) = &self.error {
    //         return Some(ErrorIncoming::ConnectionClosed {
    //             error_code: e.err_code().unwrap_or(0),
    //         });
    //     }
    //     None
    // }

    fn poll_incoming(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<
        Result<IncomingStreamType<Self::BidiStream, Self::RecvStream, B>, ConnectionErrorIncoming>,
    > {
        // put the two streams together

        match ready!(self.incoming.poll_next_unpin(cx)).unwrap() {
            Ok(x) => Poll::Ready(Ok(x)),
            Err(e) => Poll::Ready(Err(ConnectionError(e).into())),
        }
    }

    fn poll_open_bidi(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, ConnectionErrorIncoming>> {
        if self.opening_bi.is_none() {
            self.opening_bi = Some(Box::pin(stream::unfold(self.conn.clone(), |conn| async {
                Some((conn.open_bi().await, conn))
            })));
        }

        match ready!(self.opening_bi.as_mut().unwrap().poll_next_unpin(cx)).unwrap() {
            Ok((send, recv)) => Poll::Ready(Ok(Self::BidiStream {
                send: Self::SendStream::new(send),
                recv: Self::RecvStream::new(recv),
            })),
            Err(e) => Poll::Ready(Err(ConnectionError(e).into())),
        }
    }

    fn poll_open_send(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, ConnectionErrorIncoming>> {
        if self.opening_uni.is_none() {
            self.opening_uni = Some(Box::pin(stream::unfold(self.conn.clone(), |conn| async {
                Some((conn.open_uni().await, conn))
            })));
        }

        match ready!(self.opening_uni.as_mut().unwrap().poll_next_unpin(cx)).unwrap() {
            Ok(send) => Poll::Ready(Ok(Self::SendStream::new(send))),
            Err(e) => Poll::Ready(Err(ConnectionError(e).into())),
        }
    }

    fn opener(&self) -> Self::OpenStreams {
        OpenStreams {
            conn: self.conn.clone(),
            opening_bi: None,
            opening_uni: None,
        }
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        self.conn.close(
            VarInt::from_u64(code.value()).expect("error code VarInt"),
            reason,
        );
    }
}

impl<B> quic::SendDatagramExt<B> for Connection<B>
where
    B: Buf,
{
    type Error = SendDatagramError;

    fn send_datagram(&mut self, data: Datagram<B>) -> Result<(), SendDatagramError> {
        // TODO investigate static buffer from known max datagram size
        let mut buf = BytesMut::new();
        data.encode(&mut buf);
        self.conn.send_datagram(buf.freeze())?;

        Ok(())
    }
}

impl<B> quic::RecvDatagramExt for Connection<B>
where
    B: Buf,
{
    type Buf = Bytes;

    type Error = LegacyConnectionError;

    #[inline]
    fn poll_accept_datagram(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        match ready!(self.datagrams.poll_next_unpin(cx)) {
            Some(Ok(x)) => Poll::Ready(Ok(Some(x))),
            Some(Err(e)) => Poll::Ready(Err(e.into())),
            None => Poll::Ready(Ok(None)),
        }
    }
}

/// Stream opener backed by a Quinn connection
///
/// Implements [`quic::OpenStreams`] using [`quinn::Connection`],
/// [`quinn::OpenBi`], [`quinn::OpenUni`].
pub struct OpenStreams {
    conn: quinn::Connection,
    opening_bi: Option<BoxStream<'static, <OpenBi<'static> as Future>::Output>>,
    opening_uni: Option<BoxStream<'static, <OpenUni<'static> as Future>::Output>>,
}

impl<B> quic::OpenStreams<B> for OpenStreams
where
    B: Buf,
{
    type RecvStream = RecvStream;
    type SendStream = SendStream<B>;
    type BidiStream = BidiStream<B>;

    fn poll_open_bidi(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, ConnectionErrorIncoming>> {
        if self.opening_bi.is_none() {
            self.opening_bi = Some(Box::pin(stream::unfold(self.conn.clone(), |conn| async {
                Some((conn.open_bi().await, conn))
            })));
        }

        match ready!(self.opening_bi.as_mut().unwrap().poll_next_unpin(cx)).unwrap() {
            Ok((send, recv)) => Poll::Ready(Ok(Self::BidiStream {
                send: Self::SendStream::new(send),
                recv: Self::RecvStream::new(recv),
            })),
            Err(err) => Poll::Ready(Err(ConnectionError(err).into())),
        }
    }

    fn poll_open_send(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, ConnectionErrorIncoming>> {
        if self.opening_uni.is_none() {
            self.opening_uni = Some(Box::pin(stream::unfold(self.conn.clone(), |conn| async {
                Some((conn.open_uni().await, conn))
            })));
        }

        match ready!(self.opening_uni.as_mut().unwrap().poll_next_unpin(cx)).unwrap() {
            Ok(send) => Poll::Ready(Ok(Self::SendStream::new(send))),
            Err(e) => Poll::Ready(Err(ConnectionError(e).into())),
        }
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        self.conn.close(
            VarInt::from_u64(code.value()).expect("error code VarInt"),
            reason,
        );
    }
}

impl Clone for OpenStreams {
    fn clone(&self) -> Self {
        Self {
            conn: self.conn.clone(),
            opening_bi: None,
            opening_uni: None,
        }
    }
}

/// Quinn-backed bidirectional stream
///
/// Implements [`quic::BidiStream`] which allows the stream to be split
/// into two structs each implementing one direction.
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

impl<B: Buf> quic::RecvStream for BidiStream<B> {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        self.recv.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.recv.stop_sending(error_code)
    }

    fn recv_id(&self) -> StreamId {
        self.recv.recv_id()
    }
}

impl<B> quic::SendStream<B> for BidiStream<B>
where
    B: Buf,
{
    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        self.send.poll_ready(cx)
    }

    fn poll_finish(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        self.send.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.send.reset(reset_code)
    }

    fn send_data<D: Into<WriteBuf<B>>>(&mut self, data: D) -> Result<(), StreamErrorIncoming> {
        self.send.send_data(data)
    }

    fn send_id(&self) -> StreamId {
        self.send.send_id()
    }
}
impl<B> quic::SendStreamUnframed<B> for BidiStream<B>
where
    B: Buf,
{
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut task::Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, StreamErrorIncoming>> {
        self.send.poll_send(cx, buf)
    }
}

/// Quinn-backed receive stream
///
/// Implements a [`quic::RecvStream`] backed by a [`quinn::RecvStream`].
pub struct RecvStream {
    stream: Option<quinn::RecvStream>,
    read_chunk_fut: ReadChunkFuture,
}

type ReadChunkFuture = ReusableBoxFuture<
    'static,
    (
        quinn::RecvStream,
        Result<Option<quinn::Chunk>, quinn::ReadError>,
    ),
>;

impl RecvStream {
    fn new(stream: quinn::RecvStream) -> Self {
        Self {
            stream: Some(stream),
            // Should only allocate once the first time it's used
            read_chunk_fut: ReusableBoxFuture::new(async { unreachable!() }),
        }
    }
}

impl quic::RecvStream for RecvStream {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        if let Some(mut stream) = self.stream.take() {
            self.read_chunk_fut.set(async move {
                let chunk = stream.read_chunk(usize::MAX, true).await;
                (stream, chunk)
            })
        };

        let (stream, chunk) = ready!(self.read_chunk_fut.poll(cx));
        self.stream = Some(stream);

        match chunk {
            Ok(Some(a)) => Poll::Ready(Ok(Some(a.bytes))),
            Ok(None) => Poll::Ready(Ok(None)),
            Err(err) => Poll::Ready(Err(ReadError(err).into())),
        }
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.stream
            .as_mut()
            .unwrap()
            .stop(VarInt::from_u64(error_code).expect("invalid error_code"))
            .ok();
    }

    fn recv_id(&self) -> StreamId {
        self.stream
            .as_ref()
            .unwrap()
            .id()
            .0
            .try_into()
            .expect("invalid stream id")
    }
}

/// The error type for [`RecvStream`]
///
/// Wraps errors that occur when reading from a receive stream.
#[derive(Debug)]
pub struct ReadError(pub quinn::ReadError);

impl From<ReadError> for StreamErrorIncoming {
    fn from(value: ReadError) -> Self {
        match value.0 {
            quinn::ReadError::Reset(var_int_error) => StreamErrorIncoming::StreamReset {
                error_code: var_int_error.into_inner(),
            },
            quinn::ReadError::ConnectionLost(err) => StreamErrorIncoming::ConnectionErrorIncoming {
                connection_error: ConnectionError(err).into(),
            },
            quinn::ReadError::UnknownStream => panic!("H3 read from unknown stream"),
            quinn::ReadError::IllegalOrderedRead => panic!("H3 illegal ordered read"),
            quinn::ReadError::ZeroRttRejected => todo!("can this happen?"),
        }
    }
}

// impl From<ReadError> for std::io::Error {
//     fn from(value: ReadError) -> Self {
//         value.0.into()
//     }
// }

// impl std::error::Error for ReadError {
//     fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
//         self.0.source()
//     }
// }

// impl fmt::Display for ReadError {
//     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
//         self.0.fmt(f)
//     }
// }

// impl From<ReadError> for Arc<dyn Error> {
//     fn from(e: ReadError) -> Self {
//         Arc::new(e)
//     }
// }

impl From<quinn::ReadError> for ReadError {
    fn from(e: quinn::ReadError) -> Self {
        Self(e)
    }
}

// impl Error for ReadError {
//     fn is_timeout(&self) -> bool {
//         matches!(
//             self.0,
//             quinn::ReadError::ConnectionLost(quinn::ConnectionError::TimedOut)
//         )
//     }

//     fn err_code(&self) -> Option<u64> {
//         match self.0 {
//             quinn::ReadError::ConnectionLost(quinn::ConnectionError::ApplicationClosed(
//                 quinn_proto::ApplicationClose { error_code, .. },
//             )) => Some(error_code.into_inner()),
//             quinn::ReadError::Reset(error_code) => Some(error_code.into_inner()),
//             _ => None,
//         }
//     }
// }

/// Quinn-backed send stream
///
/// Implements a [`quic::SendStream`] backed by a [`quinn::SendStream`].
pub struct SendStream<B: Buf> {
    stream: Option<quinn::SendStream>,
    writing: Option<WriteBuf<B>>,
    write_fut: WriteFuture,
}

type WriteFuture =
    ReusableBoxFuture<'static, (quinn::SendStream, Result<usize, quinn::WriteError>)>;

impl<B> SendStream<B>
where
    B: Buf,
{
    fn new(stream: quinn::SendStream) -> SendStream<B> {
        Self {
            stream: Some(stream),
            writing: None,
            write_fut: ReusableBoxFuture::new(async { unreachable!() }),
        }
    }
}

impl<B> quic::SendStream<B> for SendStream<B>
where
    B: Buf,
{
    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        if let Some(ref mut data) = self.writing {
            while data.has_remaining() {
                if let Some(mut stream) = self.stream.take() {
                    let chunk = data.chunk().to_owned(); // FIXME - avoid copy
                    self.write_fut.set(async move {
                        let ret = stream.write(&chunk).await;
                        (stream, ret)
                    });
                }

                let (stream, res) = ready!(self.write_fut.poll(cx));
                self.stream = Some(stream);
                match res {
                    Ok(cnt) => data.advance(cnt),
                    Err(err) => {
                        return Poll::Ready(Err(SendStreamError::Write(err).into()));
                    }
                }
            }
        }
        self.writing = None;
        Poll::Ready(Ok(()))
    }

    fn poll_finish(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        self.stream
            .as_mut()
            .unwrap()
            .poll_finish(cx)
            .map_err(|e| SendStreamError::Write(e).into())
    }

    fn reset(&mut self, reset_code: u64) {
        let _ = self
            .stream
            .as_mut()
            .unwrap()
            .reset(VarInt::from_u64(reset_code).unwrap_or(VarInt::MAX));
    }

    fn send_data<D: Into<WriteBuf<B>>>(&mut self, data: D) -> Result<(), StreamErrorIncoming> {
        if self.writing.is_some() {
            return Err(StreamErrorIncoming::NotReady);
        }
        self.writing = Some(data.into());
        Ok(())
    }

    fn send_id(&self) -> StreamId {
        self.stream
            .as_ref()
            .unwrap()
            .id()
            .0
            .try_into()
            .expect("invalid stream id")
    }
}

impl<B> quic::SendStreamUnframed<B> for SendStream<B>
where
    B: Buf,
{
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut task::Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, StreamErrorIncoming>> {
        if self.writing.is_some() {
            // This signifies a bug in implementation
            panic!("poll_send called while send stream is not ready")
        }

        let s = Pin::new(self.stream.as_mut().unwrap());

        let res = ready!(futures::io::AsyncWrite::poll_write(s, cx, buf.chunk()));
        match res {
            Ok(written) => {
                buf.advance(written);
                Poll::Ready(Ok(written))
            }
            Err(err) => {
                // We are forced to use AsyncWrite for now because we cannot store
                // the result of a call to:
                // quinn::send_stream::write<'a>(&'a mut self, buf: &'a [u8]) -> Result<usize, WriteError>.
                //
                // This is why we have to unpack the error from io::Error instead of having it
                // returned directly. This should not panic as long as quinn's AsyncWrite impl
                // doesn't change.
                let err = err
                    .into_inner()
                    .expect("write stream returned an empty error")
                    .downcast::<WriteError>()
                    .expect("write stream returned an error which type is not WriteError");

                Poll::Ready(Err(SendStreamError::Write(*err).into()))
            }
        }
    }
}

/// The error type for [`SendStream`]
///
/// Wraps errors that can happen writing to or polling a send stream.
#[derive(Debug)]
pub enum SendStreamError {
    /// Errors when writing, wrapping a [`quinn::WriteError`]
    Write(WriteError),
    /// Error when the stream is not ready, because it is still sending
    /// data from a previous call
    NotReady,
}

impl From<SendStreamError> for StreamErrorIncoming {
    fn from(value: SendStreamError) -> Self {
        match value {
            SendStreamError::Write(WriteError::ConnectionLost(err)) => {
                StreamErrorIncoming::ConnectionErrorIncoming {
                    connection_error: ConnectionError(err).into(),
                }
            }
            SendStreamError::Write(WriteError::Stopped(err)) => StreamErrorIncoming::StreamReset {
                error_code: err.into_inner(),
            },
            SendStreamError::Write(WriteError::UnknownStream) => {
                panic!("H3 write to unknown stream")
            }
            SendStreamError::Write(WriteError::ZeroRttRejected) => todo!("can this happen?"),
            SendStreamError::NotReady => todo!("???"),
        }
    }
}

impl From<SendStreamError> for std::io::Error {
    fn from(value: SendStreamError) -> Self {
        match value {
            SendStreamError::Write(err) => err.into(),
            SendStreamError::NotReady => {
                std::io::Error::new(std::io::ErrorKind::Other, "send stream is not ready")
            }
        }
    }
}

impl std::error::Error for SendStreamError {}

impl Display for SendStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl From<WriteError> for SendStreamError {
    fn from(e: WriteError) -> Self {
        Self::Write(e)
    }
}

impl Error for SendStreamError {
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

impl From<SendStreamError> for Arc<dyn Error> {
    fn from(e: SendStreamError) -> Self {
        Arc::new(e)
    }
}
