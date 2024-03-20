//! HTTP/3 server connection
//!
//! The [`Connection`] struct manages a connection from the side of the HTTP/3 server

use std::{
    collections::HashSet,
    marker::PhantomData,
    option::Option,
    result::Result,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::Buf;
use futures_util::{
    future::{self},
    ready,
};
use http::Request;
use quic::RecvStream;
use quic::StreamId;
use tokio::sync::mpsc;

use crate::{
    connection::{self, ConnectionInner, ConnectionState, SharedStateRef},
    error::{Code, Error, ErrorLevel},
    ext::Datagram,
    frame::{FrameStream, FrameStreamError},
    proto::{
        frame::{Frame, PayloadLen},
        push::PushId,
    },
    qpack,
    quic::{self, RecvDatagramExt, SendDatagramExt, SendStream as _},
    stream::BufRecvStream,
};

use crate::server::request::ResolveRequest;

use tracing::{trace, warn};

use super::stream::{ReadDatagram, RequestStream};

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
    pub(super) max_field_section_size: u64,
    // List of all incoming streams that are currently running.
    pub(super) ongoing_streams: HashSet<StreamId>,
    // Let the streams tell us when they are no longer running.
    pub(super) request_end_recv: mpsc::UnboundedReceiver<StreamId>,
    pub(super) request_end_send: mpsc::UnboundedSender<StreamId>,
    // Has a GOAWAY frame been sent? If so, this StreamId is the last we are willing to accept.
    pub(super) sent_closing: Option<StreamId>,
    // Has a GOAWAY frame been received? If so, this is PushId the last the remote will accept.
    pub(super) recv_closing: Option<PushId>,
    // The id of the last stream received by this connection.
    pub(super) last_accepted_stream: Option<StreamId>,
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
    /// Use a custom [`super::builder::Builder`] with [`super::builder::builder()`] to create a connection
    /// with different settings.
    /// Provide a Connection which implements [`quic::Connection`].
    pub async fn new(conn: C) -> Result<Self, Error> {
        super::builder::builder().build(conn).await
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

pub(super) struct RequestEnd {
    pub(super) request_end: mpsc::UnboundedSender<StreamId>,
    pub(super) stream_id: StreamId,
}
