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
        StreamError,
    },
    ext::Protocol,
    proto::frame::Frame,
    quic::{self, OpenStreams, WriteBuf},
    server::{Connection, RequestResolver, RequestStream},
    ConnectionState, SharedState,
};
use h3::{
    quic::SendStreamUnframed,
    stream::{BidiStreamHeader, BufRecvStream, UniStreamHeader},
};
use h3_datagram::{
    datagram_handler::{DatagramSender, HandleDatagramsExt, SendDatagramError},
    quic_traits,
};
use http::{Method, Request, Response, StatusCode};

use h3::webtransport::SessionId;
use pin_project_lite::pin_project;
use tokio::sync::Mutex as AsyncMutex;

use crate::stream::{BidiStream, SendStream};

mod connection;
pub use connection::{IncomingStream, WebTransportConnection};

/// One accepted WebTransport session.
///
/// Owns its CONNECT stream and outgoing stream capabilities, not the HTTP/3
/// connection. Keep driving [`WebTransportConnection`] independently to receive
/// HTTP requests and incoming WebTransport streams for every session.
///
/// Dropping a session does not drop the connection driver or other sessions.
pub struct WebTransportSession<C, B>
where
    C: quic::Connection<B> + quic_traits::DatagramConnectionExt<B>,
    Connection<C, B>: HandleDatagramsExt<C, B>,
    B: Buf,
{
    // See: https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/#section-2-3
    session_id: SessionId,
    connect: AsyncMutex<SessionConnect<C::BidiStream, B>>,
    opener: Mutex<C::OpenStreams>,
    datagram_sender: Mutex<DatagramSender<C::SendDatagramHandler, B>>,
    /// Shared State
    ///
    /// Shared with the connection driver, without owning that driver.
    shared: Arc<SharedState>,
}

struct SessionConnect<S, B> {
    stream: RequestStream<S, B>,
    closed: Option<Result<(), Arc<StreamError>>>,
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
    /// The returned future owns only session resources. It does not borrow the
    /// connection, so response flow control cannot block the caller from driving
    /// other requests. The caller remains responsible for authorization and for
    /// driving the connection throughout the session's lifetime.
    pub fn accept(
        request: Request<()>,
        stream: RequestStream<C::BidiStream, B>,
        conn: &mut Connection<C, B>,
    ) -> impl Future<Output = Result<Self, StreamError>> {
        let prepared = Self::prepare(stream, conn);
        async move {
            let session = prepared?;
            let valid = validate_wt_connect(&request);
            let mut response = Response::builder().status(if valid {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            });
            if valid {
                response = response.header("sec-webtransport-http3-draft", "draft02");
            }
            {
                let mut connect = session.connect.lock().await;
                connect
                    .stream
                    .send_response(response.body(()).unwrap())
                    .await?;
                if !valid {
                    connect.stream.finish().await?;
                    return Err(StreamError::StreamError {
                        code: Code::H3_MESSAGE_ERROR,
                        reason: "invalid WebTransport CONNECT request".into(),
                    });
                }
            }
            Ok(session)
        }
    }

    fn prepare(
        stream: RequestStream<C::BidiStream, B>,
        conn: &mut Connection<C, B>,
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

        let session_id = stream.send_id().into();
        let datagram_sender = conn.get_datagram_sender(stream.send_id());
        let conn_inner = &mut conn.inner.conn;
        let opener = Mutex::new(conn_inner.opener());

        Ok(Self {
            session_id,
            opener,
            connect: AsyncMutex::new(SessionConnect {
                stream,
                closed: None,
            }),
            datagram_sender: Mutex::new(datagram_sender),
            shared,
        })
    }

    /// Send a datagram bound to this session's CONNECT stream.
    pub fn send_datagram(&self, data: B) -> Result<(), SendDatagramError> {
        self.datagram_sender.lock().unwrap().send_datagram(data)
    }

    /// Open a new bidirectional stream
    pub fn open_bi(&self) -> OpenBi<'_, C, B> {
        OpenBi {
            opener: &self.opener,
            stream: None,
            session_id: self.session_id,
            stream_handler: WTransportStreamHandler {
                shared: self.shared.clone(),
            },
        }
    }

    /// Open a new unidirectional stream
    pub fn open_uni(&self) -> OpenUni<'_, C, B> {
        OpenUni {
            opener: &self.opener,
            stream: None,
            session_id: self.session_id,
            stream_handler: WTransportStreamHandler {
                shared: self.shared.clone(),
            },
        }
    }

    /// Returns the session id
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Wait for the peer's CONNECT stream to end.
    ///
    /// Returns `Ok(())` on receive-side FIN, or the stream/connection error on
    /// reset or connection loss. The terminal result is retained for subsequent
    /// and concurrent callers; errors are shared because [`StreamError`] is not
    /// cloneable. Cancellation leaves the CONNECT reader with the session, so
    /// another call can continue waiting without losing framing state.
    ///
    /// This consumes and discards CONNECT body data without buffering a whole
    /// body. It does not interpret capsules or expose their close codes. A
    /// capsule without FIN is not reported as closure by this API.
    ///
    /// Keep driving [`WebTransportConnection`] independently. Waiting does not
    /// borrow that driver, prevent outgoing session operations, or receive any
    /// application streams or datagrams. GOAWAY alone does not complete this
    /// future. Completion does not close the connection, cancel sibling sessions
    /// or terminate admitted streams: their owner still decides how to drain.
    pub async fn closed(&self) -> Result<(), Arc<StreamError>> {
        let mut connect = self.connect.lock().await;
        if let Some(result) = &connect.closed {
            return result.clone();
        }
        let result = loop {
            match connect.stream.recv_data().await {
                Ok(Some(_)) => {}
                Ok(None) => break Ok(()),
                Err(error) => break Err(Arc::new(error)),
            }
        };
        connect.closed = Some(result.clone());
        result
    }
}

/// Resolves an accepted bidirectional stream as either WebTransport or HTTP/3.
pub struct BiStreamResolver<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    resolver: RequestResolver<C, B>,
}

impl<C, B> BiStreamResolver<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// Reads the first frame and classifies the accepted stream.
    pub async fn resolve(mut self) -> Result<AcceptedBi<C, B>, StreamError> {
        let frame = poll_fn(|cx| self.resolver.frame_stream.poll_next(cx)).await;

        match frame {
            Ok(Some(Frame::WebTransportStream(session_id))) => {
                // Take the stream out of the framed reader and split it in half like Paul Allen
                let stream = self.resolver.frame_stream.into_inner();
                Ok(AcceptedBi::BidiStream(session_id, BidiStream::new(stream)))
            }
            // Make the underlying HTTP/3 connection handle the rest
            frame => {
                let (req, resp) = self.resolver.accept_with_frame(frame)?.resolve().await?;
                Ok(AcceptedBi::Request(req, resp))
            }
        }
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
#[allow(clippy::large_enum_variant)]
pub enum AcceptedBi<C: quic::Connection<B>, B: Buf> {
    /// An incoming bidirectional stream
    BidiStream(SessionId, BidiStream<C::BidiStream, B>),
    /// An incoming HTTP/3 request, including another CONNECT request.
    ///
    /// This makes it possible to respond to multiple CONNECT requests
    Request(Request<()>, RequestStream<C::BidiStream, B>),
}

fn validate_wt_connect(request: &Request<()>) -> bool {
    let protocol = request.extensions().get::<Protocol>();
    matches!((request.method(), protocol), (&Method::CONNECT, Some(p)) if p == &Protocol::WEB_TRANSPORT)
}
