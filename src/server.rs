//! HTTP/3 Servers

use std::marker::PhantomData;

use bytes::Buf;
use http::{HeaderMap, Request, Response};

use crate::quic;

/// An HTTP/3 server connection.
pub struct Connection<T, B> {
    _quic: T,
    _marker: PhantomData<fn(B)>,
}

/// A builder to configure options for an HTTP/3 server connection.
pub struct Builder {
    _p: (),
}

/// A request stream accepted on an HTTP/3 server connection.
///
/// # Combines Request and Response
///
/// The streaming of the request body and trailers, and the sending of the
/// corresponding response and its body and trailers are handled in the same
/// type. This allows sharing the QUIC stream, without *requiring* some
/// synchronization to split it the send and receive sides.
///
/// # Split
///
/// If the underlying transport supports "splitting" QUIC streams into send and
/// receive halves, the `RequestStream` can use that to split into two halves
/// as well: a receiver of the request body, and a sender of the response.
pub struct RequestStream<T, B> {
    _quic: T,
    _marker: PhantomData<fn(B)>,
}

pub async fn handshake<T, B>(transport: T) -> Result<Connection<T, B>, crate::Error>
where
    T: quic::Connection<B>,
    B: Buf,
{
    Builder::new().handshake(transport).await
}

impl Builder {
    pub fn new() -> Builder {
        Builder { _p: () }
    }

    pub async fn handshake<T, B>(&self, mut transport: T) -> Result<Connection<T, B>, crate::Error>
    where
        T: quic::Connection<B>,
        B: Buf,
    {
        // 3.3 Connection Establishment
        // https://quicwg.org/base-drafts/draft-ietf-quic-http.html#name-connection-establishment

        // Open *our* control stream.
        let mut control_tx = quic::open_uni_stream(&mut transport).await.unwrap_or_else(|_| panic!("create_send_stream"));


        // Send *our* SETTINGS.
        let settings_frame = (|| -> B {
            todo!("encode settings frame");
        })();
        quic::send_data(&mut control_tx, settings_frame).await.unwrap_or_else(|_| panic!());

        // Accept *their* control stream.
        let mut control_rx = quic::accept_uni_stream(&mut transport).await.unwrap_or_else(|_| panic!());

        // Await their SETTINGS.
        let _remote_settings = quic::recv_data(&mut control_rx).await.unwrap_or_else(|_| panic!());

        // Open *our* mandatory extension streams (QPACK encoder and decoder)


        todo!()
    }
}

// ===== impl Connection =====

impl<T, B> Connection<T, B>
where
    T: quic::Connection<B>,
    B: Buf,
{
    /// Accept new requests sent on this connection.
    pub async fn accept(&mut self) -> Result<(Request<()>, RequestStream<T::BidiStream, B>), crate::Error> {
        todo!()
    }
}

// ===== impl RequestStream =====

impl<T, B> RequestStream<T, B>
where
    T: quic::RecvStream,
{
    /// Receive some of the request body.
    pub async fn recv_data(&mut self) -> Result<Option<T::Buf>, crate::Error> {
        todo!()
    }

    /// Receive an optional set of trailers that ends the request stream.
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, crate::Error> {
        todo!()
    }

    // TODO: We need a way to *open* new streams, in order to send PUSH
    // promises.
}

impl<T, B> RequestStream<T, B>
where
    T: quic::SendStream<B>,
    B: Buf,
{
    /// Send a `Response` for the received `Request`.
    pub async fn send_response(&mut self, _resp: Response<()>) -> Result<(), crate::Error> {
        todo!()
    }

    /// Send some data of the response body.
    pub async fn send_data(&mut self, _buf: B) -> Result<(), crate::Error> {
        todo!()
    }

    /// Send trailers that end the response side.
    pub async fn send_trailers(&mut self, _trailers: HeaderMap) -> Result<(), crate::Error> {
        todo!()
    }
}

impl<T, B> RequestStream<T, B>
where
    T: quic::BidiStream<B>,
    B: Buf,
{
    //pub fn split(self) -> (RequestStream<T::RecvStream, B>, SendResponse<T::SendStream, B>) {
    pub fn split(self) -> (RequestStream<T::RecvStream, B>, T::SendStream) {
        todo!()
    }
}
