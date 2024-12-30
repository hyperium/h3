//! server API

use std::{
    future::Future,
    marker::PhantomData,
    task::{ready, Context, Poll},
};

use bytes::Buf;
use h3::{
    quic::{self, StreamId},
    server::Connection,
    Error,
};
use pin_project_lite::pin_project;

use crate::{
    datagram::Datagram,
    datagram_traits::HandleDatagramsExt,
    quic_traits::{self, RecvDatagramExt, SendDatagramExt},
};

impl<B, C> HandleDatagramsExt<C, B> for Connection<C, B>
where
    B: Buf,
    C: quic::Connection<B> + SendDatagramExt<B> + RecvDatagramExt,
    <C as quic_traits::RecvDatagramExt>::Error: h3::quic::Error + 'static,
    <C as quic_traits::SendDatagramExt<B>>::Error: h3::quic::Error + 'static,
{
    /// Sends a datagram
    fn send_datagram(&mut self, stream_id: StreamId, data: B) -> Result<(), Error> {
        self.inner
            .conn
            .send_datagram(Datagram::new(stream_id, data))?;
        Ok(())
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
    <C as quic_traits::RecvDatagramExt>::Error: h3::quic::Error + 'static,
    B: Buf,
{
    type Output = Result<Option<Datagram<C::Buf>>, Error>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match ready!(self.conn.inner.conn.poll_accept_datagram(cx))? {
            Some(v) => Poll::Ready(Ok(Some(Datagram::decode(v)?))),
            None => Poll::Ready(Ok(None)),
        }
    }
}
