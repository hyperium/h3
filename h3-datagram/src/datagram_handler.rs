//! Traits which define the user API for datagrams.
//! These traits are implemented for the client and server types in the `h3` crate.

use std::{error::Error, fmt::Display, future::poll_fn, marker::PhantomData, sync::Arc};

use crate::{
    datagram::Datagram,
    quic_traits::{DatagramConnectionExt, RecvDatagram, SendDatagram, SendDatagramErrorIncoming},
};
use bytes::Buf;
use h3::{
    error::{connection_error_creators::CloseStream, ConnectionError, StreamError},
    quic::{self, StreamId},
    ConnectionState, SharedState,
};

/// Gives the ability to send datagrams.
#[derive(Debug)]
pub struct DatagramSender<H: SendDatagram<B>, B: Buf> {
    pub(crate) handler: H,
    pub(crate) _marker: PhantomData<B>,
    pub(crate) shared_state: Arc<SharedState>,
    pub(crate) stream_id: StreamId,
}

impl<H, B> ConnectionState for DatagramSender<H, B>
where
    H: SendDatagram<B>,
    B: Buf,
{
    fn shared_state(&self) -> &SharedState {
        self.shared_state.as_ref()
    }
}

impl<H, B> DatagramSender<H, B>
where
    H: SendDatagram<B>,
    B: Buf,
{
    /// Sends a datagram
    pub fn send_datagram(&mut self, data: B) -> Result<(), SendDatagramError> {
        let encoded_datagram = Datagram::new(self.stream_id, data);
        match self.handler.send_datagram(encoded_datagram.encode()) {
            Ok(()) => Ok(()),
            Err(e) => Err(self.handle_send_datagram_error(e)),
        }
    }

    fn handle_send_datagram_error(
        &mut self,
        error: SendDatagramErrorIncoming,
    ) -> SendDatagramError {
        match error {
            SendDatagramErrorIncoming::NotAvailable => SendDatagramError::NotAvailable,
            SendDatagramErrorIncoming::TooLarge => SendDatagramError::TooLarge,
            SendDatagramErrorIncoming::ConnectionError(error) => {
                self.set_conn_error_and_wake(error.clone());
                SendDatagramError::ConnectionError(ConnectionError::Remote(error))
            }
        }
    }
}

#[derive(Debug)]
pub struct DatagramReader<H: RecvDatagram> {
    pub(crate) handler: H,
    pub(crate) shared_state: Arc<SharedState>,
}

impl<H> ConnectionState for DatagramReader<H>
where
    H: RecvDatagram,
{
    fn shared_state(&self) -> &SharedState {
        self.shared_state.as_ref()
    }
}

impl<H> CloseStream for DatagramReader<H> where H: RecvDatagram {}

impl<H> DatagramReader<H>
where
    H: RecvDatagram,
{
    /// Reads an incoming datagram
    pub async fn read_datagram(&mut self) -> Result<Datagram<H::Buffer>, StreamError> {
        match poll_fn(|cx| self.handler.poll_incoming_datagram(cx)).await {
            Ok(datagram) => Datagram::decode(datagram)
                .map_err(|err| self.handle_connection_error_on_stream(err)),
            Err(err) => Err(self.handle_quic_stream_error(
                quic::StreamErrorIncoming::ConnectionErrorIncoming {
                    connection_error: err,
                },
            )),
        }
    }
}

pub trait HandleDatagramsExt<C, B>: ConnectionState
where
    B: Buf,
    C: quic::Connection<B> + DatagramConnectionExt<B>,
{
    /// Sends a datagram
    fn get_datagram_sender(&self, stream_id: StreamId)
        -> DatagramSender<C::SendDatagramHandler, B>;
    /// Reads an incoming datagram
    fn get_datagram_reader(&self) -> DatagramReader<C::RecvDatagramHandler>;
}

/// Types of errors when sending a datagram.
#[derive(Debug)]
#[non_exhaustive]
pub enum SendDatagramError {
    /// The peer is not accepting datagrams on the quic layer
    ///
    /// This can be because the peer does not support it or disabled it or any other reason.
    #[non_exhaustive]
    NotAvailable,
    /// The datagram is too large to send
    #[non_exhaustive]
    TooLarge,
    /// Connection error
    #[non_exhaustive]
    ConnectionError(ConnectionError),
}

impl Display for SendDatagramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendDatagramError::NotAvailable => write!(f, "Datagrams are not available"),
            SendDatagramError::TooLarge => write!(f, "Datagram is too large"),
            SendDatagramError::ConnectionError(e) => write!(f, "Connection error: {}", e),
        }
    }
}

impl Error for SendDatagramError {}
