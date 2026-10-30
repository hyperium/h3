//! Provides the server side WebTransport session

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::{Buf, Bytes};
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

use crate::stream::{BidiStream, SendStream};

mod capsule;
mod connection;
mod session;
pub use connection::{IncomingStream, WebTransportConnection};
use session::{SessionState, Sessions};

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
    state: Arc<SessionState<C, B>>,
    opener: Mutex<C::OpenStreams>,
    datagram_sender: Mutex<DatagramSender<C::SendDatagramHandler, B>>,
    /// Shared State
    ///
    /// Shared with the connection driver, without owning that driver.
    shared: Arc<SharedState>,
}

/// Application-provided WebTransport session close information.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionClose {
    /// The application's 32-bit close code, without HTTP/3 stream-code mapping.
    pub code: u32,
    /// The peer's UTF-8 close reason, at most 1024 bytes.
    pub reason: String,
}

/// Failure to send a datagram through a WebTransport session.
#[derive(Debug)]
pub enum SessionDatagramError {
    /// The session has already terminated.
    Closed,
    /// The QUIC datagram transport rejected the send.
    Transport(SendDatagramError),
}

impl std::fmt::Display for SessionDatagramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("WebTransport session closed"),
            Self::Transport(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SessionDatagramError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Closed => None,
            Self::Transport(error) => Some(error),
        }
    }
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
        conn: &mut WebTransportConnection<C, B>,
    ) -> impl Future<Output = Result<Self, StreamError>> {
        conn.accept_session(request, stream)
    }

    fn accept_with_state(
        request: Request<()>,
        mut stream: RequestStream<C::BidiStream, B>,
        conn: &mut Connection<C, B>,
        state: Arc<SessionState<C, B>>,
    ) -> impl Future<Output = Result<Self, StreamError>> {
        let prepared = Self::prepare(&stream, conn, state);
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
            stream.send_response(response.body(()).unwrap()).await?;
            if !valid {
                stream.finish().await?;
                return Err(StreamError::StreamError {
                    code: Code::H3_MESSAGE_ERROR,
                    reason: "invalid WebTransport CONNECT request".into(),
                });
            }
            session.state.attach(stream);
            Ok(session)
        }
    }

    fn prepare(
        stream: &RequestStream<C::BidiStream, B>,
        conn: &mut Connection<C, B>,
        state: Arc<SessionState<C, B>>,
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
        state.reserve()?;

        Ok(Self {
            session_id,
            opener,
            state,
            datagram_sender: Mutex::new(datagram_sender),
            shared,
        })
    }

    /// Send a datagram bound to this session's CONNECT stream.
    pub fn send_datagram(&self, data: B) -> Result<(), SessionDatagramError> {
        self.state
            .while_open(|| self.datagram_sender.lock().unwrap().send_datagram(data))
            .map_err(|_| SessionDatagramError::Closed)?
            .map_err(SessionDatagramError::Transport)
    }

    /// Open a new bidirectional stream
    pub fn open_bi(&self) -> OpenBi<'_, C, B> {
        OpenBi {
            opener: &self.opener,
            state: &self.state,
            changed: self.state.changed.notified(),
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
            state: &self.state,
            changed: self.state.changed.notified(),
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

    /// Wait for session termination and retain the application's close information.
    ///
    /// A peer close capsule returns its code and reason. A clean CONNECT FIN
    /// without a capsule returns code zero and an empty reason. Resets and
    /// connection failures return the transport error. Local `close` also wakes
    /// observers. Concurrent and repeated observers see the retained result.
    ///
    /// Cancellation retains partially read capsules with the session. Keep
    /// driving [`WebTransportConnection`] to process control data even when no
    /// observer is waiting. Termination resets this session's data streams;
    /// other sessions and HTTP requests remain independent. Drain application
    /// work before closing the session, not after its CONNECT has ended.
    pub async fn closed(&self) -> Result<SessionClose, Arc<StreamError>> {
        let notified = self.state.changed.notified();
        let mut notified = std::pin::pin!(notified);
        poll_fn(|cx| {
            if notified.as_mut().poll(cx).is_ready() {
                notified.set(self.state.changed.notified());
                let _ = notified.as_mut().poll(cx);
            }
            self.state.poll_io(cx);
            match self.state.outcome() {
                Some(result) => Poll::Ready(result),
                None => Poll::Pending,
            }
        })
        .await
    }

    /// Close only this session, sending a close capsule followed by FIN.
    ///
    /// Reasons longer than 1024 UTF-8 bytes are rejected before changing session
    /// state. The first local or remote termination wins. Cancellation retains
    /// queued DATA and FIN progress; the driver or a later call resumes the same
    /// capsule instead of sending it twice. Completion means local write
    /// completion, not acknowledgement by the peer.
    pub async fn close(&self, code: u32, reason: &str) -> Result<(), Arc<StreamError>>
    where
        B: From<Bytes>,
    {
        let wire = capsule::encode(code, reason).map_err(|reason| {
            Arc::new(StreamError::StreamError {
                code: Code::H3_MESSAGE_ERROR,
                reason: reason.into(),
            })
        })?;
        let started = {
            let mut io = self.state.connect.lock().unwrap();
            let connect = io.as_mut().expect("accepted session has a CONNECT stream");
            if !connect.finishing && self.state.outcome().is_none() {
                connect.outgoing = Some(B::from(wire));
                connect.finishing = true;
                true
            } else {
                false
            }
        };
        if started {
            self.state.terminate(Ok(SessionClose {
                code,
                reason: reason.to_owned(),
            }));
        }
        let notified = self.state.changed.notified();
        let mut notified = std::pin::pin!(notified);
        poll_fn(|cx| {
            if notified.as_mut().poll(cx).is_ready() {
                notified.set(self.state.changed.notified());
                let _ = notified.as_mut().poll(cx);
            }
            self.state.poll_io(cx);
            let io = self.state.connect.lock().unwrap();
            let connect = io.as_ref().unwrap();
            if connect.sent {
                Poll::Ready(match &connect.send_error {
                    Some(error) => Err(error.clone()),
                    None => Ok(()),
                })
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

impl<C, B> Drop for WebTransportSession<C, B>
where
    C: quic::Connection<B> + quic_traits::DatagramConnectionExt<B>,
    Connection<C, B>: HandleDatagramsExt<C, B>,
    B: Buf,
{
    fn drop(&mut self) {
        self.state.abort();
    }
}

/// Resolves an accepted bidirectional stream as either WebTransport or HTTP/3.
pub struct BiStreamResolver<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    resolver: RequestResolver<C, B>,
    sessions: Arc<Mutex<Sessions<C, B>>>,
}

impl<C, B> BiStreamResolver<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// Reads the first frame and classifies the accepted stream.
    pub async fn resolve(mut self) -> Result<AcceptedBi<C, B>, StreamError> {
        let request_id = self.resolver.frame_stream.id().into();
        let frame = poll_fn(|cx| self.resolver.frame_stream.poll_next(cx)).await;

        match frame {
            Ok(Some(Frame::WebTransportStream(session_id))) => {
                // Take the stream out of the framed reader and split it in half like Paul Allen
                let stream = self.resolver.frame_stream.into_inner();
                let stream = BidiStream::new(stream);
                let (state, retired) = {
                    let mut sessions = self.sessions.lock().unwrap();
                    let retired = sessions.retire(request_id);
                    (sessions.get(session_id), retired)
                };
                if let Some(retired) = retired {
                    retired.terminate(Err(Arc::new(session::gone())));
                }
                state.bidi(&stream.stream);
                Ok(AcceptedBi::BidiStream(session_id, stream))
            }
            // Make the underlying HTTP/3 connection handle the rest
            frame => {
                let (req, resp) = self.resolver.accept_with_frame(frame)?.resolve().await?;
                if !validate_wt_connect(&req) {
                    let retired = self.sessions.lock().unwrap().retire(request_id);
                    if let Some(retired) = retired {
                        retired.terminate(Err(Arc::new(session::gone())));
                    }
                }
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
        state: &'a Arc<SessionState<C, B>>,
        #[pin]
        changed: tokio::sync::futures::Notified<'a>,
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
        let _ = p.changed.as_mut().poll(cx);
        if p.state.is_closed() {
            return Poll::Ready(Err(session::gone()));
        }
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
                    p.state.bidi(&stream.stream);

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
        state: &'a Arc<SessionState<C, B>>,
        #[pin]
        changed: tokio::sync::futures::Notified<'a>,
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
        let _ = p.changed.as_mut().poll(cx);
        if p.state.is_closed() {
            return Poll::Ready(Err(session::gone()));
        }
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
                    p.state.send(&send.stream);

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
