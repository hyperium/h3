//! Traits which define the user API for datagrams.
//! These traits are implemented for the client and server types in the `h3` crate.

use std::{future::poll_fn, marker::PhantomData, sync::Arc};

use crate::{
    datagram::Datagram,
    quic_traits::{DatagramConnectionExt, RecvDatagram, SendDatagram, SendDatagramErrorIncoming},
};
use bytes::Buf;
use h3::{
    error::ConnectionError,
    quic::{self, StreamId},
    ConnectionState, SharedState,
};

/// Gives the ability to send datagrams.
pub struct DatagramSender<H: SendDatagram<B>, B: Buf> {
    pub(crate) handler: H,
    pub(crate) _marker: PhantomData<B>,
    pub(crate) shared_state: Arc<SharedState>,
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
    pub fn send_datagram(&mut self, stream_id: StreamId, data: B) -> Result<(), SendDatagramError> {
        let mut buf = BytesMut::new();
        Datagram::new(stream_id, data).encode(&mut buf);

        self.handler.send_datagram(data)

        self.send_datagram(Datagram::new(stream_id, data))
            .map_err(|e| self.handle_send_datagram_error(e))
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

pub struct DatagramReader<H: RecvDatagram<B>, B: Buf> {
    pub(crate) handler: H,
    pub(crate) _marker: PhantomData<B>,
    pub(crate) shared_state: Arc<SharedState>,
}

impl<H, B> ConnectionState for DatagramReader<H, B>
where
    H: RecvDatagram<B>,
    B: Buf,
{
    fn shared_state(&self) -> &SharedState {
        self.shared_state.as_ref()
    }
}

impl<H, B> DatagramReader<H, B>
where
    H: RecvDatagram<B>,
    B: Buf,
{
    /// Reads an incoming datagram
    pub async fn read_datagram(&mut self) -> Result<Datagram<B>, ConnectionError> {
        let x = poll_fn(|cx| self.handler.poll_incoming_datagram(cx)).await;
        todo!()
    }
}

pub trait HandleDatagramsExt<C, B>: ConnectionState
where
    B: Buf,
    C: quic::Connection<B> + DatagramConnectionExt<B>,
{
    /// Sends a datagram
    fn get_datagram_sender(&self) -> DatagramSender<C::SendDatagramHandler, B>;
    /// Reads an incoming datagram
    fn get_datagram_reader(&self) -> DatagramReader<C::RecvDatagramHandler, B>;
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

/*pin_project! {
    /// Future for [`Connection::read_datagram`]
    pub struct ReadDatagram<'a, C, B>
    where
            C: quic::Connection<B>,
            B: Buf,
        {
            pub(crate) conn: &'a mut ConnectionInner<C, B>,
            pub(crate) _marker: PhantomData<B>,
        }
}

impl<'a, C, B> Future for ReadDatagram<'a, C, B>
where
    C: quic::Connection<B> + RecvDatagramExt,
    B: Buf,
{
    type Output = Result<Option<Datagram<C::Buf>>, ConnectionError>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match ready!(self.conn.conn.poll_accept_datagram(cx))
            .map_err(|e| self.conn.handle_connection_error(e))?
        {
            Some(v) => Poll::Ready(Ok(Some(
                Datagram::decode(v).map_err(|e| self.conn.handle_connection_error(e))?,
            ))),
            None => Poll::Ready(Ok(None)),
        }
    }
}*/
