//! client API

use std::marker::PhantomData;

use bytes::Buf;
use h3::{
    client::Connection,
    quic::{self, StreamId},
};

use crate::{
    datagram::Datagram,
    datagram_traits::{HandleDatagramsExt, ReadDatagram, SendDatagramError},
    quic_traits::{RecvDatagramExt, SendDatagramExt},
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
            .map_err(|e| self.handle_send_datagram_error(e))?;
        Ok(())
    }

    /// Reads an incoming datagram
    fn read_datagram(&mut self) -> ReadDatagram<C, B> {
        ReadDatagram {
            conn: &mut self.inner,
            _marker: PhantomData,
        }
    }
}
