//! server API

use std::{
    future::Future,
    marker::PhantomData,
    task::{ready, Context, Poll},
};

use bytes::Buf;
use h3::{
    error2::{traits::CloseConnection, ConnectionError},
    quic::{self, StreamId},
    server::Connection,
};
use pin_project_lite::pin_project;

use crate::{
    datagram::Datagram,
    datagram_traits::{HandleDatagramsExt, SendDatagramError},
    quic_traits::{RecvDatagramExt, SendDatagramErrorIncoming, SendDatagramExt},
};

impl<B, C> HandleDatagramsExt<C, B> for Connection<C, B>
where
    B: Buf,
    C: quic::Connection<B> + SendDatagramExt<B> + RecvDatagramExt,
{
    /// Sends a datagram
    fn send_datagram(&mut self, stream_id: StreamId, data: B) -> Result<(), SendDatagramError> {
        self.inner
            .conn
            .send_datagram(Datagram::new(stream_id, data))
            .map_err(|e| self.handle_send_datagram_error(e))
    }

    /// Reads an incoming datagram
    fn read_datagram(&mut self) -> ReadDatagram<C, B> {
        ReadDatagram {
            conn: self,
            _marker: PhantomData,
        }
    }
}

pin_project! {
    /// Future for [`Connection::read_datagram`]
    pub struct ReadDatagram<'a, C, B>
    where
            C: quic::Connection<B>,
            B: Buf,
        {
            conn: &'a mut Connection<C, B>,
            _marker: PhantomData<B>,
        }
}

impl<'a, C, B> Future for ReadDatagram<'a, C, B>
where
    C: quic::Connection<B> + RecvDatagramExt,
    B: Buf,
{
    type Output = Result<Option<Datagram<C::Buf>>, ConnectionError>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match ready!(self.conn.inner.conn.poll_accept_datagram(cx))
            .map_err(|e| self.conn.handle_quic_connection_error(e))?
        {
            Some(v) => Poll::Ready(Ok(Some(
                Datagram::decode(v).map_err(|e| self.conn.handle_connection_error(e))?,
            ))),
            None => Poll::Ready(Ok(None)),
        }
    }
}
