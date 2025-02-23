//! Traits which define the user API for datagrams.
//! These traits are implemented for the client and server types in the `h3` crate.

use bytes::Buf;
use h3::{
    error2::{traits::CloseConnection, ConnectionError},
    quic::{self, StreamId},
    ConnectionState2,
};

use crate::{quic_traits::SendDatagramErrorIncoming, server::ReadDatagram};

pub trait HandleDatagramsExt<C, B>: ConnectionState2 + CloseConnection
where
    B: Buf,
    C: quic::Connection<B>,
{
    /// Sends a datagram
    fn send_datagram(&mut self, stream_id: StreamId, data: B) -> Result<(), SendDatagramError>;
    /// Reads an incoming datagram
    fn read_datagram(&mut self) -> ReadDatagram<C, B>;

    fn handle_send_datagram_error(
        &mut self,
        error: SendDatagramErrorIncoming,
    ) -> SendDatagramError {
        match error {
            SendDatagramErrorIncoming::NotAvailable => SendDatagramError::NotAvailable,
            SendDatagramErrorIncoming::TooLarge => SendDatagramError::TooLarge,
            SendDatagramErrorIncoming::ConnectionError(e) => {
                SendDatagramError::ConnectionError(self.handle_quic_connection_error(e))
            }
        }
    }
}

/// Types of errors when sending a datagram.
#[derive(Debug)]
pub enum SendDatagramError {
    /// The peer is not accepting datagrams on the quic layer
    ///
    /// This can be because the peer does not support it or disabled it or any other reason.
    NotAvailable,
    /// The datagram is too large to send
    TooLarge,
    /// Connection error
    ConnectionError(ConnectionError),
}
