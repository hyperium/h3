//! QUIC Transport traits
//!
//! This module includes traits and types meant to allow being generic over any
//! QUIC implementation.

use core::task;
use std::{error::Error, task::Poll};

use bytes::Buf;

use crate::datagram::Datagram;

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
