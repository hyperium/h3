//! Support for the h3-datagram crate.
//!
//! This module implements the traits defined in h3-datagram for the quinn crate.

use std::future::Future;
use std::task::{ready, Poll};

use futures_util::{stream, StreamExt};
use h3_datagram::datagram::EncodedDatagram;
use h3_datagram::quic_traits::{
    DatagramConnectionExt, RecvDatagram, SendDatagram, SendDatagramErrorIncoming,
};

use h3_datagram::ConnectionErrorIncoming;

use bytes::{Buf, Bytes};
use quinn::{ReadDatagram, SendDatagramError};

use crate::{convert_connection_error, BoxStreamSync, Connection};

/// A Struct which allows to send datagrams over a QUIC connection.
pub struct SendDatagramHandler {
    conn: quinn::Connection,
}

impl<B: Buf> SendDatagram<B> for SendDatagramHandler {
    fn send_datagram<T: Into<h3_datagram::datagram::EncodedDatagram<B>>>(
        &mut self,
        data: T,
    ) -> Result<(), SendDatagramErrorIncoming> {
        let mut buf: EncodedDatagram<B> = data.into();
        self.conn
            .send_datagram(buf.copy_to_bytes(buf.remaining()))
            .map_err(convert_send_datagram_error)
    }
}

/// A Struct which allows to receive datagrams over a QUIC connection.
pub struct RecvDatagramHandler {
    datagrams: BoxStreamSync<'static, <ReadDatagram<'static> as Future>::Output>,
}

impl RecvDatagram for RecvDatagramHandler {
    type Buffer = Bytes;
    fn poll_incoming_datagram(
        &mut self,
        cx: &mut core::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::Buffer, ConnectionErrorIncoming>> {
        Poll::Ready(
            ready!(self.datagrams.poll_next_unpin(cx))
                .expect("self. datagrams never returns None")
                .map_err(convert_connection_error),
        )
    }
}

impl<B: Buf> DatagramConnectionExt<B> for Connection {
    type SendDatagramHandler = SendDatagramHandler;
    type RecvDatagramHandler = RecvDatagramHandler;

    fn send_datagram_handler(&self) -> Self::SendDatagramHandler {
        SendDatagramHandler {
            conn: self.conn.clone(),
        }
    }

    fn recv_datagram_handler(&self) -> Self::RecvDatagramHandler {
        RecvDatagramHandler {
            datagrams: Box::pin(stream::unfold(self.conn.clone(), |conn| async {
                Some((conn.read_datagram().await, conn))
            })),
        }
    }
}

fn convert_send_datagram_error(error: SendDatagramError) -> SendDatagramErrorIncoming {
    match error {
        SendDatagramError::UnsupportedByPeer | SendDatagramError::Disabled => {
            SendDatagramErrorIncoming::NotAvailable
        }
        SendDatagramError::TooLarge => SendDatagramErrorIncoming::TooLarge,
        SendDatagramError::ConnectionLost(e) => SendDatagramErrorIncoming::ConnectionError(
            convert_h3_error_to_datagram_error(convert_connection_error(e)),
        ),
    }
}

fn convert_h3_error_to_datagram_error(
    error: h3::quic::ConnectionErrorIncoming,
) -> h3_datagram::ConnectionErrorIncoming {
    match error {
        ConnectionErrorIncoming::ApplicationClose { error_code } => {
            h3_datagram::ConnectionErrorIncoming::ApplicationClose { error_code }
        }
        ConnectionErrorIncoming::Timeout => h3_datagram::ConnectionErrorIncoming::Timeout,
        ConnectionErrorIncoming::InternalError(err) => {
            h3_datagram::ConnectionErrorIncoming::InternalError(err)
        }
        ConnectionErrorIncoming::Undefined(error) => {
            h3_datagram::ConnectionErrorIncoming::Undefined(error)
        }
    }
}
