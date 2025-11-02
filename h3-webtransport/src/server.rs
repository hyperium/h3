//! Provides the server side WebTransport session

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::Buf;
use futures_util::{future::poll_fn, ready, Future};
use h3::{
    error::{
        connection_error_creators::CloseStream, internal_error::InternalConnectionError, Code,
        ConnectionError, StreamError,
    },
    ext::Protocol,
    frame::FrameStream,
    proto::frame::Frame,
    quic::{self, OpenStreams, WriteBuf},
    server::{Connection, RequestStream},
    ConnectionState, SharedState,
};
use h3::{
    quic::SendStreamUnframed,
    stream::{BidiStreamHeader, BufRecvStream, UniStreamHeader},
};
use h3_datagram::{
    datagram_handler::{DatagramReader, DatagramSender, HandleDatagramsExt},
    quic_traits,
};
use http::{Method, Request, Response, StatusCode};

use h3::webtransport::SessionId;
use pin_project_lite::pin_project;

use crate::stream::{BidiStream, RecvStream, SendStream};

/// WebTransport session driver.
///
/// Maintains the session using the underlying HTTP/3 connection.
///
/// Similar to [`h3::server::Connection`](https://docs.rs/h3/latest/h3/server/struct.Connection.html) it is generic over the QUIC implementation and Buffer.
pub struct WebTransportSession<C, B>
where
    C: quic::Connection<B> + quic_traits::DatagramConnectionExt<B>,
    Connection<C, B>: HandleDatagramsExt<C, B>,
    B: Buf,
{
    // See: https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/#section-2-3
    session_id: SessionId,
    /// The underlying HTTP/3 connection
    server_conn: Mutex<Connection<C, B>>,
    connect_stream: RequestStream<C::BidiStream, B>,
    opener: Mutex<C::OpenStreams>,
    /// Shared State
    ///
    /// Shared state is already in server_conn, but with this it is not necessary to lock the mutex
    shared: Arc<SharedState>,
}

impl<C, B> ConnectionState for WebTransportSession<C, B>
where
    C: quic::Connection<B> + quic_traits::DatagramConnectionExt<B>,
    Connection<C, B>: HandleDatagramsExt<C, B>,
    B: Buf,
{
    fn shared_state(&self) -> &SharedState {
        &self.shared
    }
}

impl<C, B> WebTransportSession<C, B>
where
    Connection<C, B>: HandleDatagramsExt<C, B>,
    C: quic::Connection<B> + quic_traits::DatagramConnectionExt<B>,
    B: Buf,
{
    /// Accepts a *CONNECT* request for establishing a WebTransport session.
    ///
    /// TODO: is the API or the user responsible for validating the CONNECT request?
    pub async fn accept(
        request: Request<()>,
        mut stream: RequestStream<C::BidiStream, B>,
        mut conn: Connection<C, B>,
    ) -> Result<Self, StreamError> {
        let shared = conn.inner.shared.clone();

        let config = shared.settings();

        if !config.enable_webtransport() {
            return Err(StreamError::ConnectionError(
                conn.inner
                    .handle_connection_error(InternalConnectionError::new(
                        Code::H3_SETTINGS_ERROR,
                        "webtransport is not supported by client".to_string(),
                    )),
            ));
        }

        if !config.enable_datagram() {
            return Err(StreamError::ConnectionError(
                conn.inner
                    .handle_connection_error(InternalConnectionError::new(
                        Code::H3_SETTINGS_ERROR,
                        "datagrams are not supported by client".to_string(),
                    )),
            ));
        }

        // The peer is responsible for validating our side of the webtransport support.
        //
        // However, it is still advantageous to show a log on the server as (attempting) to
        // establish a WebTransportSession without the proper h3 config is usually a mistake.
        if !conn.inner.config.settings.enable_webtransport() {
            tracing::warn!("Server does not support webtransport");
        }

        if !conn.inner.config.settings.enable_datagram() {
            tracing::warn!("Server does not support datagrams");
        }

        if !conn.inner.config.settings.enable_extended_connect() {
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

        stream.send_response(response).await?;

        let session_id = stream.send_id().into();
        let conn_inner = &mut conn.inner.conn;
        let opener = Mutex::new(conn_inner.opener());

        Ok(Self {
            session_id,
            opener,
            server_conn: Mutex::new(conn),
            connect_stream: stream,
            shared,
        })
    }

    /// Receive a datagram from the client
    pub fn datagram_reader(&self) -> DatagramReader<C::RecvDatagramHandler> {
        self.server_conn.lock().unwrap().get_datagram_reader()
    }

    /// Sends a datagram
    ///
    /// TODO: maybe make async. `quinn` does not require an async send
    pub fn datagram_sender(&self) -> DatagramSender<C::SendDatagramHandler, B> {
        self.server_conn
            .lock()
            .unwrap()
            .get_datagram_sender(self.connect_stream.send_id())
    }

    /// Accept an incoming unidirectional stream from the client, it reads the stream until EOF.
    pub fn accept_uni(&self) -> AcceptUni<'_, C, B> {
        AcceptUni {
            conn: &self.server_conn,
        }
    }

    /// Accepts an incoming bidirectional stream or request
    pub async fn accept_bi(&self) -> Result<Option<AcceptedBi<C, B>>, StreamError> {
        let stream = poll_fn(|cx| {
            let mut conn = self.server_conn.lock().unwrap();
            conn.poll_accept_request_stream(cx)
        })
        .await;

        let stream = match stream {
            Ok(Some(s)) => FrameStream::new(BufRecvStream::new(s)),
            Ok(None) => {
                // FIXME: is proper HTTP GoAway shutdown required?
                return Ok(None);
            }
            Err(err) => return Err(StreamError::ConnectionError(err)),
        };

        let mut resolver = { self.server_conn.lock().unwrap().create_resolver(stream) };
        // Read the first frame.
        //
        // This will determine if it is a webtransport bi-stream or a request stream
        let frame = poll_fn(|cx| resolver.frame_stream.poll_next(cx)).await;

        match frame {
            Ok(None) => Ok(None),
            Ok(Some(Frame::WebTransportStream(session_id))) => {
                // Take the stream out of the framed reader and split it in half like Paul Allen
                let stream = resolver.frame_stream.into_inner();
                Ok(Some(AcceptedBi::BidiStream(
                    session_id,
                    BidiStream::new(stream),
                )))
            }
            // Make the underlying HTTP/3 connection handle the rest
            frame => {
                let (req, resp) = resolver.accept_with_frame(frame)?.resolve().await?;
                Ok(Some(AcceptedBi::Request(req, resp)))
            }
        }
    }

    /// Open a new bidirectional stream
    pub fn open_bi(&self, session_id: SessionId) -> OpenBi<'_, C, B> {
        OpenBi {
            opener: &self.opener,
            stream: None,
            session_id,
            stream_handler: WTransportStreamHandler {
                shared: self.shared.clone(),
            },
        }
    }

    /// Open a new unidirectional stream
    pub fn open_uni(&self, session_id: SessionId) -> OpenUni<'_, C, B> {
        OpenUni {
            opener: &self.opener,
            stream: None,
            session_id,
            stream_handler: WTransportStreamHandler {
                shared: self.shared.clone(),
            },
        }
    }

    /// Returns the session id
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}

/// Streams are opened, but the initial webtransport header has not been sent
type PendingStreams<C, B> = (
    BidiStream<<C as quic::OpenStreams<B>>::BidiStream, B>,
    WriteBuf<&'static [u8]>,
);

/// Streams are opened, but the initial webtransport header has not been sent
type PendingUniStreams<C, B> = (
    SendStream<<C as quic::OpenStreams<B>>::SendStream, B>,
    WriteBuf<&'static [u8]>,
);

pin_project! {
    /// Future for opening a bidi stream
    pub struct OpenBi<'a, C:quic::Connection<B>, B:Buf> {
        opener: &'a Mutex<C::OpenStreams>,
        stream: Option<PendingStreams<C,B>>,
        session_id: SessionId,
        stream_handler: WTransportStreamHandler,
    }
}

struct WTransportStreamHandler {
    shared: Arc<SharedState>,
}

impl ConnectionState for WTransportStreamHandler {
    fn shared_state(&self) -> &SharedState {
        &self.shared
    }
}

impl CloseStream for WTransportStreamHandler {}

impl<'a, B, C> Future for OpenBi<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
    C::BidiStream: SendStreamUnframed<B>,
{
    type Output = Result<BidiStream<C::BidiStream, B>, StreamError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut p = self.project();
        loop {
            match &mut p.stream {
                Some((stream, buf)) => {
                    while buf.has_remaining() {
                        ready!(stream.poll_send(cx, buf))
                            .map_err(|err| p.stream_handler.handle_quic_stream_error(err))?;
                    }

                    let (stream, _) = p.stream.take().unwrap();
                    return Poll::Ready(Ok(stream));
                }
                None => {
                    let mut opener = (*p.opener).lock().unwrap();
                    // Open the stream first
                    let res = ready!(opener.poll_open_bidi(cx))
                        .map_err(|err| p.stream_handler.handle_quic_stream_error(err))?;
                    let stream = BidiStream::new(BufRecvStream::new(res));

                    let buf = WriteBuf::from(BidiStreamHeader::WebTransportBidi(*p.session_id));
                    *p.stream = Some((stream, buf));
                }
            }
        }
    }
}

pin_project! {
    /// Opens a unidirectional stream
    pub struct OpenUni<'a, C: quic::Connection<B>, B:Buf> {
        opener: &'a Mutex<C::OpenStreams>,
        stream: Option<PendingUniStreams<C, B>>,
        // Future for opening a uni stream
        session_id: SessionId,
        stream_handler: WTransportStreamHandler
    }
}

impl<'a, C, B> Future for OpenUni<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
    C::SendStream: SendStreamUnframed<B>,
{
    type Output = Result<SendStream<C::SendStream, B>, StreamError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut p = self.project();
        loop {
            match &mut p.stream {
                Some((send, buf)) => {
                    while buf.has_remaining() {
                        ready!(send.poll_send(cx, buf))
                            .map_err(|err| p.stream_handler.handle_quic_stream_error(err))?;
                    }
                    let (send, buf) = p.stream.take().unwrap();
                    assert!(!buf.has_remaining());
                    return Poll::Ready(Ok(send));
                }
                None => {
                    let mut opener = (*p.opener).lock().unwrap();
                    let send = ready!(opener.poll_open_send(cx))
                        .map_err(|err| p.stream_handler.handle_quic_stream_error(err))?;
                    let send = BufRecvStream::new(send);
                    let send = SendStream::new(send);

                    let buf = WriteBuf::from(UniStreamHeader::WebTransportUni(*p.session_id));
                    *p.stream = Some((send, buf));
                }
            }
        }
    }
}

/// An accepted incoming bidirectional stream.
///
/// Since
// Todo
#[allow(clippy::large_enum_variant)]
pub enum AcceptedBi<C: quic::Connection<B>, B: Buf> {
    /// An incoming bidirectional stream
    BidiStream(SessionId, BidiStream<C::BidiStream, B>),
    /// An incoming HTTP/3 request, passed through a webtransport session.
    ///
    /// This makes it possible to respond to multiple CONNECT requests
    Request(Request<()>, RequestStream<C::BidiStream, B>),
}

/// Future for [`WebTransportSession::accept_uni`]
pub struct AcceptUni<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    conn: &'a Mutex<Connection<C, B>>,
}

impl<'a, C, B> Future for AcceptUni<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    type Output = Result<Option<(SessionId, RecvStream<C::RecvStream, B>)>, ConnectionError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut conn = self.conn.lock().unwrap();
        conn.inner.poll_accept_recv(cx)?;

        // Get the currently available streams
        let streams = conn.inner.accepted_streams_mut();
        if let Some((id, stream)) = streams.wt_uni_streams.pop() {
            return Poll::Ready(Ok(Some((id, RecvStream::new(stream)))));
        }

        Poll::Pending
    }
}

fn validate_wt_connect(request: &Request<()>) -> bool {
    let protocol = request.extensions().get::<Protocol>();
    matches!((request.method(), protocol), (&Method::CONNECT, Some(p)) if p == &Protocol::WEB_TRANSPORT)
}
