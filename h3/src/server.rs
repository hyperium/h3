//! This module provides methods to create a http/3 Server.
//!
//! It allows to accept incoming requests, and send responses.
//!
//! # Examples
//!
//! ## Simple example
//! ```rust
//! async fn doc<C>(conn: C)
//! where
//! C: h3::quic::Connection<bytes::Bytes>,
//! <C as h3::quic::Connection<bytes::Bytes>>::BidiStream: Send + 'static
//! {
//!     let mut server_builder = h3::server::builder();
//!     // Build the Connection
//!     let mut h3_conn = server_builder.build(conn).await.unwrap();
//!     loop {
//!         // Accept incoming requests
//!         match h3_conn.accept().await {
//!             Ok(Some((req, mut stream))) => {
//!                 // spawn a new task to handle the request
//!                 tokio::spawn(async move {
//!                     // build a http response
//!                     let response = http::Response::builder().status(http::StatusCode::OK).body(()).unwrap();
//!                     // send the response to the wire
//!                     stream.send_response(response).await.unwrap();
//!                     // send some date
//!                     stream.send_data(bytes::Bytes::from("test")).await.unwrap();
//!                     // finnish the stream
//!                     stream.finish().await.unwrap();
//!                 });
//!             }
//!             Ok(None) => {
//!                 // break if no Request is accepted
//!                 break;
//!             }
//!             Err(err) => {
//!                 match err.get_error_level() {
//!                     // break on connection errors
//!                     h3::error::ErrorLevel::ConnectionError => break,
//!                     // continue on stream errors
//!                     h3::error::ErrorLevel::StreamError => continue,
//!                 }
//!             }
//!         }
//!     }
//! }
//! ```
//!
//! ## File server
//! A ready-to-use example of a file server is available [here](https://github.com/hyperium/h3/blob/master/examples/client.rs)

use std::{
    collections::HashSet,
    marker::PhantomData,
    option::Option,
    result::Result,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::{Buf, BytesMut};
use futures_util::{
    future::{self, Future},
    ready,
};
use http::{response, HeaderMap, Request, Response};
use pin_project_lite::pin_project;
use quic::RecvStream;
use quic::StreamId;
use tokio::sync::mpsc;

use crate::{
    config::Config,
    connection::{self, ConnectionInner, ConnectionState, SharedStateRef},
    error::{Code, Error, ErrorLevel},
    ext::Datagram,
    frame::{FrameStream, FrameStreamError},
    proto::{
        frame::{Frame, PayloadLen},
        headers::Header,
        push::PushId,
    },
    qpack,
    quic::{self, RecvDatagramExt, SendDatagramExt, SendStream as _},
    request::ResolveRequest,
    stream::{self, BufRecvStream},
};
use tracing::{error, trace, warn};

/// Create a builder of HTTP/3 server connections
///
/// This function creates a [`Builder`] that carries settings that can
/// be shared between server connections.
pub fn builder() -> Builder {
    Builder::new()
}

/// Server connection driver
///
/// The [`Connection`] struct manages a connection from the side of the HTTP/3 server
///
/// Create a new Instance with [`Connection::new()`].
/// Accept incoming requests with [`Connection::accept()`].
/// And shutdown a connection with [`Connection::shutdown()`].
pub struct Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// TODO: temporarily break encapsulation for `WebTransportSession`
    pub inner: ConnectionInner<C, B>,
    max_field_section_size: u64,
    // List of all incoming streams that are currently running.
    ongoing_streams: HashSet<StreamId>,
    // Let the streams tell us when they are no longer running.
    request_end_recv: mpsc::UnboundedReceiver<StreamId>,
    request_end_send: mpsc::UnboundedSender<StreamId>,
    // Has a GOAWAY frame been sent? If so, this StreamId is the last we are willing to accept.
    sent_closing: Option<StreamId>,
    // Has a GOAWAY frame been received? If so, this is PushId the last the remote will accept.
    recv_closing: Option<PushId>,
    // The id of the last stream received by this connection.
    last_accepted_stream: Option<StreamId>,
}

impl<C, B> ConnectionState for Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    fn shared_state(&self) -> &SharedStateRef {
        &self.inner.shared
    }
}

impl<C, B> Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// Create a new HTTP/3 server connection with default settings
    ///
    /// Use a custom [`Builder`] with [`builder()`] to create a connection
    /// with different settings.
    /// Provide a Connection which implements [`quic::Connection`].
    pub async fn new(conn: C) -> Result<Self, Error> {
        builder().build(conn).await
    }

    /// Closes the connection with a code and a reason.
    pub fn close<T: AsRef<str>>(&mut self, code: Code, reason: T) -> Error {
        self.inner.close(code, reason)
    }
}

impl<C, B> Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// Accept an incoming request.
    ///
    /// It returns a tuple with a [`http::Request`] and an [`RequestStream`].
    /// The [`http::Request`] is the received request from the client.
    /// The [`RequestStream`] can be used to send the response.
    pub async fn accept(
        &mut self,
    ) -> Result<Option<(Request<()>, RequestStream<C::BidiStream, B>)>, Error> {
        // Accept the incoming stream
        let mut stream = match future::poll_fn(|cx| self.poll_accept_request(cx)).await {
            Ok(Some(s)) => FrameStream::new(BufRecvStream::new(s)),
            Ok(None) => {
                // We always send a last GoAway frame to the client, so it knows which was the last
                // non-rejected request.
                self.shutdown(0).await?;
                return Ok(None);
            }
            Err(err) => {
                match err.inner.kind {
                    crate::error::Kind::Closed => return Ok(None),
                    crate::error::Kind::Application {
                        code,
                        reason,
                        level: ErrorLevel::ConnectionError,
                    } => {
                        return Err(self.inner.close(
                            code,
                            reason.unwrap_or_else(|| String::into_boxed_str(String::from(""))),
                        ))
                    }
                    _ => return Err(err),
                };
            }
        };

        let frame = future::poll_fn(|cx| stream.poll_next(cx)).await;
        let req = self.accept_with_frame(stream, frame)?;
        if let Some(req) = req {
            Ok(Some(req.resolve().await?))
        } else {
            Ok(None)
        }
    }

    /// Accepts an http request where the first frame has already been read and decoded.
    ///
    ///
    /// This is needed as a bidirectional stream may be read as part of incoming webtransport
    /// bi-streams. If it turns out that the stream is *not* a `WEBTRANSPORT_STREAM` the request
    /// may still want to be handled and passed to the user.
    pub fn accept_with_frame(
        &mut self,
        mut stream: FrameStream<C::BidiStream, B>,
        frame: Result<Option<Frame<PayloadLen>>, FrameStreamError>,
    ) -> Result<Option<ResolveRequest<C, B>>, Error> {
        let mut encoded = match frame {
            Ok(Some(Frame::Headers(h))) => h,

            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
            //# If a client-initiated
            //# stream terminates without enough of the HTTP message to provide a
            //# complete response, the server SHOULD abort its response stream with
            //# the error code H3_REQUEST_INCOMPLETE.
            Ok(None) => {
                return Err(self.inner.close(
                    Code::H3_REQUEST_INCOMPLETE,
                    "request stream closed before headers",
                ));
            }

            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
            //# Receipt of an invalid sequence of frames MUST be treated as a
            //# connection error of type H3_FRAME_UNEXPECTED.

            //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.5
            //# A server MUST treat the
            //# receipt of a PUSH_PROMISE frame as a connection error of type
            //# H3_FRAME_UNEXPECTED.
            Ok(Some(_)) => {
                //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
                //# Receipt of an invalid sequence of frames MUST be treated as a
                //# connection error of type H3_FRAME_UNEXPECTED.
                // Close if the first frame is not a header frame
                return Err(self.inner.close(
                    Code::H3_FRAME_UNEXPECTED,
                    "first request frame is not headers",
                ));
            }
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
                        return Err(self.inner.close(
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
        };

        let mut request_stream = RequestStream {
            request_end: Arc::new(RequestEnd {
                request_end: self.request_end_send.clone(),
                stream_id: stream.send_id(),
            }),
            inner: connection::RequestStream::new(
                stream,
                self.max_field_section_size,
                self.inner.shared.clone(),
                self.inner.send_grease_frame,
            ),
        };

        let decoded = match qpack::decode_stateless(&mut encoded, self.max_field_section_size) {
            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
            //# An HTTP/3 implementation MAY impose a limit on the maximum size of
            //# the message header it will accept on an individual HTTP message.
            Err(qpack::DecoderError::HeaderTooLong(cancel_size)) => Err(cancel_size),
            Ok(decoded) => {
                // send the grease frame only once
                self.inner.send_grease_frame = false;
                Ok(decoded)
            }
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
                        return Err(self.inner.close(
                            code,
                            reason.unwrap_or_else(|| String::into_boxed_str(String::from(""))),
                        ))
                    }
                    crate::error::Kind::Application {
                        code,
                        reason: _,
                        level: ErrorLevel::StreamError,
                    } => {
                        request_stream.stop_stream(code);
                        return Err(err);
                    }
                    _ => return Err(err),
                };
            }
        };

        Ok(Some(ResolveRequest::new(
            request_stream,
            decoded,
            self.max_field_section_size,
        )))
    }

    /// Initiate a graceful shutdown, accepting `max_request` potentially still in-flight
    ///
    /// See [connection shutdown](https://www.rfc-editor.org/rfc/rfc9114.html#connection-shutdown) for more information.
    pub async fn shutdown(&mut self, max_requests: usize) -> Result<(), Error> {
        let max_id = self
            .last_accepted_stream
            .map(|id| id + max_requests)
            .unwrap_or(StreamId::FIRST_REQUEST);

        self.inner.shutdown(&mut self.sent_closing, max_id).await
    }

    /// Accepts an incoming bidirectional stream.
    ///
    /// This could be either a *Request* or a *WebTransportBiStream*, the first frame's type
    /// decides.
    pub fn poll_accept_request(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<C::BidiStream>, Error>> {
        let _ = self.poll_control(cx)?;
        let _ = self.poll_requests_completion(cx);
        loop {
            match self.inner.poll_accept_request(cx) {
                Poll::Ready(Err(x)) => break Poll::Ready(Err(x)),
                Poll::Ready(Ok(None)) => {
                    if self.poll_requests_completion(cx).is_ready() {
                        break Poll::Ready(Ok(None));
                    } else {
                        // Wait for all the requests to be finished, request_end_recv will wake
                        // us on each request completion.
                        break Poll::Pending;
                    }
                }
                Poll::Pending => {
                    if self.recv_closing.is_some() && self.poll_requests_completion(cx).is_ready() {
                        // The connection is now idle.
                        break Poll::Ready(Ok(None));
                    } else {
                        return Poll::Pending;
                    }
                }
                Poll::Ready(Ok(Some(mut s))) => {
                    // When the connection is in a graceful shutdown procedure, reject all
                    // incoming requests not belonging to the grace interval. It's possible that
                    // some acceptable request streams arrive after rejected requests.
                    if let Some(max_id) = self.sent_closing {
                        if s.send_id() > max_id {
                            s.stop_sending(Code::H3_REQUEST_REJECTED.value());
                            s.reset(Code::H3_REQUEST_REJECTED.value());
                            if self.poll_requests_completion(cx).is_ready() {
                                break Poll::Ready(Ok(None));
                            }
                            continue;
                        }
                    }
                    self.last_accepted_stream = Some(s.send_id());
                    self.ongoing_streams.insert(s.send_id());
                    break Poll::Ready(Ok(Some(s)));
                }
            };
        }
    }

    pub(crate) fn poll_control(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        while (self.poll_next_control(cx)?).is_ready() {}
        Poll::Pending
    }

    pub(crate) fn poll_next_control(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Frame<PayloadLen>, Error>> {
        let frame = ready!(self.inner.poll_control(cx))?;

        match &frame {
            Frame::Settings(w) => trace!("Got settings > {:?}", w),
            &Frame::Goaway(id) => self.inner.process_goaway(&mut self.recv_closing, id)?,
            f @ Frame::MaxPushId(_) | f @ Frame::CancelPush(_) => {
                warn!("Control frame ignored {:?}", f);

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.3
                //= type=TODO
                //# If a server receives a CANCEL_PUSH frame for a push
                //# ID that has not yet been mentioned by a PUSH_PROMISE frame, this MUST
                //# be treated as a connection error of type H3_ID_ERROR.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.7
                //= type=TODO
                //# A MAX_PUSH_ID frame cannot reduce the maximum push
                //# ID; receipt of a MAX_PUSH_ID frame that contains a smaller value than
                //# previously received MUST be treated as a connection error of type
                //# H3_ID_ERROR.
            }

            //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.5
            //# A server MUST treat the
            //# receipt of a PUSH_PROMISE frame as a connection error of type
            //# H3_FRAME_UNEXPECTED.
            frame => {
                return Poll::Ready(Err(Code::H3_FRAME_UNEXPECTED.with_reason(
                    format!("on server control stream: {:?}", frame),
                    ErrorLevel::ConnectionError,
                )))
            }
        }
        Poll::Ready(Ok(frame))
    }

    fn poll_requests_completion(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        loop {
            match self.request_end_recv.poll_recv(cx) {
                // The channel is closed
                Poll::Ready(None) => return Poll::Ready(()),
                // A request has completed
                Poll::Ready(Some(id)) => {
                    self.ongoing_streams.remove(&id);
                }
                Poll::Pending => {
                    if self.ongoing_streams.is_empty() {
                        // Tell the caller there is not more ongoing requests.
                        // Still, the completion of future requests will wake us.
                        return Poll::Ready(());
                    } else {
                        return Poll::Pending;
                    }
                }
            }
        }
    }
}

impl<C, B> Connection<C, B>
where
    C: quic::Connection<B> + SendDatagramExt<B>,
    B: Buf,
{
    /// Sends a datagram
    pub fn send_datagram(&mut self, stream_id: StreamId, data: B) -> Result<(), Error> {
        self.inner
            .conn
            .send_datagram(Datagram::new(stream_id, data))?;
        tracing::info!("Sent datagram");

        Ok(())
    }
}

impl<C, B> Connection<C, B>
where
    C: quic::Connection<B> + RecvDatagramExt,
    B: Buf,
{
    /// Reads an incoming datagram
    pub fn read_datagram(&mut self) -> ReadDatagram<C, B> {
        ReadDatagram {
            conn: self,
            _marker: PhantomData,
        }
    }
}

impl<C, B> Drop for Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    fn drop(&mut self) {
        self.inner.close(Code::H3_NO_ERROR, "");
    }
}

//= https://www.rfc-editor.org/rfc/rfc9114#section-6.1
//= type=TODO
//# In order to
//# permit these streams to open, an HTTP/3 server SHOULD configure non-
//# zero minimum values for the number of permitted streams and the
//# initial stream flow-control window.

//= https://www.rfc-editor.org/rfc/rfc9114#section-6.1
//= type=TODO
//# So as to not unnecessarily limit
//# parallelism, at least 100 request streams SHOULD be permitted at a
//# time.

/// Builder of HTTP/3 server connections.
///
/// Use this struct to create a new [`Connection`].
/// Settings for the [`Connection`] can be provided here.
///
/// # Example
///
/// ```rust
/// fn doc<C,B>(conn: C)
/// where
/// C: h3::quic::Connection<B>,
/// B: bytes::Buf,
/// {
///     let mut server_builder = h3::server::builder();
///     // Set the maximum header size
///     server_builder.max_field_section_size(1000);
///     // do not send grease types
///     server_builder.send_grease(false);
///     // Build the Connection
///     let mut h3_conn = server_builder.build(conn);
/// }
/// ```
pub struct Builder {
    pub(crate) config: Config,
}

impl Builder {
    /// Creates a new [`Builder`] with default settings.
    pub(super) fn new() -> Self {
        Builder {
            config: Default::default(),
        }
    }

    #[cfg(test)]
    pub fn send_settings(&mut self, value: bool) -> &mut Self {
        self.config.send_settings = value;
        self
    }

    /// Set the maximum header size this client is willing to accept
    ///
    /// See [header size constraints] section of the specification for details.
    ///
    /// [header size constraints]: https://www.rfc-editor.org/rfc/rfc9114.html#name-header-size-constraints
    pub fn max_field_section_size(&mut self, value: u64) -> &mut Self {
        self.config.settings.max_field_section_size = value;
        self
    }

    /// Send grease values to the Client.
    /// See [setting](https://www.rfc-editor.org/rfc/rfc9114.html#settings-parameters), [frame](https://www.rfc-editor.org/rfc/rfc9114.html#frame-reserved) and [stream](https://www.rfc-editor.org/rfc/rfc9114.html#stream-grease) for more information.
    #[inline]
    pub fn send_grease(&mut self, value: bool) -> &mut Self {
        self.config.send_grease = value;
        self
    }

    /// Indicates to the peer that WebTransport is supported.
    ///
    /// See: [establishing a webtransport session](https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/#section-3.1)
    ///
    ///
    /// **Server**:
    /// Supporting for webtransport also requires setting `enable_connect` `enable_datagram`
    /// and `max_webtransport_sessions`.
    #[inline]
    pub fn enable_webtransport(&mut self, value: bool) -> &mut Self {
        self.config.settings.enable_webtransport = value;
        self
    }

    /// Enables the CONNECT protocol
    pub fn enable_connect(&mut self, value: bool) -> &mut Self {
        self.config.settings.enable_extended_connect = value;
        self
    }

    /// Limits the maximum number of WebTransport sessions
    pub fn max_webtransport_sessions(&mut self, value: u64) -> &mut Self {
        self.config.settings.max_webtransport_sessions = value;
        self
    }

    /// Indicates that the client or server supports HTTP/3 datagrams
    ///
    /// See: <https://www.rfc-editor.org/rfc/rfc9297#section-2.1.1>
    pub fn enable_datagram(&mut self, value: bool) -> &mut Self {
        self.config.settings.enable_datagram = value;
        self
    }
}

impl Builder {
    /// Build an HTTP/3 connection from a QUIC connection
    ///
    /// This method creates a [`Connection`] instance with the settings in the [`Builder`].
    pub async fn build<C, B>(&self, conn: C) -> Result<Connection<C, B>, Error>
    where
        C: quic::Connection<B>,
        B: Buf,
    {
        let (sender, receiver) = mpsc::unbounded_channel();
        Ok(Connection {
            inner: ConnectionInner::new(conn, SharedStateRef::default(), self.config).await?,
            max_field_section_size: self.config.settings.max_field_section_size,
            request_end_send: sender,
            request_end_recv: receiver,
            ongoing_streams: HashSet::new(),
            sent_closing: None,
            recv_closing: None,
            last_accepted_stream: None,
        })
    }
}

struct RequestEnd {
    request_end: mpsc::UnboundedSender<StreamId>,
    stream_id: StreamId,
}

/// Manage request and response transfer for an incoming request
///
/// The [`RequestStream`] struct is used to send and/or receive
/// information from the client.
pub struct RequestStream<S, B> {
    inner: connection::RequestStream<S, B>,
    request_end: Arc<RequestEnd>,
}

impl<S, B> AsMut<connection::RequestStream<S, B>> for RequestStream<S, B> {
    fn as_mut(&mut self) -> &mut connection::RequestStream<S, B> {
        &mut self.inner
    }
}

impl<S, B> ConnectionState for RequestStream<S, B> {
    fn shared_state(&self) -> &SharedStateRef {
        &self.inner.conn_state
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::RecvStream,
    B: Buf,
{
    /// Receive data sent from the client
    pub async fn recv_data(&mut self) -> Result<Option<impl Buf>, Error> {
        self.inner.recv_data().await
    }

    /// Poll for data sent from the client
    pub fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<impl Buf>, Error>> {
        self.inner.poll_data(cx)
    }

    /// Receive an optional set of trailers for the request
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        self.inner.recv_trailers().await
    }

    /// Tell the peer to stop sending into the underlying QUIC stream
    pub fn stop_sending(&mut self, error_code: crate::error::Code) {
        self.inner.stream.stop_sending(error_code)
    }

    /// Returns the underlying stream id
    pub fn id(&self) -> StreamId {
        self.inner.stream.id()
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::SendStream<B>,
    B: Buf,
{
    /// Send the HTTP/3 response
    ///
    /// This should be called before trying to send any data with
    /// [`RequestStream::send_data`].
    pub async fn send_response(&mut self, resp: Response<()>) -> Result<(), Error> {
        let (parts, _) = resp.into_parts();
        let response::Parts {
            status, headers, ..
        } = parts;
        let headers = Header::response(status, headers);

        let mut block = BytesMut::new();
        let mem_size = qpack::encode_stateless(&mut block, headers)?;

        let max_mem_size = self
            .inner
            .conn_state
            .read("send_response")
            .peer_config
            .max_field_section_size;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
        //# An implementation that
        //# has received this parameter SHOULD NOT send an HTTP message header
        //# that exceeds the indicated size, as the peer will likely refuse to
        //# process it.
        if mem_size > max_mem_size {
            return Err(Error::header_too_big(mem_size, max_mem_size));
        }

        stream::write(&mut self.inner.stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        Ok(())
    }

    /// Send some data on the response body.
    pub async fn send_data(&mut self, buf: B) -> Result<(), Error> {
        self.inner.send_data(buf).await
    }

    /// Stop a stream with an error code
    ///
    /// The code can be [`Code::H3_NO_ERROR`].
    pub fn stop_stream(&mut self, error_code: Code) {
        self.inner.stop_stream(error_code);
    }

    /// Send a set of trailers to end the response.
    ///
    /// Either [`RequestStream::finish`] or
    /// [`RequestStream::send_trailers`] must be called to finalize a
    /// request.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        self.inner.send_trailers(trailers).await
    }

    /// End the response without trailers.
    ///
    /// Either [`RequestStream::finish`] or
    /// [`RequestStream::send_trailers`] must be called to finalize a
    /// request.
    pub async fn finish(&mut self) -> Result<(), Error> {
        self.inner.finish().await
    }

    //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1.1
    //= type=TODO
    //# Implementations SHOULD cancel requests by abruptly terminating any
    //# directions of a stream that are still open.  To do so, an
    //# implementation resets the sending parts of streams and aborts reading
    //# on the receiving parts of streams; see Section 2.4 of
    //# [QUIC-TRANSPORT].

    /// Returns the underlying stream id
    pub fn send_id(&self) -> StreamId {
        self.inner.stream.send_id()
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::BidiStream<B>,
    B: Buf,
{
    /// Splits the Request-Stream into send and receive.
    /// This can be used the send and receive data on different tasks.
    pub fn split(
        self,
    ) -> (
        RequestStream<S::SendStream, B>,
        RequestStream<S::RecvStream, B>,
    ) {
        let (send, recv) = self.inner.split();
        (
            RequestStream {
                inner: send,
                request_end: self.request_end.clone(),
            },
            RequestStream {
                inner: recv,
                request_end: self.request_end,
            },
        )
    }
}

impl Drop for RequestEnd {
    fn drop(&mut self) {
        if let Err(e) = self.request_end.send(self.stream_id) {
            error!(
                "failed to notify connection of request end: {} {}",
                self.stream_id, e
            );
        }
    }
}

pin_project! {
    /// Future for [`Connection::read_datagram`]
    pub struct ReadDatagram<'a, C, B>
    where
            C: quic::Connection<B>,
            B: Buf,
        {
            conn: &'a mut Connection<C, B>,
            _marker: PhantomData<B>,
        }
}

impl<'a, C, B> Future for ReadDatagram<'a, C, B>
where
    C: quic::Connection<B> + RecvDatagramExt,
    B: Buf,
{
    type Output = Result<Option<Datagram<C::Buf>>, Error>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        tracing::trace!("poll: read_datagram");
        match ready!(self.conn.inner.conn.poll_accept_datagram(cx))? {
            Some(v) => Poll::Ready(Ok(Some(Datagram::decode(v)?))),
            None => Poll::Ready(Ok(None)),
        }
    }
}
