//! Traits which define the user API for datagrams.
//! These traits are implemented for the client and server types in the `h3` crate.

use std::{
    future::Future,
    marker::PhantomData,
    task::{ready, Context, Poll},
};

use bytes::Buf;
use h3::{
    connection::ConnectionInner,
    error2::ConnectionError,
    quic::{self, StreamId},
    ConnectionState2,
};
use pin_project_lite::pin_project;

use crate::{
    datagram::Datagram,
    quic_traits::{RecvDatagramExt, SendDatagramErrorIncoming},
};

pub trait HandleDatagramsExt<C, B>: ConnectionState2
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
                todo!()
                //SendDatagramError::ConnectionError(self..handle_quic_connection_error(e))
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

pin_project! {
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
}
