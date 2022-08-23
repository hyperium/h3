use std::{
    convert::TryFrom,
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
    task::{Context, Poll},
};

use bytes::{Buf, Bytes, BytesMut};
use futures_util::{future, ready};
use http::HeaderMap;
use tracing::warn;

use crate::{
    error::{Code, Error},
    frame::FrameStream,
    proto::{
        frame::{Frame, PayloadLen, SettingId, Settings},
        headers::Header,
        stream::{StreamId, StreamType},
        varint::VarInt,
    },
    qpack,
    quic::{self, SendStream as _},
    stream::{self, AcceptRecvStream, AcceptedRecvStream},
};

#[doc(hidden)]
pub struct SharedState {
    // maximum size for a header we send
    pub peer_max_field_section_size: u64,
    // connection-wide error, concerns all RequestStreams and drivers
    pub error: Option<Error>,
    // Has the connection received a GoAway frame? If so, this StreamId is the last
    // we're willing to accept. This lets us finish the requests or pushes that were
    // already in flight when the graceful shutdown was initiated.
    pub closing: Option<StreamId>,
}

#[derive(Clone)]
#[doc(hidden)]
pub struct SharedStateRef(Arc<RwLock<SharedState>>);

impl SharedStateRef {
    pub fn read(&self, panic_msg: &'static str) -> RwLockReadGuard<SharedState> {
        self.0.read().expect(panic_msg)
    }

    pub fn write(&self, panic_msg: &'static str) -> RwLockWriteGuard<SharedState> {
        self.0.write().expect(panic_msg)
    }
}

impl Default for SharedStateRef {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(SharedState {
            peer_max_field_section_size: VarInt::MAX.0,
            error: None,
            closing: None,
        })))
    }
}

pub trait ConnectionState {
    fn shared_state(&self) -> &SharedStateRef;

    fn maybe_conn_err<E: Into<Error>>(&self, err: E) -> Error {
        if let Some(ref e) = self.shared_state().0.read().unwrap().error {
            e.clone()
        } else {
            err.into()
        }
    }
}

pub struct ConnectionInner<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    pub(super) shared: SharedStateRef,
    conn: C,
    control_send: C::SendStream,
    control_recv: Option<FrameStream<C::RecvStream, B>>,
    decoder_recv: Option<AcceptedRecvStream<C::RecvStream, B>>,
    encoder_recv: Option<AcceptedRecvStream<C::RecvStream, B>>,
    pending_recv_streams: Vec<AcceptRecvStream<C::RecvStream>>,
    // The id of the last stream received by this connection:
    // request and push stream for server and clients respectively.
    last_accepted_stream: Option<StreamId>,
    got_peer_settings: bool,
    pub(super) send_grease_frame: bool,
}

impl<C, B> ConnectionInner<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    pub async fn new(
        mut conn: C,
        max_field_section_size: u64,
        shared: SharedStateRef,
        grease: bool,
    ) -> Result<Self, Error> {
        let mut control_send = future::poll_fn(|cx| conn.poll_open_send(cx))
            .await
            .map_err(|e| Code::H3_STREAM_CREATION_ERROR.with_transport(e))?;

        let mut settings = Settings::default();
        settings
            .insert(SettingId::MAX_HEADER_LIST_SIZE, max_field_section_size)
            .map_err(|e| Code::H3_INTERNAL_ERROR.with_cause(e))?;

        // Grease Settings (https://httpwg.org/specs/rfc9114.html#rfc.section.7.2.4.1)
        if grease {
            match settings.insert(SettingId::grease(), 0) {
                Ok(_) => (),
                Err(err) => warn!("Error when adding the grease Setting. Reason {}", err),
            }
        }

        //= https://www.rfc-editor.org/rfc/rfc9114#section-3.2
        //# After the QUIC connection is
        //# established, a SETTINGS frame MUST be sent by each endpoint as the
        //# initial frame of their respective HTTP control stream.
        stream::write(
            &mut control_send,
            (StreamType::CONTROL, Frame::Settings(settings)),
        )
        .await?;

        let mut conn_inner = Self {
            shared,
            conn,
            control_send,
            control_recv: None,
            decoder_recv: None,
            encoder_recv: None,
            pending_recv_streams: Vec::with_capacity(3),
            last_accepted_stream: None,
            got_peer_settings: false,
            send_grease_frame: grease,
        };
        // start a grease stream
        if grease {
            conn_inner.start_grease_stream().await;
        }

        Ok(conn_inner)
    }

    /// Initiate graceful shutdown, accepting `max_streams` potentially in-flight streams
    pub async fn shutdown(&mut self, max_streams: usize) -> Result<(), Error> {
        let max_id = self
            .last_accepted_stream
            .map(|id| id + max_streams)
            .unwrap_or_else(StreamId::first_request);

        self.shared.write("graceful shutdown").closing = Some(max_id);

        //= https://www.rfc-editor.org/rfc/rfc9114#section-3.3
        //# When either endpoint chooses to close the HTTP/3
        //# connection, the terminating endpoint SHOULD first send a GOAWAY frame
        //# (Section 5.2) so that both endpoints can reliably determine whether
        //# previously sent frames have been processed and gracefully complete or
        //# terminate any necessary remaining tasks.
        stream::write(&mut self.control_send, Frame::Goaway(max_id)).await
    }

    pub fn poll_accept_request(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<C::BidiStream>, Error>> {
        {
            let state = self.shared.read("poll_accept_request");
            if let Some(ref e) = state.error {
                return Poll::Ready(Err(e.clone()));
            }
        }

        // .into().into() converts the impl QuicError into crate::error::Error.
        // The `?` operator doesn't work here for some reason.
        self.conn.poll_accept_bidi(cx).map_err(|e| e.into().into())
    }

    pub fn poll_accept_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if let Some(ref e) = self.shared.read("poll_accept_request").error {
            return Poll::Ready(Err(e.clone()));
        }

        loop {
            match self.conn.poll_accept_recv(cx)? {
                Poll::Ready(Some(stream)) => self
                    .pending_recv_streams
                    .push(AcceptRecvStream::new(stream)),
                Poll::Ready(None) => {
                    return Poll::Ready(Err(
                        Code::H3_GENERAL_PROTOCOL_ERROR.with_reason("Connection closed unexpected")
                    ))
                }
                Poll::Pending => break,
            }
        }

        let mut resolved = vec![];

        for (index, pending) in self.pending_recv_streams.iter_mut().enumerate() {
            match pending.poll_type(cx)? {
                Poll::Ready(()) => resolved.push(index),
                Poll::Pending => (),
            }
        }

        for (removed, index) in resolved.into_iter().enumerate() {
            let stream = self
                .pending_recv_streams
                .remove(index - removed)
                .into_stream()?;
            match stream {
                AcceptedRecvStream::Control(s) => {
                    if self.control_recv.is_some() {
                        return Poll::Ready(Err(
                            self.close(Code::H3_STREAM_CREATION_ERROR, "got two control streams")
                        ));
                    }
                    self.control_recv = Some(s);
                }
                enc @ AcceptedRecvStream::Encoder(_) => {
                    if let Some(_prev) = self.encoder_recv.replace(enc) {
                        return Poll::Ready(Err(
                            self.close(Code::H3_STREAM_CREATION_ERROR, "got two encoder streams")
                        ));
                    }
                }
                dec @ AcceptedRecvStream::Decoder(_) => {
                    if let Some(_prev) = self.decoder_recv.replace(dec) {
                        return Poll::Ready(Err(
                            self.close(Code::H3_STREAM_CREATION_ERROR, "got two decoder streams")
                        ));
                    }
                }
                _ => (),
            }
        }

        Poll::Pending
    }

    pub fn poll_control(&mut self, cx: &mut Context<'_>) -> Poll<Result<Frame<PayloadLen>, Error>> {
        if let Some(ref e) = self.shared.read("poll_accept_request").error {
            return Poll::Ready(Err(e.clone()));
        }

        loop {
            match self.poll_accept_recv(cx) {
                Poll::Ready(Ok(_)) => continue,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending if self.control_recv.is_none() => return Poll::Pending,
                _ => break,
            }
        }

        let recvd = ready!(self
            .control_recv
            .as_mut()
            .expect("control_recv")
            .poll_next(cx))?;

        let res = match recvd {
            None => Err(self.close(Code::H3_CLOSED_CRITICAL_STREAM, "control stream closed")),
            Some(frame) => {
                match frame {
                    Frame::Settings(settings) if !self.got_peer_settings => {
                        self.got_peer_settings = true;
                        self.shared
                            .write("connection settings write")
                            .peer_max_field_section_size = settings
                            .get(SettingId::MAX_HEADER_LIST_SIZE)
                            .unwrap_or(VarInt::MAX.0);
                        Ok(Frame::Settings(settings))
                    }
                    Frame::Goaway(id) => {
                        let closing = self.shared.read("connection goaway read").closing;
                        match closing {
                            Some(closing_id) if closing_id.initiator() == id.initiator() => {
                                if id <= closing_id {
                                    self.shared.write("connection goaway overwrite").closing =
                                        Some(id);
                                    Ok(Frame::Goaway(id))
                                } else {
                                    Err(self.close(
                                        Code::H3_ID_ERROR,
                                        format!("received a GoAway({}) greater than the former one ({})", id, closing_id)
                                ))
                                }
                            }
                            // When closing initiator is different, the current side has already started to close
                            // and should not be initiating any new requests / pushes anyway. So we can ignore it.
                            Some(_) => Ok(Frame::Goaway(id)),
                            None => {
                                self.shared.write("connection goaway write").closing = Some(id);
                                Ok(Frame::Goaway(id))
                            }
                        }
                    }
                    f @ Frame::CancelPush(_) | f @ Frame::MaxPushId(_) => {
                        if self.got_peer_settings {
                            Ok(f)
                        } else {
                            Err(self.close(
                                Code::H3_MISSING_SETTINGS,
                                format!("received {:?} before settings on control stream", f),
                            ))
                        }
                    }
                    frame => Err(self.close(
                        Code::H3_FRAME_UNEXPECTED,
                        format!("on control stream: {:?}", frame),
                    )),
                }
            }
        };
        Poll::Ready(res)
    }

    pub fn start_stream(&mut self, id: StreamId) {
        self.last_accepted_stream = Some(id);
    }

    pub fn close<T: AsRef<str>>(&mut self, code: Code, reason: T) -> Error {
        self.shared.write("connection close err").error = Some(code.with_reason(reason.as_ref()));
        self.conn.close(code, reason.as_ref().as_bytes());
        code.with_reason(reason.as_ref())
    }

    /// starts an grease stream
    /// https://httpwg.org/specs/rfc9114.html#stream-grease
    async fn start_grease_stream(&mut self) {
        // start the stream
        let mut grease_stream = match future::poll_fn(|cx| self.conn.poll_open_send(cx))
            .await
            .map_err(|e| Code::H3_STREAM_CREATION_ERROR.with_transport(e))
        {
            Err(err) => {
                warn!("grease stream creation failed with {}", err);
                return ();
            }
            Ok(grease) => grease,
        };
        // send a frame over the stream
        match stream::write(&mut grease_stream, (StreamType::grease(), Frame::Grease)).await {
            Ok(_) => (),
            Err(err) => {
                warn!("write data on grease stream failed with {}", err);
                return ();
            }
        }
        // close the stream
        match future::poll_fn(|cx| grease_stream.poll_finish(cx))
            .await
            .map_err(|e| Code::H3_NO_ERROR.with_transport(e))
        {
            Err(err) => {
                warn!("grease stream error on close {}", err);
                return ();
            }
            Ok(_) => (),
        }
        ()
    }
}

pub struct RequestStream<S, B> {
    pub(super) stream: FrameStream<S, B>,
    pub(super) trailers: Option<Bytes>,
    pub(super) conn_state: SharedStateRef,
    pub(super) max_field_section_size: u64,
    send_grease_frame: bool,
}

impl<S, B> RequestStream<S, B> {
    pub fn new(
        stream: FrameStream<S, B>,
        max_field_section_size: u64,
        conn_state: SharedStateRef,
        grease: bool,
    ) -> Self {
        Self {
            stream,
            conn_state,
            max_field_section_size,
            trailers: None,
            send_grease_frame: grease,
        }
    }
}

impl<S, B> ConnectionState for RequestStream<S, B> {
    fn shared_state(&self) -> &SharedStateRef {
        &self.conn_state
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::RecvStream,
{
    /// Receive some of the request body.
    pub async fn recv_data(&mut self) -> Result<Option<impl Buf>, Error> {
        if !self.stream.has_data() {
            let frame = future::poll_fn(|cx| self.stream.poll_next(cx))
                .await
                .map_err(|e| self.maybe_conn_err(e))?;
            match frame {
                Some(Frame::Data { .. }) => (),
                Some(Frame::Headers(encoded)) => {
                    self.trailers = Some(encoded);
                    return Ok(None);
                }
                Some(_) => return Err(Code::H3_FRAME_UNEXPECTED.into()),
                None => return Ok(None),
            }
        }

        let data = future::poll_fn(|cx| self.stream.poll_data(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;
        Ok(data)
    }

    /// Receive trailers
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        let mut trailers = if let Some(encoded) = self.trailers.take() {
            encoded
        } else {
            let frame = future::poll_fn(|cx| self.stream.poll_next(cx))
                .await
                .map_err(|e| self.maybe_conn_err(e))?;
            match frame {
                Some(Frame::Headers(encoded)) => encoded,
                Some(_) => return Err(Code::H3_FRAME_UNEXPECTED.into()),
                None => return Ok(None),
            }
        };

        if !self.stream.is_eos() {
            // Get the trailing frame
            let trailing_frame = future::poll_fn(|cx| self.stream.poll_next(cx))
                .await
                .map_err(|e| self.maybe_conn_err(e))?;

            if trailing_frame.is_some() {
                // if it's not unknown or reserved, fail.
                return Err(Code::H3_FRAME_UNEXPECTED.into());
            }
        }

        let qpack::Decoded { fields, .. } =
            match qpack::decode_stateless(&mut trailers, self.max_field_section_size) {
                Err(qpack::DecoderError::HeaderTooLong(cancel_size)) => {
                    return Err(Error::header_too_big(
                        cancel_size,
                        self.max_field_section_size,
                    ))
                }
                Ok(decoded) => decoded,
                Err(e) => return Err(e.into()),
            };

        Ok(Some(Header::try_from(fields)?.into_fields()))
    }

    pub fn stop_sending(&mut self, err_code: Code) {
        self.stream.stop_sending(err_code);
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::SendStream<B>,
    B: Buf,
{
    /// Send some data on the response body.
    pub async fn send_data(&mut self, buf: B) -> Result<(), Error> {
        let frame = Frame::Data(buf);

        stream::write(&mut self.stream, frame)
            .await
            .map_err(|e| self.maybe_conn_err(e))?;
        Ok(())
    }

    /// Send a set of trailers to end the request.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        let mut block = BytesMut::new();
        let mem_size = qpack::encode_stateless(&mut block, Header::trailer(trailers))?;
        let max_mem_size = self
            .conn_state
            .read("send_trailers shared state read")
            .peer_max_field_section_size;
        if mem_size > max_mem_size {
            return Err(Error::header_too_big(mem_size, max_mem_size));
        }
        stream::write(&mut self.stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        Ok(())
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        if self.send_grease_frame {
            // send a grease frame once per Connection
            stream::write(&mut self.stream, Frame::Grease)
                .await
                .map_err(|e| self.maybe_conn_err(e))?;
            self.send_grease_frame = false;
        }
        future::poll_fn(|cx| self.stream.poll_ready(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;
        future::poll_fn(|cx| self.stream.poll_finish(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::BidiStream<B>,
    B: Buf,
{
    pub(crate) fn split(
        self,
    ) -> (
        RequestStream<S::SendStream, B>,
        RequestStream<S::RecvStream, B>,
    ) {
        let (send, recv) = self.stream.split();

        (
            RequestStream {
                stream: send,
                trailers: None,
                conn_state: self.conn_state.clone(),
                max_field_section_size: 0,
                send_grease_frame: self.send_grease_frame,
            },
            RequestStream {
                stream: recv,
                trailers: self.trailers,
                conn_state: self.conn_state,
                max_field_section_size: self.max_field_section_size,
                send_grease_frame: self.send_grease_frame,
            },
        )
    }
}
