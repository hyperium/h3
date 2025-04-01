//! server API

use std::marker::PhantomData;

use bytes::Buf;
use h3::{
    quic::{self},
    server::Connection,
};

use crate::{
    datagram_handler::{DatagramReader, DatagramSender, HandleDatagramsExt},
    quic_traits::DatagramConnectionExt,
};

impl<B, C> HandleDatagramsExt<C, B> for Connection<C, B>
where
    B: Buf,
    C: quic::Connection<B> + DatagramConnectionExt<B>,
{
    /// Get the datagram sender
    fn get_datagram_sender(
        &self,
        stream_id: quic::StreamId,
    ) -> crate::datagram_handler::DatagramSender<
        <C as crate::quic_traits::DatagramConnectionExt<B>>::SendDatagramHandler,
        B,
    > {
        DatagramSender {
            handler: self.inner.conn.send_datagram_handler(),
            _marker: PhantomData,
            shared_state: self.inner.shared.clone(),
            stream_id,
        }
    }

    /// Get the datagram reader
    fn get_datagram_reader(
        &self,
    ) -> crate::datagram_handler::DatagramReader<
        <C as crate::quic_traits::DatagramConnectionExt<B>>::RecvDatagramHandler,
    > {
        DatagramReader {
            handler: self.inner.conn.recv_datagram_handler(),
            shared_state: self.inner.shared.clone(),
        }
    }
}
