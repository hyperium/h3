//! Provides the server side WebTransport session

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    task::{Context, Poll},
};

use crate::{
    connection::ConnectionState,
    error::{Code, ErrorLevel},
    frame::FrameStream,
    proto::{datagram::Datagram, frame::Frame, stream::Dir},
    quic::{self, BidiStream as _, OpenStreams, RecvStream as _, SendStream as _, WriteBuf},
    request::ResolveRequest,
    server::{self, Connection, RequestStream},
    stream::{BidiStreamHeader, BufRecvStream, UniStreamHeader},
    Error, Protocol,
};
use bytes::{Buf, Bytes};
use futures_util::{future::poll_fn, ready, stream::FuturesUnordered, Future, StreamExt};
use http::{Method, Request, Response, StatusCode};
use tokio::sync::mpsc;

use super::{
    accept::AcceptBi as AcceptIncomingBi,
    stream::{self, RecvStream, SendStream},
    SessionId,
};

/// Accepts and manages WebTransport sessions
pub struct WebTransportServer<C: quic::Connection> {
    inner: Arc<Mutex<WConn<C>>>,
}

impl<C: quic::Connection> WebTransportServer<C> {
    /// Creates a new WebTransport server from an HTTP/3 connection.
    pub fn new(conn: Connection<C>) -> Self {
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

        let opener = conn.inner.conn.lock().unwrap().opener();

        let inner = WConn {
            conn,
            sessions: Default::default(),
            pending_bi: Default::default(),
            pending_requests: Default::default(),
            bi_streams: Default::default(),
            opener,
        };

        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    /// Accepts the next WebTransport session.
    pub async fn accept_session(&self) -> Result<Option<WebTransportSession<C>>, Error> {
        loop {
            let req = self.inner().pending_requests.pop();
            if let Some(req) = req {
                let (request, mut stream) = req.resolve().await?;

                if request.method() != Method::CONNECT {
                    // TODO buffer this request for someone else
                }

                {
                    let mut inner = self.inner();

                    let shared = inner.conn.shared_state().clone();
                    {
                        let config = shared.write("Read WebTransport support").config;

                        tracing::debug!("Client settings: {:#?}", config);
                        if !config.enable_webtransport {
                            return Err(inner.conn.close(
                                Code::H3_SETTINGS_ERROR,
                                "webtransport is not supported by client",
                            ));
                        }

                        if !config.enable_datagram {
                            return Err(inner.conn.close(
                                Code::H3_SETTINGS_ERROR,
                                "datagrams are not supported by client",
                            ));
                        }
                    }

                    // Create a channel to communicate with the session

                    let session_id = stream.send_id().into();

                    // Ensure the session is inserted **before** sending and awaiting the response
                    //
                    // This is because streams can be opened by the client before the response it received
                    inner.sessions.insert(session_id);
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

                tracing::info!("Stream id: {}", stream.id());
                let session_id = stream.send_id().into();
                tracing::info!("Established new WebTransport session with id {session_id:?}");

                let session = WebTransportSession {
                    session_id,
                    conn: self.inner.clone(),
                    connect_stream: stream,
                };

                return Ok(Some(session));
            } else {
                match poll_fn(|cx| self.inner().poll_incoming_bidi(cx)).await {
                    Ok(Some(())) => {}
                    Ok(None) => return Ok(None),
                    Err(err) => return Err(err),
                }
            }
        }
    }

    fn inner(&self) -> MutexGuard<'_, WConn<C>> {
        self.inner.lock().unwrap()
    }
}

/// Manages multiple connected WebTransport sessions.
struct WConn<C: quic::Connection> {
    conn: Connection<C>,

    sessions: HashSet<SessionId>,

    /// Bidirectional streams which have not yet read the first frame
    /// This determines if it is a webtransport stream or a request stream
    pending_bi: FuturesUnordered<AcceptIncomingBi<C>>,

    pending_requests: Vec<ResolveRequest<C>>,
    bi_streams: Vec<(
        SessionId,
        SendStream<C::SendStream>,
        RecvStream<C::RecvStream>,
    )>,
    opener: C::OpenStreams,
}

impl<C: quic::Connection> WConn<C> {
    /// Accepts the next request.
    ///
    /// *CONNECT* requests for the WebTransport protocol are intercepted. Use [`Self::accept_session`]
    pub async fn accept_request(&self) -> (Request<()>, RequestStream<C::BidiStream>) {
        todo!()
    }

    /// Polls for the next incoming request or bidi stream
    fn poll_incoming_bidi(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<()>, Error>> {
        loop {
            let conn = &mut self.conn;
            // Handle any pending requests
            if let Poll::Ready(Some((frame, stream))) = self.pending_bi.poll_next_unpin(cx) {
                match frame {
                    Ok(Some(Frame::WebTransportStream(session_id))) => {
                        tracing::debug!("Got webtransport stream for {session_id:?}");

                        if let Some(session) = self.sessions.get(&session_id) {
                            let (send, recv) = stream.into_inner().split();
                            self.bi_streams.push((
                                session_id,
                                SendStream::new(send),
                                RecvStream::new(recv),
                            ));
                        }

                        return Poll::Ready(Ok(Some(())));
                    }
                    frame => {
                        match conn.accept_with_frame(stream, frame) {
                            Ok(Some(request)) => {
                                self.pending_requests.push(request);
                                return Poll::Ready(Ok(Some(())));
                            }
                            Ok(None) => {
                                // Connection is closed
                                return Poll::Ready(Ok(None));
                            }
                            Err(err) => {
                                tracing::debug!("Error accepting request: {err}");
                                return Poll::Ready(Err(err));
                            }
                        }
                    }
                }
            }

            // Accept *new* incoming bidirectional stream
            //
            // This could be an HTTP/3 request, or a webtransport bidi stream.
            let stream = ready!(self.conn.poll_accept_bi(cx)?);

            if let Some(stream) = stream {
                let stream = FrameStream::new(BufRecvStream::new(stream));
                self.pending_bi.push(AcceptIncomingBi::new(stream));
                // Go back around and poll the streams
            } else {
                // Connection is closed
                return Poll::Ready(Ok(None));
            }
        }

        // Peek the first varint to determine the type
    }
}

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
    session_id: SessionId,
    conn: Arc<Mutex<WConn<C>>>,
    connect_stream: RequestStream<C::BidiStream>,
}

impl<C> WebTransportSession<C>
where
    C: quic::Connection,
{
    /// Receive a datagram from the client
    pub fn accept_datagram(&self) -> ReadDatagram<C> {
        todo!()
        // ReadDatagram {
        // conn: self.conn.lock().unwrap().inner.conn.clone(),
        // }
    }

    /// Sends a datagram
    ///
    /// TODO: maybe make async. `quinn` does not require an async send
    pub fn send_datagram(&self, data: impl Buf) -> Result<(), Error> {
        self.conn
            .lock()
            .unwrap()
            .conn
            .send_datagram(self.connect_stream.id(), data)?;

        Ok(())
    }

    /// Accept an incoming unidirectional stream from the client, it reads the stream until EOF.
    pub fn accept_uni(&self) -> AcceptUni<C> {
        todo!()
        // AcceptUni { conn: &self.conn }
    }

    /// Accepts an incoming bidirectional stream or request
    pub fn accept_bi(&self) -> AcceptBi<C> {
        AcceptBi {
            conn: &self.conn,
            session_id: self.session_id,
        }
    }

    /// Open a new bidirectional stream
    pub fn open_bi(&self, session_id: SessionId) -> OpenBi<C> {
        OpenBi {
            conn: &self.conn,
            stream: None,
            session_id,
        }
    }

    /// Open a new unidirectional stream
    pub fn open_uni(&self, session_id: SessionId) -> OpenUni<C> {
        OpenUni {
            conn: &self.conn,
            stream: None,
            session_id,
        }
    }

    /// Returns the session id
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}

/// Streams are opened, but the initial webtransport header has not been sent
type PendingStreams<C> = (
    SendStream<<C as quic::Connection>::SendStream>,
    RecvStream<<C as quic::Connection>::RecvStream>,
    WriteBuf<&'static [u8]>,
);

/// Streams are opened, but the initial webtransport header has not been sent
type PendingUniStreams<C> = (
    SendStream<<C as quic::Connection>::SendStream>,
    WriteBuf<&'static [u8]>,
);

#[pin_project::pin_project]
/// Future for opening a bidi stream
pub struct OpenBi<'a, C: quic::Connection> {
    conn: &'a Mutex<WConn<C>>,
    stream: Option<PendingStreams<C>>,
    session_id: SessionId,
}

impl<'a, C: quic::Connection> Future for OpenBi<'a, C> {
    type Output = Result<(SendStream<C::SendStream>, RecvStream<C::RecvStream>), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut p = self.project();
        loop {
            match &mut p.stream {
                Some((send, _, buf)) => {
                    while buf.has_remaining() {
                        let n = ready!(send.poll_send(cx, buf))?;
                        tracing::debug!("Wrote {n} bytes");
                    }

                    tracing::debug!("Finished sending header frame");

                    let (send, recv, _) = p.stream.take().unwrap();
                    return Poll::Ready(Ok((send, recv)));
                }
                None => {
                    let mut conn = (*p.conn).lock().unwrap();
                    let opener = &mut conn.opener;
                    // Open the stream first
                    let res = ready!(opener.poll_open_bidi(cx))?;
                    let (send, recv) = BufRecvStream::new(res).split();

                    let send: SendStream<C::SendStream> = SendStream::new(send);
                    let recv = RecvStream::new(recv);

                    let buf = WriteBuf::from(BidiStreamHeader::WebTransportBidi(*p.session_id));
                    *p.stream = Some((send, recv, buf));
                }
            }
        }
    }
}

#[pin_project::pin_project]
/// Future for opening a uni stream
pub struct OpenUni<'a, C: quic::Connection> {
    conn: &'a Mutex<WConn<C>>,
    stream: Option<PendingUniStreams<C>>,
    session_id: SessionId,
}

impl<'a, C: quic::Connection> Future for OpenUni<'a, C> {
    type Output = Result<SendStream<C::SendStream>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut p = self.project();
        loop {
            match &mut p.stream {
                Some((send, buf)) => {
                    while buf.has_remaining() {
                        ready!(send.poll_send(cx, buf))?;
                    }
                    let (send, buf) = p.stream.take().unwrap();
                    assert!(!buf.has_remaining());
                    return Poll::Ready(Ok(send));
                }
                None => {
                    let mut conn = (*p.conn).lock().unwrap();
                    let opener = &mut conn.opener;

                    let send = ready!(opener.poll_open_uni(cx))?;
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
pub enum AcceptedBi<C: quic::Connection> {
    /// An incoming bidirectional stream
    BidiStream(
        SessionId,
        SendStream<C::SendStream>,
        RecvStream<C::RecvStream>,
    ),
    /// An incoming HTTP/3 request, passed through a webtransport session.
    ///
    /// This makes it possible to respond to multiple CONNECT requests
    Request(Request<()>, RequestStream<C::BidiStream>),
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
            Some(v) => {
                let datagram = Datagram::decode(v)?;
                Poll::Ready(Ok(Some((datagram.stream_id().into(), datagram.payload))))
            }
            None => Poll::Ready(Ok(None)),
        }
    }
}

/// Accepts the next incoming bidirectional stream
pub struct AcceptBi<'a, C>
where
    C: quic::Connection,
{
    conn: &'a Mutex<WConn<C>>,

    session_id: SessionId,
}

impl<'a, C> Future for AcceptBi<'a, C>
where
    C: quic::Connection,
{
    type Output = Result<Option<(SendStream<C::SendStream>, RecvStream<C::RecvStream>)>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            let mut inner = self.conn.lock().unwrap();

            if let Some(idx) = inner
                .bi_streams
                .iter()
                .position(|(id, _, _)| *id == self.session_id)
            {
                let (_, send, recv) = inner.bi_streams.remove(idx);
                return Poll::Ready(Ok(Some((send, recv))));
            }

            match ready!(inner.poll_incoming_bidi(cx)) {
                Ok(Some(())) => {}
                Ok(None) => return Poll::Ready(Ok(None)),
                Err(err) => return Poll::Ready(Err(err)),
            }
        }
    }
}

/// Future for [`WebTransportSession::accept_uni`]
pub struct AcceptUni<'a, C>
where
    C: quic::Connection,
{
    conn: &'a Mutex<WConn<C>>,

    session_id: SessionId,
}

impl<'a, C> Future for AcceptUni<'a, C>
where
    C: quic::Connection,
{
    type Output = Result<Option<(SessionId, stream::RecvStream<C::RecvStream>)>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        tracing::trace!("poll: read_uni_stream");

        todo!()
        // let mut conn = self.conn.lock().unwrap();
        // conn.poll_accept_recv(cx)?;

        // // Get the currently available streams
        // let streams = conn.inner.accepted_streams_mut();
        // if let Some(v) = streams.uni_streams.pop() {
        //     tracing::info!("Got uni stream");
        //     return Poll::Ready(Ok(Some(v)));
        // }

        // tracing::debug!("Waiting on incoming streams");

        // Poll::Pending
    }
}

fn validate_wt_connect(request: &Request<()>) -> bool {
    let protocol = request.extensions().get::<Protocol>();
    matches!((request.method(), protocol), (&Method::CONNECT, Some(p)) if p == &Protocol::WEB_TRANSPORT)
}
