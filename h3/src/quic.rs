//! QUIC Transport traits
//!
//! This module includes traits and types meant to allow being generic over any
//! QUIC implementation.

use std::fmt::Display;
use std::marker::PhantomData;
use std::task::{self, Poll};

use bytes::Buf;

use crate::ext::Datagram;
pub use crate::proto::stream::{InvalidStreamId, StreamId};
pub use crate::stream::WriteBuf;

// Unresolved questions:
//
// - Should the `poll_` methods be `Pin<&mut Self>`?

/// Trait that represent an error from the transport layer
pub trait Error: std::error::Error + Send + Sync {
    /// Check if the current error is a transport timeout
    fn is_timeout(&self) -> bool;

    /// Get the QUIC error code from connection close or stream stop
    fn err_code(&self) -> Option<u64>;
}

impl<'a, E: Error + 'a> From<E> for Box<dyn Error + 'a> {
    fn from(err: E) -> Box<dyn Error + 'a> {
        Box::new(err)
    }
}

impl Display for ConnectionErrorIncoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // display enum with fields
        match self {
            ConnectionErrorIncoming::ApplicationClose { error_code } => {
                write!(f, "ApplicationClose: {}", error_code)
            }
            ConnectionErrorIncoming::Timeout => write!(f, "Timeout"),
            ConnectionErrorIncoming::ConnectionClosed { error_code } => {
                write!(f, "ConnectionClosed: {}", error_code)
            }
        }
    }
}

impl std::error::Error for ConnectionErrorIncoming {}

impl Error for ConnectionErrorIncoming {
    fn is_timeout(&self) -> bool {
        matches!(self, ConnectionErrorIncoming::Timeout)
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            ConnectionErrorIncoming::ApplicationClose { error_code } => Some(*error_code),
            _ => None,
        }
    }
}

impl std::error::Error for StreamErrorIncoming {}

impl Display for StreamErrorIncoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // display enum with fields
        match self {
            StreamErrorIncoming::ConnectionErrorIncoming { connection_error } => {
                write!(f, "ConnectionError: {}", connection_error)
            }
            StreamErrorIncoming::StreamReset { error_code } => {
                write!(f, "StreamClosed: {}", error_code)
            }
            StreamErrorIncoming::NotReady => write!(f, "NotReady"),
        }
    }
}

impl Error for StreamErrorIncoming {
    fn is_timeout(&self) -> bool {
        match &self {
            StreamErrorIncoming::ConnectionErrorIncoming {
                connection_error: ConnectionErrorIncoming::Timeout,
            } => true,
            _ => false,
        }
    }

    fn err_code(&self) -> Option<u64> {
        match self {
            StreamErrorIncoming::ConnectionErrorIncoming { connection_error } => {
                connection_error.err_code()
            }
            StreamErrorIncoming::StreamReset { error_code } => Some(*error_code),
            StreamErrorIncoming::NotReady => todo!("! Remove this invariant from enum"),
        }
    }
}

/// Error type to communicate that the quic connection was closed
///
/// This is used by to implement the quic abstraction traits
#[derive(Debug)]
pub enum ConnectionErrorIncoming {
    /// Error from the http3 layer
    ApplicationClose {
        /// http3 error code
        error_code: u64,
    },
    /// Quic connection timeout
    Timeout,
    /// Quic error
    ConnectionClosed {
        /// quic error code
        error_code: u64,
    },
}

/// Error type to communicate that the stream was closed
///
/// This is used by to implement the quic abstraction traits
#[derive(Debug)]
pub enum StreamErrorIncoming {
    /// Stream is closed because the whole connection is closed
    ConnectionErrorIncoming {
        /// Connection error
        connection_error: ConnectionErrorIncoming,
    },
    /// Stream was closed by the peer
    StreamReset {
        /// Error code sent by the peer
        error_code: u64,
    },
    /// Todo! Why is this needed?.
    ///
    /// This invariant was in SendStreamError from h3-quinn but i dont know what happened if it was triggered
    NotReady,
}

/// Trait representing a incoming QUIC stream.
pub enum IncomingStreamType<BiDi, UniDi, B>
where
    B: Buf,
    BiDi: SendStream<B> + RecvStream,
    UniDi: RecvStream,
{
    /// A bidirectional stream
    Bidirectional(BiDi, PhantomData<B>),
    /// A unidirectional stream
    Unidirectional(UniDi),
}

/// Trait representing a QUIC connection.
pub trait Connection<B: Buf> {
    /// The type produced by `poll_accept_bidi()`
    type BidiStream: SendStream<B> + RecvStream;
    /// The type of the sending part of `BidiStream`
    type SendStream: SendStream<B>;
    /// The type produced by `poll_accept_recv()`
    type RecvStream: RecvStream;
    /// A producer of outgoing Unidirectional and Bidirectional streams.
    type OpenStreams: OpenStreams<
        B,
        SendStream = Self::SendStream,
        RecvStream = Self::RecvStream,
        BidiStream = Self::BidiStream,
    >;

    /// The error type that can occur when accepting a new stream
    ///
    /// TODO: this should be a trait
    ///

    /// Poll the connection for incoming streams.
    fn poll_incoming(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<
        Result<IncomingStreamType<Self::BidiStream, Self::RecvStream, B>, ConnectionErrorIncoming>,
    >;

    /// Poll the connection to create a new bidirectional stream.
    fn poll_open_bidi(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, ConnectionErrorIncoming>>;

    /// Poll the connection to create a new unidirectional stream.
    fn poll_open_send(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, ConnectionErrorIncoming>>;

    /// Get an object to open outgoing streams.
    fn opener(&self) -> Self::OpenStreams;

    /// Close the connection immediately
    fn close(&mut self, code: crate::error::Code, reason: &[u8]);
}

/// Extends the `Connection` trait for sending datagrams
///
/// See: <https://www.rfc-editor.org/rfc/rfc9297>
pub trait SendDatagramExt<B: Buf> {
    /// The error type that can occur when sending a datagram
    type Error: Into<Box<dyn Error>>;

    /// Send a datagram
    fn send_datagram(&mut self, data: Datagram<B>) -> Result<(), Self::Error>;
}

/// Extends the `Connection` trait for receiving datagrams
///
/// See: <https://www.rfc-editor.org/rfc/rfc9297>
pub trait RecvDatagramExt {
    /// The type of `Buf` for *raw* datagrams (without the stream_id decoded)
    type Buf: Buf;
    /// The error type that can occur when receiving a datagram
    type Error: Into<Box<dyn Error>>;

    /// Poll the connection for incoming datagrams.
    fn poll_accept_datagram(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>>;
}

/// Trait for opening outgoing streams
pub trait OpenStreams<B: Buf> {
    /// The type produced by `poll_open_bidi()`
    type BidiStream: SendStream<B> + RecvStream;
    /// The type produced by `poll_open_send()`
    type SendStream: SendStream<B>;
    /// The type of the receiving part of `BidiStream`
    type RecvStream: RecvStream;
    /// Error type yielded by these trait methods

    /// Poll the connection to create a new bidirectional stream.
    fn poll_open_bidi(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, ConnectionErrorIncoming>>;

    /// Poll the connection to create a new unidirectional stream.
    fn poll_open_send(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, ConnectionErrorIncoming>>;

    /// Close the connection immediately
    fn close(&mut self, code: crate::error::Code, reason: &[u8]);
}

/// A trait describing the "send" actions of a QUIC stream.
pub trait SendStream<B: Buf> {
    /// Polls if the stream can send more data.
    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), StreamErrorIncoming>>;

    /// Send more data on the stream.
    fn send_data<T: Into<WriteBuf<B>>>(&mut self, data: T) -> Result<(), StreamErrorIncoming>;

    /// Poll to finish the sending side of the stream.
    fn poll_finish(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), StreamErrorIncoming>>;

    /// Send a QUIC reset code.
    fn reset(&mut self, reset_code: u64);

    /// Get QUIC send stream id
    fn send_id(&self) -> StreamId;
}

/// Allows sending unframed pure bytes to a stream. Similar to [`AsyncWrite`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncWrite.html)
pub trait SendStreamUnframed<B: Buf>: SendStream<B> {
    /// Attempts write data into the stream.
    ///
    /// Returns the number of bytes written.
    ///
    /// `buf` is advanced by the number of bytes written.
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut task::Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, StreamErrorIncoming>>;
}

/// A trait describing the "receive" actions of a QUIC stream.
pub trait RecvStream {
    /// The type of `Buf` for data received on this stream.
    type Buf: Buf;

    /// Poll the stream for more data.
    ///
    /// When the receive side will no longer receive more data (such as because
    /// the peer closed their sending side), this should return `None`.
    fn poll_data(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, StreamErrorIncoming>>;

    /// Send a `STOP_SENDING` QUIC code.
    fn stop_sending(&mut self, error_code: u64);

    /// Get QUIC send stream id
    fn recv_id(&self) -> StreamId;
}

/// Optional trait to allow "splitting" a bidirectional stream into two sides.
pub trait BidiStream<B: Buf>: SendStream<B> + RecvStream {
    /// The type for the send half.
    type SendStream: SendStream<B>;
    /// The type for the receive half.
    type RecvStream: RecvStream;

    /// Split this stream into two halves.
    fn split(self) -> (Self::SendStream, Self::RecvStream);
}
