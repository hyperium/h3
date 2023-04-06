//! Provides the server side WebTransport session

use std::{
    marker::PhantomData,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use crate::{
    connection::ConnectionState,
    error::Code,
    proto::datagram::Datagram,
    quic,
    server::{self, Connection, RequestStream},
    Error, Protocol,
};
use bytes::{Buf, Bytes};
use futures_util::{ready, Future};
use http::{Method, Request, Response, StatusCode};
use quic::StreamId;

use super::{stream::RecvStream, SessionId};

/// WebTransport session driver.
///
/// Maintains the session using the underlying HTTP/3 connection.
///
/// Similar to [`crate::Connection`] it is generic over the QUIC implementation and Buffer.
pub struct WebTransportSession<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    // See: https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/#section-2-3
    session_id: StreamId,
    conn: Mutex<Connection<C, B>>,
    connect_stream: RequestStream<C::BidiStream, B>,
}

impl<C, B> WebTransportSession<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// Accepts a *CONNECT* request for establishing a WebTransport session.
    ///
    /// TODO: is the API or the user responsible for validating the CONNECT request?
    pub async fn accept(
        request: Request<()>,
        mut stream: RequestStream<C::BidiStream, B>,
        mut conn: Connection<C, B>,
    ) -> Result<Self, Error> {
        // future::poll_fn(|cx| conn.poll_control(cx)).await?;

        let shared = conn.shared_state().clone();
        {
            let config = shared.write("Read WebTransport support").config;

            tracing::debug!("Client settings: {:#?}", config);
            if !config.enable_webtransport {
                return Err(conn.close(
                    Code::H3_SETTINGS_ERROR,
                    "webtransport is not supported by client",
                ));
            }

            if !config.enable_datagram {
                return Err(conn.close(
                    Code::H3_SETTINGS_ERROR,
                    "datagrams are not supported by client",
                ));
            }
        }

        tracing::debug!("Validated client webtransport support");

        // The peer is responsible for validating our side of the webtransport support.
        //
        // However, it is still advantageous to show a log on the server as (attempting) to
        // establish a WebTransportSession without the proper h3 config is usually a mistake.
        if !conn.inner.config.enable_webtransport {
            tracing::warn!("Server does not support webtransport");
        }

        if !conn.inner.config.enable_datagram {
            tracing::warn!("Server does not support datagrams");
        }

        if !conn.inner.config.enable_connect {
            tracing::warn!("Server does not support CONNECT");
        }

        // Respond to the CONNECT request.

        //= https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/#section-3.3
        let response = if validate_wt_connect(&request) {
            Response::builder()
                // This is the only header that chrome cares about.
                .header("sec-webtransport-http3-draft", "draft02")
                .status(StatusCode::OK)
                .body(())
                .unwrap()
        } else {
            Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(())
                .unwrap()
        };

        tracing::info!("Sending response: {response:?}");
        stream.send_response(response).await?;

        let session_id = stream.send_id();
        tracing::info!("Established new WebTransport session with id {session_id:?}");
        Ok(Self {
            session_id,
            conn: Mutex::new(conn),
            connect_stream: stream,
        })
    }

    /// Receive a datagram from the client
    pub fn read_datagram(&self) -> ReadDatagram<C, B> {
        ReadDatagram {
            conn: self.conn.lock().unwrap().inner.conn.clone(),
            _marker: PhantomData,
        }
    }

    /// Sends a datagram
    ///
    /// TODO: maybe make async. `quinn` does not require an async send
    pub fn send_datagram(&self, data: impl Buf) -> Result<(), Error> {
        self.conn
            .lock()
            .unwrap()
            .send_datagram(self.connect_stream.id(), data)?;

        Ok(())
    }

    /// Accept an incoming unidirectional stream from the client, it reads the stream until EOF.
    pub fn accept_uni(&self) -> AcceptUni<C, B> {
        AcceptUni {
            conn: &self.conn,
            _marker: PhantomData,
        }
    }
}

/// Future for [`Connection::read_datagram`]
pub struct ReadDatagram<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    conn: Arc<Mutex<C>>,
    _marker: PhantomData<B>,
}

impl<C, B> Future for ReadDatagram<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    type Output = Result<Option<(SessionId, Bytes)>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        tracing::trace!("poll: read_datagram");

        let mut conn = self.conn.lock().unwrap();
        match ready!(conn.poll_accept_datagram(cx))? {
            // Some(v) => Poll::Ready(Ok(Some(Datagram::decode(v)?.payload))),
            Some(v) => {
                let datagram = Datagram::decode(v)?;
                Poll::Ready(Ok(Some((datagram.stream_id().into(), datagram.payload))))
            }
            None => Poll::Ready(Ok(None)),
        }
    }
}

/// Future for [`WebTransportSession::accept_uni`]
pub struct AcceptUni<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    conn: &'a Mutex<server::Connection<C, B>>,
    _marker: PhantomData<B>,
}

impl<'a, C, B> Future for AcceptUni<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    type Output = Result<Option<(SessionId, RecvStream<C::RecvStream>)>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        tracing::trace!("poll: read_uni_stream");

        let mut conn = self.conn.lock().unwrap();
        conn.inner.poll_accept_recv(cx)?;

        // Get the currently available streams
        let streams = conn.inner.accepted_streams_mut();
        if let Some(v) = streams.uni_streams.pop() {
            tracing::info!("Got uni stream");
            return Poll::Ready(Ok(Some(v)));
        }

        tracing::debug!("Waiting on incoming streams");

        Poll::Pending
    }
}

fn validate_wt_connect(request: &Request<()>) -> bool {
    matches!((request.method(), request.extensions().get::<Protocol>()), (&Method::CONNECT, Some(p)) if p == &Protocol::WEB_TRANSPORT)
}
