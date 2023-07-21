//! Provides the server side WebTransport session

use std::{
    marker::PhantomData,
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};

use bytes::Buf;
use futures_util::{future::poll_fn, ready, Future};
use h3::{
    connection::ConnectionState,
    error::{Code, ErrorLevel},
    ext::{Datagram, Protocol},
    frame::FrameStream,
    proto::frame::Frame,
    quic::{self, OpenStreams, RecvDatagramExt, SendDatagramExt, WriteBuf},
    server::{self, Connection, RequestStream},
    Error,
};
use h3::{
    quic::SendStreamUnframed,
    stream::{BidiStreamHeader, BufRecvStream, UniStreamHeader},
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
    C: quic::Connection<B>,
    B: Buf,
{
    // See: https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/#section-2-3
    session_id: SessionId,
    /// The underlying HTTP/3 connection
    server_conn: Mutex<Connection<C, B>>,
    connect_stream: RequestStream<C::BidiStream, B>,
    opener: Mutex<C::OpenStreams>,
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
        let shared = conn.shared_state().clone();
        {
            let config = shared.write("Read WebTransport support").peer_config;

            if !config.enable_webtransport() {
                return Err(conn.close(
                    Code::H3_SETTINGS_ERROR,
                    "webtransport is not supported by client",
                ));
            }

            if !config.enable_datagram() {
                return Err(conn.close(
                    Code::H3_SETTINGS_ERROR,
                    "datagrams are not supported by client",
                ));
            }
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
        })
    }

    /// Receive a datagram from the client
    pub fn accept_datagram(&self) -> ReadDatagram<C, B> {
        ReadDatagram {
            conn: &self.server_conn,
            _marker: PhantomData,
        }
    }

    /// Sends a datagram
    ///
    /// TODO: maybe make async. `quinn` does not require an async send
    pub fn send_datagram(&self, data: B) -> Result<(), Error>
    where
        C: SendDatagramExt<B>,
    {
        self.server_conn
            .lock()
            .unwrap()
            .send_datagram(self.connect_stream.id(), data)?;

        Ok(())
    }

    /// Accept an incoming unidirectional stream from the client, it reads the stream until EOF.
    pub fn accept_uni(&self) -> AcceptUni<C, B> {
        AcceptUni {
            conn: &self.server_conn,
        }
    }

    /// Accepts an incoming bidirectional stream or request
    pub async fn accept_bi(&self) -> Result<Option<AcceptedBi<C, B>>, Error> {
        // Get the next stream
        // Accept the incoming stream
        let stream = poll_fn(|cx| {
            let mut conn = self.server_conn.lock().unwrap();
            conn.poll_accept_request(cx)
        })
        .await;

        let mut stream = match stream {
            Ok(Some(s)) => FrameStream::new(BufRecvStream::new(s)),
            Ok(None) => {
                // FIXME: is proper HTTP GoAway shutdown required?
                return Ok(None);
            }
            Err(err) => {
                match err.kind() {
                    h3::error::Kind::Closed => return Ok(None),
                    h3::error::Kind::Application {
                        code,
                        reason,
                        level: ErrorLevel::ConnectionError,
                        ..
                    } => {
                        return Err(self.server_conn.lock().unwrap().close(
                            code,
                            reason.unwrap_or_else(|| String::into_boxed_str(String::from(""))),
                        ))
                    }
                    _ => return Err(err),
                };
            }
        };

        // Read the first frame.
        //
        // This will determine if it is a webtransport bi-stream or a request stream
        let frame = poll_fn(|cx| stream.poll_next(cx)).await;

        match frame {
            Ok(None) => Ok(None),
            Ok(Some(Frame::WebTransportStream(session_id))) => {
                // Take the stream out of the framed reader and split it in half like Paul Allen
                let stream = stream.into_inner();

                Ok(Some(AcceptedBi::BidiStream(
                    session_id,
                    BidiStream::new(stream),
                )))
            }
            // Make the underlying HTTP/3 connection handle the rest
            frame => {
                let req = {
                    let mut conn = self.server_conn.lock().unwrap();
                    conn.accept_with_frame(stream, frame)?
                };
                if let Some(req) = req {
                    let (req, resp) = req.resolve().await?;
                    Ok(Some(AcceptedBi::Request(req, resp)))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Open a new bidirectional stream
    pub fn open_bi(&self, session_id: SessionId) -> OpenBi<C, B> {
        OpenBi {
            opener: &self.opener,
            stream: None,
            session_id,
        }
    }

    /// Open a new unidirectional stream
    pub fn open_uni(&self, session_id: SessionId) -> OpenUni<C, B> {
        OpenUni {
            opener: &self.opener,
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
type PendingStreams<C, B> = (
    BidiStream<<C as quic::Connection<B>>::BidiStream, B>,
    WriteBuf<&'static [u8]>,
);

/// Streams are opened, but the initial webtransport header has not been sent
type PendingUniStreams<C, B> = (
    SendStream<<C as quic::Connection<B>>::SendStream, B>,
    WriteBuf<&'static [u8]>,
);

pin_project! {
    /// Future for opening a bidi stream
    pub struct OpenBi<'a, C:quic::Connection<B>, B:Buf> {
        opener: &'a Mutex<C::OpenStreams>,
        stream: Option<PendingStreams<C,B>>,
        session_id: SessionId,
    }
}

impl<'a, B, C> Future for OpenBi<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
    C::BidiStream: SendStreamUnframed<B>,
{
    type Output = Result<BidiStream<C::BidiStream, B>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut p = self.project();
        loop {
            match &mut p.stream {
                Some((stream, buf)) => {
                    while buf.has_remaining() {
                        ready!(stream.poll_send(cx, buf))?;
                    }

                    let (stream, _) = p.stream.take().unwrap();
                    return Poll::Ready(Ok(stream));
                }
                None => {
                    let mut opener = (*p.opener).lock().unwrap();
                    // Open the stream first
                    let res = ready!(opener.poll_open_bidi(cx))?;
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
    }
}

impl<'a, C, B> Future for OpenUni<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
    C::SendStream: SendStreamUnframed<B>,
{
    type Output = Result<SendStream<C::SendStream, B>, Error>;

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
                    let mut opener = (*p.opener).lock().unwrap();
                    let send = ready!(opener.poll_open_send(cx))?;
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
pub enum AcceptedBi<C: quic::Connection<B>, B: Buf> {
    /// An incoming bidirectional stream
    BidiStream(SessionId, BidiStream<C::BidiStream, B>),
    /// An incoming HTTP/3 request, passed through a webtransport session.
    ///
    /// This makes it possible to respond to multiple CONNECT requests
    Request(Request<()>, RequestStream<C::BidiStream, B>),
}

/// Future for [`Connection::read_datagram`]
pub struct ReadDatagram<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    conn: &'a Mutex<Connection<C, B>>,
    _marker: PhantomData<B>,
}

impl<'a, C, B> Future for ReadDatagram<'a, C, B>
where
    C: quic::Connection<B> + RecvDatagramExt,
    B: Buf,
{
    type Output = Result<Option<(SessionId, C::Buf)>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut conn = self.conn.lock().unwrap();
        match ready!(conn.inner.conn.poll_accept_datagram(cx))? {
            Some(v) => {
                let datagram = Datagram::decode(v)?;
                Poll::Ready(Ok(Some((
                    datagram.stream_id().into(),
                    datagram.into_payload(),
                ))))
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
}

impl<'a, C, B> Future for AcceptUni<'a, C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    type Output = Result<Option<(SessionId, RecvStream<C::RecvStream, B>)>, Error>;

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
