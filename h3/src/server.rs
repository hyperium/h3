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
//!     // Accept incoming requests
//!     while let Some((req, mut stream)) = h3_conn.accept().await.unwrap() {
//!         // spawn a new task to handle the request
//!         tokio::spawn(async move {
//!             // build a http response
//!             let response = http::Response::builder().status(http::StatusCode::OK).body(()).unwrap();
//!             // send the response to the wire
//!             stream.send_response(response).await.unwrap();
//!             // send some date
//!             stream.send_data(bytes::Bytes::from("test")).await.unwrap();
//!             // finnish the stream
//!             stream.finish().await.unwrap();
//!         });
//!     };
//! }
//! ```
//!
//! ## File server
//! A ready-to-use example of a file server is available [here](https://github.com/hyperium/h3/blob/master/examples/client.rs)

use std::{
    collections::HashSet,
    convert::TryFrom,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::{Buf, BytesMut};
use futures_util::future;
use http::{response, HeaderMap, Request, Response, StatusCode};
use quic::StreamId;
use tokio::sync::mpsc;

use crate::{
    connection::{self, ConnectionInner, ConnectionState, SharedStateRef},
    error::{Code, Error},
    frame::FrameStream,
    proto::{frame::Frame, headers::Header, varint::VarInt},
    qpack,
    quic::{self, RecvStream as _, SendStream as _},
    stream,
};
use tracing::{error, trace, warn};

/// This function creates a [`Builder`] for the Server.
pub fn builder() -> Builder {
    Builder::new()
}

/// The [`Connection`] struct manages a connection from the http/3 Server.
///
/// Create a new Instance with [`Connection::new()`].
/// Accept incoming requests with [`Connection::accept()`].
/// And shutdown a connection with [`Connection::shutdown()`].
pub struct Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    inner: ConnectionInner<C, B>,
    max_field_section_size: u64,
    // List of all incoming streams that are currently running.
    ongoing_streams: HashSet<StreamId>,
    // Let the streams tell us when they are no longer running.
    request_end_recv: mpsc::UnboundedReceiver<StreamId>,
    request_end_send: mpsc::UnboundedSender<StreamId>,
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
    /// This method creates a new Connection for Servers with default settings.
    /// Use [`builder()`] to create a connection with different settings.
    /// Provide a Connection which implements [`quic::Connection`].
    pub async fn new(conn: C) -> Result<Self, Error> {
        Ok(builder().build(conn).await?)
    }
}

impl<C, B> Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// This method accepts a http request from a Client.
    /// It returns a tuple with a [`http::Request`] and an [`RequestStream`].
    /// The [`http::Request`] is the received request from the client.
    /// The [`RequestStream`] can be used to send the response.
    pub async fn accept(
        &mut self,
    ) -> Result<Option<(Request<()>, RequestStream<C::BidiStream, B>)>, Error> {
        let mut stream = match future::poll_fn(|cx| self.poll_accept_request(cx)).await {
            Ok(Some(s)) => FrameStream::new(s),
            Ok(None) => {
                // We always send a last GoAway frame to the client, so it knows which was the last
                // non-rejected request.
                self.inner.shutdown(0).await?;
                return Ok(None);
            }
            Err(e) => {
                if e.is_closed() {
                    return Ok(None);
                }
                return Err(e);
            }
        };

        let frame = future::poll_fn(|cx| stream.poll_next(cx)).await;

        let mut encoded = match frame {
            Ok(Some(Frame::Headers(h))) => h,

            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
            //# If a client-initiated
            //# stream terminates without enough of the HTTP message to provide a
            //# complete response, the server SHOULD abort its response stream with
            //# the error code H3_REQUEST_INCOMPLETE.
            Ok(None) => {
                return Err(
                    Code::H3_REQUEST_INCOMPLETE.with_reason("request stream closed before headers")
                )
            }

            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
            //# Receipt of an invalid sequence of frames MUST be treated as a
            //# connection error of type H3_FRAME_UNEXPECTED.

            //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.5
            //# A server MUST treat the
            //# receipt of a PUSH_PROMISE frame as a connection error of type
            //# H3_FRAME_UNEXPECTED.
            Ok(Some(_)) => {
                return Err(
                    Code::H3_FRAME_UNEXPECTED.with_reason("first request frame is not headers")
                )
            }
            Err(e) => {
                let err: Error = e.into();
                if err.is_closed() {
                    return Ok(None);
                }
                return Err(err);
            }
        };

        let mut request_stream = RequestStream {
            request_end: Arc::new(RequestEnd {
                request_end: self.request_end_send.clone(),
                stream_id: stream.id(),
            }),
            inner: connection::RequestStream::new(
                stream,
                self.max_field_section_size,
                self.inner.shared.clone(),
                self.inner.send_grease_frame,
            ),
        };

        let qpack::Decoded { fields, .. } =
            match qpack::decode_stateless(&mut encoded, self.max_field_section_size) {
                //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
                //# An HTTP/3 implementation MAY impose a limit on the maximum size of
                //# the message header it will accept on an individual HTTP message.
                Err(qpack::DecoderError::HeaderTooLong(cancel_size)) => {
                    request_stream
                        .send_response(
                            http::Response::builder()
                                .status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
                                .body(())
                                .expect("header too big response"),
                        )
                        .await?;
                    return Err(Error::header_too_big(
                        cancel_size,
                        self.max_field_section_size,
                    ));
                }
                Ok(decoded) => decoded,
                Err(e) => return Err(e.into()),
            };

        let (method, uri, headers) = Header::try_from(fields)?.into_request_parts()?;

        let mut req = http::Request::new(());
        *req.method_mut() = method;
        *req.uri_mut() = uri;
        *req.headers_mut() = headers;
        *req.version_mut() = http::Version::HTTP_3;
        // send the grease frame only once
        self.inner.send_grease_frame = false;

        Ok(Some((req, request_stream)))
    }

    /// This method stops the connection gracefully.
    /// See [Connection-Shutdown](https://httpwg.org/specs/rfc9114.html#connection-shutdown) for more information.
    pub async fn shutdown(&mut self, max_requests: usize) -> Result<(), Error> {
        self.inner.shutdown(max_requests).await
    }

    fn poll_accept_request(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<C::BidiStream>, Error>> {
        let _ = self.poll_control(cx)?;
        let _ = self.poll_requests_completion(cx);

        let closing = self.shared_state().read("server accept").closing;

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
                    if closing.is_some() && self.poll_requests_completion(cx).is_ready() {
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
                    if let Some(max_id) = closing {
                        if s.id() > max_id {
                            s.stop_sending(Code::H3_REQUEST_REJECTED.value());
                            s.reset(Code::H3_REQUEST_REJECTED.value());
                            if self.poll_requests_completion(cx).is_ready() {
                                break Poll::Ready(Ok(None));
                            }
                            continue;
                        }
                    }
                    self.inner.start_stream(s.id());
                    self.ongoing_streams.insert(s.id());
                    break Poll::Ready(Ok(Some(s)));
                }
            };
        }
    }

    fn poll_control(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        while let Poll::Ready(frame) = self.inner.poll_control(cx)? {
            match frame {
                Frame::Settings(_) => trace!("Got settings"),
                Frame::Goaway(id) => {
                    if !id.is_push() {
                        return Poll::Ready(Err(Code::H3_ID_ERROR
                            .with_reason(format!("non-push StreamId in a GoAway frame: {}", id))));
                    }
                }
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
                    return Poll::Ready(Err(Code::H3_FRAME_UNEXPECTED
                        .with_reason(format!("on server control stream: {:?}", frame))))
                }
            }
        }
        Poll::Pending
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

/// Use this struct to create a new [`Connection`].
/// All the settings for the [`Connection`] can be provided here.
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
    pub(super) max_field_section_size: u64,
    pub(super) send_grease: bool,
}

impl Builder {
    /// Creates a new [`Builder`] with default settings.
    pub(super) fn new() -> Self {
        Builder {
            max_field_section_size: VarInt::MAX.0,
            send_grease: true,
        }
    }

    /// Set the `max_field_section_size` for the [`Builder`].
    /// See [Header size](https://httpwg.org/specs/rfc9114.html#header-size-constraints) for more information.
    pub fn max_field_section_size(&mut self, value: u64) -> &mut Self {
        self.max_field_section_size = value;
        self
    }

    /// Send grease values to the Client.
    /// See [setting](https://httpwg.org/specs/rfc9114.html#settings-parameters), [frame](https://httpwg.org/specs/rfc9114.html#frame-reserved) and [stream](https://httpwg.org/specs/rfc9114.html#stream-grease) for more information.
    pub fn send_grease(&mut self, value: bool) -> &mut Self {
        self.send_grease = value;
        self
    }
}

impl Builder {
    /// This method creates a [`Connection`] instance with the settings in the [`Builder`].
    pub async fn build<C, B>(&self, conn: C) -> Result<Connection<C, B>, Error>
    where
        C: quic::Connection<B>,
        B: Buf,
    {
        let (sender, receiver) = mpsc::unbounded_channel();
        Ok(Connection {
            inner: ConnectionInner::new(
                conn,
                self.max_field_section_size,
                SharedStateRef::default(),
                self.send_grease,
            )
            .await?,
            max_field_section_size: self.max_field_section_size,
            request_end_send: sender,
            request_end_recv: receiver,
            ongoing_streams: HashSet::new(),
        })
    }
}

pub struct RequestEnd {
    request_end: mpsc::UnboundedSender<StreamId>,
    stream_id: StreamId,
}

/// The [`RequestStream`] struct is to send and/or receive information from the client.
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
{
    /// Receives data, sent from the Client.
    pub async fn recv_data(&mut self) -> Result<Option<impl Buf>, Error> {
        self.inner.recv_data().await
    }

    pub fn stop_sending(&mut self, error_code: crate::error::Code) {
        self.inner.stream.stop_sending(error_code)
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::SendStream<B>,
    B: Buf,
{
    /// This method sends a Http-Response to the Client.
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
            .peer_max_field_section_size;

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

    /// Send data to the Client.
    pub async fn send_data(&mut self, buf: B) -> Result<(), Error> {
        self.inner.send_data(buf).await
    }

    /// Send the Http-Trailers.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        self.inner.send_trailers(trailers).await
    }

    // Closes the Stream when all data is sent.
    pub async fn finish(&mut self) -> Result<(), Error> {
        self.inner.finish().await
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::RecvStream + quic::SendStream<B>,
    B: Buf,
{
    /// Receives Http-Trailers from the Client.
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        let res = self.inner.recv_trailers().await;
        if let Err(ref e) = res {
            if e.is_header_too_big() {
                self.send_response(
                    http::Response::builder()
                        .status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
                        .body(())
                        .expect("header too big response"),
                )
                .await?;
            }
        }
        res
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
