//! QUIC Transport traits
//!
//! This module includes traits and types meant to allow being generic over any
//! QUIC implementation.

use core::task;
use std::task::Poll;

use bytes::Buf;
use h3::quic::ConnectionErrorIncoming;

use crate::datagram::Datagram;

/// Extends the `Connection` trait for sending datagrams
///
/// See: <https://www.rfc-editor.org/rfc/rfc9297>
pub trait SendDatagramExt<B: Buf> {
    /// Send a datagram
    fn send_datagram(&mut self, data: Datagram<B>) -> Result<(), SendDatagramErrorIncoming>;
}

/// Extends the `Connection` trait for receiving datagrams
///
/// See: <https://www.rfc-editor.org/rfc/rfc9297>
pub trait RecvDatagramExt {
    /// The type of `Buf` for *raw* datagrams (without the stream_id decoded)
    type Buf: Buf;

    /// Poll the connection for incoming datagrams.
    fn poll_accept_datagram(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, ConnectionErrorIncoming>>;
}

/// Types of errors when sending a datagram.
#[derive(Debug)]
pub enum SendDatagramErrorIncoming {
    /// The peer is not accepting datagrams
    ///
    /// This can be because the peer does not support it or disabled it or any other reason.
    NotAvailable,
    /// The datagram is too large to send
    TooLarge,
    /// Connection error
    ConnectionError(ConnectionErrorIncoming),
}
