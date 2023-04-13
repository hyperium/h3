//! Provides the server side WebTransport session

use std::{
    marker::PhantomData,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use crate::{
    connection::ConnectionState,
    error::{Code, ErrorLevel},
    frame::FrameStream,
    proto::{datagram::Datagram, frame::Frame},
    quic::{self, BidiStream, SendStream},
    server::{self, Connection, RequestStream},
    Error, Protocol,
};
use bytes::{Buf, Bytes};
use futures_util::{future::poll_fn, ready, Future};
use http::{Method, Request, Response, StatusCode};
use quic::StreamId;

use super::{
    stream::{self},
    SessionId,
};

/// WebTransport session driver.
///
/// Maintains the session using the underlying HTTP/3 connection.
///
/// Similar to [`crate::Connection`] it is generic over the QUIC implementation and Buffer.
pub struct WebTransportSession<C>
where
    C: quic::Connection,
{
    // See: https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/#section-2-3
    session_id: StreamId,
    conn: Mutex<Connection<C>>,
    connect_stream: RequestStream<C::BidiStream>,
}

impl<C> WebTransportSession<C>
where
    C: quic::Connection,
{
    /// Accepts a *CONNECT* request for establishing a WebTransport session.
    ///
    /// TODO: is the API or the user responsible for validating the CONNECT request?
    pub async fn accept(
        request: Request<()>,
        mut stream: RequestStream<C::BidiStream>,
        mut conn: Connection<C>,
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
    pub fn read_datagram(&self) -> ReadDatagram<C> {
        ReadDatagram {
            conn: self.conn.lock().unwrap().inner.conn.clone(),
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
    pub fn accept_uni(&self) -> AcceptUni<C> {
        AcceptUni { conn: &self.conn }
    }

    /// Accepts an incoming bidirectional stream
    /// TODO: should this return an enum of BiStream/Request to allow ordinary HTTP/3 requests to
    /// pass through?
    ///
    /// Currently, they are ignored, and the function loops until it receives a *webtransport*
    /// stream.
    pub async fn accept_bi(
        &self,
    ) -> Result<
        Option<(
            SessionId,
            stream::SendStream<C::SendStream>,
            stream::RecvStream<C::RecvStream>,
        )>,
        Error,
    > {
        loop {
            // Get the next stream
            // Accept the incoming stream
            let stream = poll_fn(|cx| {
                let mut conn = self.conn.lock().unwrap();
                conn.poll_accept_request(cx)
            })
            .await;

            tracing::debug!("Received biderectional stream");

            let mut stream = match stream {
                Ok(Some(s)) => FrameStream::new(s),
                Ok(None) => {
                    // We always send a last GoAway frame to the client, so it knows which was the last
                    // non-rejected request.
                    // self.shutdown(0).await?;
                    todo!("shutdown");
                    // return Ok(None);
                }
                Err(err) => {
                    match err.inner.kind {
                        crate::error::Kind::Closed => return Ok(None),
                        crate::error::Kind::Application {
                            code,
                            reason,
                            level: ErrorLevel::ConnectionError,
                        } => {
                            return Err(self.conn.lock().unwrap().close(
                                code,
                                reason.unwrap_or_else(|| String::into_boxed_str(String::from(""))),
                            ))
                        }
                        _ => return Err(err),
                    };
                }
            };

            tracing::debug!("Reading first frame");
            // Read the first frame.
            //
            // This wil determine if it is a webtransport bi-stream or a request stream
            let frame = poll_fn(|cx| stream.poll_next(cx)).await;

            let mut conn = self.conn.lock().unwrap();
            match frame {
                Ok(None) => return Ok(None),
                Ok(Some(Frame::WebTransportStream(session_id))) => {
                    tracing::info!("Got webtransport stream");
                    // Take the stream out of the framed reader
                    let (stream, buf) = stream.into_inner();
                    let (send, recv) = stream.split();
                    // Don't lose already read data
                    let recv = stream::RecvStream::new(buf, recv);
                    let send = stream::SendStream::new(send);
                    return Ok(Some((session_id, send, recv)));
                }
                // Not a webtransport stream, discard
                //
                // Find a workaround to return this request, in order to accept more sessions
                Ok(Some(_frame)) => continue,
                Err(e) => {
                    let err: Error = e.into();
                    if err.is_closed() {
                        return Ok(None);
                    }
                    match err.inner.kind {
                        crate::error::Kind::Closed => return Ok(None),
                        crate::error::Kind::Application {
                            code,
                            reason,
                            level: ErrorLevel::ConnectionError,
                        } => {
                            return Err(conn.close(
                                code,
                                reason.unwrap_or_else(|| String::into_boxed_str(String::from(""))),
                            ))
                        }
                        crate::error::Kind::Application {
                            code,
                            reason: _,
                            level: ErrorLevel::StreamError,
                        } => {
                            stream.reset(code.into());
                            return Err(err);
                        }
                        _ => return Err(err),
                    };
                }
            }
        }
    }
}

/// Future for [`Connection::read_datagram`]
pub struct ReadDatagram<C>
where
    C: quic::Connection,
{
    conn: Arc<Mutex<C>>,
}

impl<C> Future for ReadDatagram<C>
where
    C: quic::Connection,
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
pub struct AcceptUni<'a, C>
where
    C: quic::Connection,
{
    conn: &'a Mutex<server::Connection<C>>,
}

impl<'a, C> Future for AcceptUni<'a, C>
where
    C: quic::Connection,
{
    type Output = Result<Option<(SessionId, stream::RecvStream<C::RecvStream>)>, Error>;

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
    let protocol = request.extensions().get::<Protocol>();
    matches!((request.method(), protocol), (&Method::CONNECT, Some(p)) if p == &Protocol::WEB_TRANSPORT)
}
