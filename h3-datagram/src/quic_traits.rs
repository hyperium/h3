//! QUIC Transport traits
//!
//! This module includes traits and types meant to allow being generic over any
//! QUIC implementation.

use core::task;
use std::task::Poll;

use bytes::Buf;
use h3::quic::ConnectionErrorIncoming;

use crate::datagram::EncodedDatagram;

/// Connection Extension trait for a DatagramHandler type defined by the quic implementation
pub trait DatagramConnectionExt<B: Buf> {
    /// The type of the Datagram send Handler
    type SendDatagramHandler: SendDatagram<B>;

    /// The type of the Datagram receive Handler
    type RecvDatagramHandler: RecvDatagram;

    /// Get the send datagram handler
    fn send_datagram_handler(&self) -> Self::SendDatagramHandler;

    /// Get the receive datagram handler
    fn recv_datagram_handler(&self) -> Self::RecvDatagramHandler;
}

/// Extends the `Connection` trait for sending datagrams
///
/// See: <https://www.rfc-editor.org/rfc/rfc9297>
pub trait SendDatagram<B: Buf> {
    /// Send a datagram
    fn send_datagram<T: Into<EncodedDatagram<B>>>(
        &mut self,
        data: T,
    ) -> Result<(), SendDatagramErrorIncoming>;
}

/// Extends the `Connection` trait for receiving datagrams
///
/// See: <https://www.rfc-editor.org/rfc/rfc9297>
pub trait RecvDatagram {
    /// The buffer type
    type Buffer: Buf;

    /// Poll the connection for incoming datagrams.
    fn poll_incoming_datagram(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::Buffer, ConnectionErrorIncoming>>;
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
