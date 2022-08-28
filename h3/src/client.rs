use std::{
    convert::TryFrom,
    marker::PhantomData,
    sync::{atomic::AtomicUsize, Arc},
    task::{Context, Poll, Waker},
};

use bytes::{Buf, Bytes, BytesMut};
use futures_util::future;
use http::{request, HeaderMap, Response};
use tracing::{info, trace};

use crate::{
    connection::{self, ConnectionInner, ConnectionState, SharedStateRef},
    error::{Code, Error},
    frame::FrameStream,
    proto::{frame::Frame, headers::Header, varint::VarInt},
    qpack, quic, stream,
};

pub fn builder() -> Builder {
    Builder::new()
}

pub async fn new<C, O>(conn: C) -> Result<(Connection<C, Bytes>, SendRequest<O, Bytes>), Error>
where
    C: quic::Connection<Bytes, OpenStreams = O>,
    O: quic::OpenStreams<Bytes>,
{
    //= https://www.rfc-editor.org/rfc/rfc9114#section-3.3
    //= type=implication
    //# Clients SHOULD NOT open more than one HTTP/3 connection to a given IP
    //# address and UDP port, where the IP address and port might be derived
    //# from a URI, a selected alternative service ([ALTSVC]), a configured
    //# proxy, or name resolution of any of these.
    Builder::new().build(conn).await
}

pub struct SendRequest<T, B>
where
    T: quic::OpenStreams<B>,
    B: Buf,
{
    open: T,
    conn_state: SharedStateRef,
    max_field_section_size: u64, // maximum size for a header we receive
    // counts instances of SendRequest to close the connection when the last is dropped.
    sender_count: Arc<AtomicUsize>,
    conn_waker: Option<Waker>,
    _buf: PhantomData<fn(B)>,
    send_grease_frame: bool,
}

impl<T, B> SendRequest<T, B>
where
    T: quic::OpenStreams<B>,
    B: Buf,
{
    pub async fn send_request(
        &mut self,
        req: http::Request<()>,
    ) -> Result<RequestStream<T::BidiStream, B>, Error> {
        let (peer_max_field_section_size, closing) = {
            let state = self.conn_state.read("send request lock state");
            (state.peer_max_field_section_size, state.closing)
        };

        if closing.is_some() {
            return Err(Error::closing());
        }

        let (parts, _) = req.into_parts();
        let request::Parts {
            method,
            uri,
            headers,
            ..
        } = parts;
        let headers = Header::request(method, uri, headers)?;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
        //= type=implication
        //# A
        //# client MUST send only a single request on a given stream.
        let mut stream = future::poll_fn(|cx| self.open.poll_open_bidi(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //= type=TODO
        //# Characters in field names MUST be
        //# converted to lowercase prior to their encoding.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.1
        //= type=TODO
        //# To allow for better compression efficiency, the Cookie header field
        //# ([COOKIES]) MAY be split into separate field lines, each with one or
        //# more cookie-pairs, before compression.

        let mut block = BytesMut::new();
        let mem_size = qpack::encode_stateless(&mut block, headers)?;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
        //# An implementation that
        //# has received this parameter SHOULD NOT send an HTTP message header
        //# that exceeds the indicated size, as the peer will likely refuse to
        //# process it.
        if mem_size > peer_max_field_section_size {
            return Err(Error::header_too_big(mem_size, peer_max_field_section_size));
        }

        stream::write(&mut stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        let request_stream = RequestStream {
            inner: connection::RequestStream::new(
                FrameStream::new(stream),
                self.max_field_section_size,
                self.conn_state.clone(),
                self.send_grease_frame,
            ),
        };
        // send the grease frame only once
        self.send_grease_frame = false;
        Ok(request_stream)
    }
}

impl<T, B> ConnectionState for SendRequest<T, B>
where
    T: quic::OpenStreams<B>,
    B: Buf,
{
    fn shared_state(&self) -> &SharedStateRef {
        &self.conn_state
    }
}

impl<T, B> Clone for SendRequest<T, B>
where
    T: quic::OpenStreams<B> + Clone,
    B: Buf,
{
    fn clone(&self) -> Self {
        self.sender_count
            .fetch_add(1, std::sync::atomic::Ordering::Release);

        Self {
            open: self.open.clone(),
            conn_state: self.conn_state.clone(),
            max_field_section_size: self.max_field_section_size,
            sender_count: self.sender_count.clone(),
            conn_waker: self.conn_waker.clone(),
            _buf: PhantomData,
            send_grease_frame: self.send_grease_frame,
        }
    }
}

impl<T, B> Drop for SendRequest<T, B>
where
    T: quic::OpenStreams<B>,
    B: Buf,
{
    fn drop(&mut self) {
        if self
            .sender_count
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
            == 1
        {
            if let Some(w) = self.conn_waker.take() {
                w.wake()
            }
            self.shared_state().write("SendRequest drop").error = Some(Error::closed());
            self.open.close(Code::H3_NO_ERROR, b"");
        }
    }
}

pub struct Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    inner: ConnectionInner<C, B>,
}

impl<C, B> Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    pub async fn shutdown(&mut self, max_requests: usize) -> Result<(), Error> {
        self.inner.shutdown(max_requests).await
    }

    pub async fn wait_idle(&mut self) -> Result<(), Error> {
        future::poll_fn(|cx| self.poll_close(cx)).await
    }

    pub fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        while let Poll::Ready(result) = self.inner.poll_control(cx) {
            match result {
                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.2
                //= type=TODO
                //# When a 0-RTT QUIC connection is being used, the initial value of each
                //# server setting is the value used in the previous session.  Clients
                //# SHOULD store the settings the server provided in the HTTP/3
                //# connection where resumption information was provided, but they MAY
                //# opt not to store settings in certain cases (e.g., if the session
                //# ticket is received before the SETTINGS frame).

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.2
                //= type=TODO
                //# A client MUST comply
                //# with stored settings -- or default values if no values are stored --
                //# when attempting 0-RTT.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.2
                //= type=TODO
                //# Once a server has provided new settings,
                //# clients MUST comply with those values.
                Ok(Frame::Settings(_)) => trace!("Got settings"),
                Ok(Frame::Goaway(id)) => {
                    //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.6
                    //# The GOAWAY frame is always sent on the control stream.  In the
                    //# server-to-client direction, it carries a QUIC stream ID for a client-
                    //# initiated bidirectional stream encoded as a variable-length integer.
                    //# A client MUST treat receipt of a GOAWAY frame containing a stream ID
                    //# of any other type as a connection error of type H3_ID_ERROR.
                    if !id.is_request() {
                        return Poll::Ready(Err(Code::H3_ID_ERROR.with_reason(format!(
                            "non-request StreamId in a GoAway frame: {}",
                            id
                        ))));
                    }
                    info!("Server initiated graceful shutdown, last: StreamId({})", id);
                }

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.5
                //# If a PUSH_PROMISE frame is received on the control stream, the client
                //# MUST respond with a connection error of type H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.7
                //# A client MUST treat the
                //# receipt of a MAX_PUSH_ID frame as a connection error of type
                //# H3_FRAME_UNEXPECTED.
                Ok(frame) => {
                    return Poll::Ready(Err(Code::H3_FRAME_UNEXPECTED
                        .with_reason(format!("on client control stream: {:?}", frame))))
                }
                Err(e) => {
                    let connection_error = self
                        .inner
                        .shared
                        .read("poll_close error read")
                        .error
                        .as_ref()
                        .cloned();

                    match connection_error {
                        Some(e) if e.is_closed() => return Poll::Ready(Ok(())),
                        Some(e) => return Poll::Ready(Err(e)),
                        None => {
                            self.inner.shared.write("poll_close error").error = e.clone().into();
                            return Poll::Ready(Err(e));
                        }
                    }
                }
            }
        }

        //= https://www.rfc-editor.org/rfc/rfc9114#section-6.1
        //# Clients MUST treat
        //# receipt of a server-initiated bidirectional stream as a connection
        //# error of type H3_STREAM_CREATION_ERROR unless such an extension has
        //# been negotiated.
        if self.inner.poll_accept_request(cx).is_ready() {
            return Poll::Ready(Err(self.inner.close(
                Code::H3_STREAM_CREATION_ERROR,
                "client received a bidirectional stream",
            )));
        }

        Poll::Pending
    }
}

pub struct Builder {
    max_field_section_size: u64,
    send_grease: bool,
}

impl Builder {
    pub(super) fn new() -> Self {
        Builder {
            max_field_section_size: VarInt::MAX.0,
            send_grease: true,
        }
    }

    pub fn max_field_section_size(&mut self, value: u64) -> &mut Self {
        self.max_field_section_size = value;
        self
    }

    pub async fn build<C, O, B>(
        &mut self,
        quic: C,
    ) -> Result<(Connection<C, B>, SendRequest<O, B>), Error>
    where
        C: quic::Connection<B, OpenStreams = O>,
        O: quic::OpenStreams<B>,
        B: Buf,
    {
        let open = quic.opener();
        let conn_state = SharedStateRef::default();

        let conn_waker = Some(future::poll_fn(|cx| Poll::Ready(cx.waker().clone())).await);

        Ok((
            Connection {
                inner: ConnectionInner::new(
                    quic,
                    self.max_field_section_size,
                    conn_state.clone(),
                    self.send_grease,
                )
                .await?,
            },
            SendRequest {
                open,
                conn_state,
                conn_waker,
                max_field_section_size: self.max_field_section_size,
                sender_count: Arc::new(AtomicUsize::new(1)),
                _buf: PhantomData,
                send_grease_frame: self.send_grease,
            },
        ))
    }
}

pub struct RequestStream<S, B> {
    inner: connection::RequestStream<S, B>,
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
    pub async fn recv_response(&mut self) -> Result<Response<()>, Error> {
        let mut frame = future::poll_fn(|cx| self.inner.stream.poll_next(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?
            .ok_or_else(|| {
                Code::H3_GENERAL_PROTOCOL_ERROR.with_reason("Did not receive response headers")
            })?;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.5
        //= type=TODO
        //# A client MUST treat
        //# receipt of a PUSH_PROMISE frame that contains a larger push ID than
        //# the client has advertised as a connection error of H3_ID_ERROR.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.5
        //= type=TODO
        //# If a client
        //# receives a push ID that has already been promised and detects a
        //# mismatch, it MUST respond with a connection error of type
        //# H3_GENERAL_PROTOCOL_ERROR.

        let decoded = if let Frame::Headers(ref mut encoded) = frame {
            match qpack::decode_stateless(encoded, self.inner.max_field_section_size) {
                //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
                //# An HTTP/3 implementation MAY impose a limit on the maximum size of
                //# the message header it will accept on an individual HTTP message.
                Err(qpack::DecoderError::HeaderTooLong(cancel_size)) => {
                    self.inner.stop_sending(Code::H3_REQUEST_CANCELLED);
                    return Err(Error::header_too_big(
                        cancel_size,
                        self.inner.max_field_section_size,
                    ));
                }
                Ok(decoded) => decoded,
                Err(e) => return Err(e.into()),
            }
        } else {
            return Err(
                Code::H3_FRAME_UNEXPECTED.with_reason("First response frame is not headers")
            );
        };

        let qpack::Decoded { fields, .. } = decoded;

        let (status, headers) = Header::try_from(fields)?.into_response_parts()?;
        let mut resp = Response::new(());
        *resp.status_mut() = status;
        *resp.headers_mut() = headers;
        *resp.version_mut() = http::Version::HTTP_3;

        Ok(resp)
    }

    pub async fn recv_data(&mut self) -> Result<Option<impl Buf>, Error> {
        self.inner.recv_data().await
    }

    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        let res = self.inner.recv_trailers().await;
        if let Err(ref e) = res {
            if e.is_header_too_big() {
                self.inner.stream.stop_sending(Code::H3_REQUEST_CANCELLED);
            }
        }
        res
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
    pub async fn send_data(&mut self, buf: B) -> Result<(), Error> {
        self.inner.send_data(buf).await
    }

    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        self.inner.send_trailers(trailers).await
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        self.inner.finish().await
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::BidiStream<B>,
    B: Buf,
{
    pub fn split(
        self,
    ) -> (
        RequestStream<S::SendStream, B>,
        RequestStream<S::RecvStream, B>,
    ) {
        let (send, recv) = self.inner.split();
        (RequestStream { inner: send }, RequestStream { inner: recv })
    }
}
