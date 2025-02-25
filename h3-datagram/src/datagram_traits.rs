//! Traits which define the user API for datagrams.
//! These traits are implemented for the client and server types in the `h3` crate.

use std::{
    future::Future,
    marker::PhantomData,
    task::{ready, Context, Poll},
};

use bytes::Buf;
use h3::{
    quic::{self, StreamId},
    Error,
};
use pin_project_lite::pin_project;

use crate::{
    datagram::Datagram,
    quic_traits::{self, RecvDatagramExt},
};

pub trait HandleDatagramsExt<C, B>
where
    B: Buf,
    C: quic::Connection<B>,
{
    /// Sends a datagram
    fn send_datagram(&mut self, stream_id: StreamId, data: B) -> Result<(), Error>;
    /// Reads an incoming datagram
    fn read_datagram(&mut self) -> ReadDatagram<C, B>;
}

pin_project! {
    /// Future for [`Connection::read_datagram`]
    pub struct ReadDatagram<'a, C, B>
    where
            C: quic::Connection<B>,
            B: Buf,
        {
            pub(crate) conn: &'a mut C,
            pub(crate) _marker: PhantomData<B>,
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
        match ready!(self.conn.poll_accept_datagram(cx))? {
            Some(v) => Poll::Ready(Ok(Some(Datagram::decode(v)?))),
            None => Poll::Ready(Ok(None)),
        }
    }
}
