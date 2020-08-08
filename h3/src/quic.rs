//! QUIC Transport traits
//!
//! This module includes traits and types meant to allow being generic over any
//! QUIC implementation.

use std::{
    io,
    task::{self, Poll},
};

use bytes::Buf;

// Unresolved questions:
//
// - Should the `poll_` methods be `Pin<&mut Self>`?

/// Trait representing a QUIC connection.
pub trait Connection<B: Buf> {
    /// The stream type returned when opening a unidirectional send stream.
    type SendStream: SendStream<B>;
    /// The stream type returned when accepting a unidirectional receive stream.
    type RecvStream: RecvStream;
    /// The stream type returned when accepting or opening a bidirectional
    /// stream.
    type BidiStream: SendStream<B> + RecvStream;
    /// The error type that can be returned when accepting or opening a stream.
    type Error;

    // Accepting streams

    /// Poll the connection for any received bidirectional streams.
    ///
    /// Returning `None` implies the connection is closing or closed.
    fn poll_accept_bidi_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::BidiStream>, Self::Error>>;

    /// Poll the connection for any received unidirectional streams.
    ///
    /// Returning `None` implies the connection is closing or closed.
    fn poll_accept_recv_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::RecvStream>, Self::Error>>;

    // Opening streams

    /// Poll the connection to create a new bidirectional stream.
    fn poll_open_bidi_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, Self::Error>>;

    /// Poll the connection to create a new unidirectional stream.
    fn poll_open_send_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>>;
}

/// A trait describing the "send" actions of a QUIC stream.
pub trait SendStream<B: Buf> {
    /// The error type returned by fallible send methods.
    type Error; // bounds?

    /// Polls if the stream can send more data.
    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>>;

    /// Send more data on the stream.
    fn send_data(&mut self, data: B) -> Result<(), Self::Error>;

    /// Poll to finish the sending side of the stream.
    fn poll_finish(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>>;

    /// Send a QUIC reset code.
    fn reset(&mut self, reset_code: u64);
}

/// A trait describing the "receive" actions of a QUIC stream.
pub trait RecvStream {
    /// The type of `Buf` for data received on this stream.
    type Buf: Buf;
    /// The error type that can occur when receiving data.
    type Error: Into<io::Error>;

    /// Poll the stream for more data.
    ///
    /// When the receive side will no longer receive more data (such as because
    /// the peer closed their sending side), this should return `None`.
    fn poll_data(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>>;

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
