//! QUIC Transport traits
//!
//! This module includes traits and types meant to allow being generic over any
//! QUIC implementation.

use std::task::{self, Poll};

use bytes::Buf;

// Unresolved questions:
//
// - Should the `poll_` methods be `Pin<&mut Self>`?

/// Trait representing a QUIC connection.
pub trait Connection<B: Buf> {
    /// The type produced by `poll_accept_bidi()`
    type BidiStream: SendStream<B> + RecvStream;
    /// The type of the sending part of `BidiStream`
    type SendStream: SendStream<B>;
    /// The type produced by `poll_accept_recv()`
    type RecvStream: RecvStream;
    /// A producer of outgoing Unidirectional and Bidirectional streams.
    type OpenStreams: OpenStreams<B>;
    /// Error type yeilded by this trait methods
    type Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    /// Accept an incoming unidirecional stream
    ///
    /// Returning `None` implies the connection is closing or closed.
    fn poll_accept_recv(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::RecvStream>, Self::Error>>;

    /// Accept an incoming bidirecional stream
    ///
    /// Returning `None` implies the connection is closing or closed.
    fn poll_accept_bidi(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::BidiStream>, Self::Error>>;

    /// Poll the connection to create a new bidirectional stream.
    fn poll_open_bidi(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, Self::Error>>;

    /// Poll the connection to create a new unidirectional stream.
    fn poll_open_send(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>>;

    /// Get an object to open outgoing streams.
    fn opener(&self) -> Self::OpenStreams;
}

/// Trait for opening outgoing streams
pub trait OpenStreams<B: Buf> {
    /// The type produced by `poll_open_bidi()`
    type BidiStream: SendStream<B> + RecvStream;
    /// The type produced by `poll_open_send()`
    type SendStream: SendStream<B>;
    /// The type of the receiving part of `BidiStream`
    type RecvStream: RecvStream;
    /// Error type yeilded by this trait methods
    type Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    /// Poll the connection to create a new bidirectional stream.
    fn poll_open_bidi(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, Self::Error>>;

    /// Poll the connection to create a new unidirectional stream.
    fn poll_open_send(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>>;
}

/// A trait describing the "send" actions of a QUIC stream.
pub trait SendStream<B: Buf> {
    /// The error type returned by fallible send methods.
    type Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    /// Polls if the stream can send more data.
    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>>;

    /// Send more data on the stream.
    fn send_data(&mut self, data: B) -> Result<(), Self::Error>;

    /// Poll to finish the sending side of the stream.
    fn poll_finish(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>>;

    /// Send a QUIC reset code.
    fn reset(&mut self, reset_code: u64);

    /// Get QUIC send stream id
    fn id(&self) -> u64;
}

/// A trait describing the "receive" actions of a QUIC stream.
pub trait RecvStream {
    /// The type of `Buf` for data received on this stream.
    type Buf: Buf;
    /// The error type that can occur when receiving data.
    type Error: Into<Box<dyn std::error::Error + Send + Sync>>;

    /// Poll the stream for more data.
    ///
    /// When the receive side will no longer receive more data (such as because
    /// the peer closed their sending side), this should return `None`.
    fn poll_data(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>>;

    /// Poll the stream for a precise number of bytes
    ///
    /// The `buf` slice should be filled with as much received data as possible. When
    /// the receive side will no longer receive more data (such as because
    /// the peer closed their sending side), this should return `None`.
    fn poll_read(
        &mut self,
        buf: &mut [u8],
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<usize>, Self::Error>>;

    /// Send a `STOP_SENDING` QUIC code.
    fn stop_sending(&mut self, error_code: u64);
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
