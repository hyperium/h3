use std::{
    convert::TryFrom,
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
    task::{Context, Poll},
};

use bytes::{Buf, Bytes, BytesMut};
use futures_util::{future, ready};
use http::HeaderMap;
use stream::WriteBuf;
use tracing::{trace, warn};

use crate::{
    config::{Config, Settings},
    error::{Code, Error},
    frame::FrameStream,
    proto::{
        frame::{self, Frame, PayloadLen},
        headers::Header,
        stream::StreamType,
        varint::VarInt,
    },
    qpack,
    quic::{self, SendStream as _},
    stream::{self, AcceptRecvStream, AcceptedRecvStream, BufRecvStream, UniStreamHeader},
    webtransport::SessionId,
};

#[doc(hidden)]
#[non_exhaustive]
pub struct SharedState {
    // Peer settings
    pub peer_config: Settings,
    // connection-wide error, concerns all RequestStreams and drivers
    pub error: Option<Error>,
    // Has a GOAWAY frame been sent or received?
    pub closing: bool,
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
            peer_config: Default::default(),
            error: None,
            closing: false,
        })))
    }
}

#[allow(missing_docs)]
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

#[allow(missing_docs)]
pub struct AcceptedStreams<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    #[allow(missing_docs)]
    pub wt_uni_streams: Vec<(SessionId, BufRecvStream<C::RecvStream, B>)>,
}

impl<B, C> Default for AcceptedStreams<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    fn default() -> Self {
        Self {
            wt_uni_streams: Default::default(),
        }
    }
}

#[allow(missing_docs)]
pub struct ConnectionInner<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    pub(super) shared: SharedStateRef,
    /// TODO: breaking encapsulation just to see if we can get this to work, will fix before merging
    pub conn: C,
    control_send: C::SendStream,
    control_recv: Option<FrameStream<C::RecvStream, B>>,
    decoder_recv: Option<AcceptedRecvStream<C::RecvStream, B>>,
    encoder_recv: Option<AcceptedRecvStream<C::RecvStream, B>>,
    /// Buffers incoming uni/recv streams which have yet to be claimed.
    ///
    /// This is opposed to discarding them by returning in `poll_accept_recv`, which may cause them to be missed by something else polling.
    ///
    /// See: <https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/#section-4.5>
    ///
    /// In WebTransport over HTTP/3, the client MAY send its SETTINGS frame, as well as
    /// multiple WebTransport CONNECT requests, WebTransport data streams and WebTransport
    /// datagrams, all within a single flight. As those can arrive out of order, a WebTransport
    /// server could be put into a situation where it receives a stream or a datagram without a
    /// corresponding session. Similarly, a client may receive a server-initiated stream or a
    /// datagram before receiving the CONNECT response headers from the server.To handle this
    /// case, WebTransport endpoints SHOULD buffer streams and datagrams until those can be
    /// associated with an established session. To avoid resource exhaustion, the endpoints
    /// MUST limit the number of buffered streams and datagrams. When the number of buffered
    /// streams is exceeded, a stream SHALL be closed by sending a RESET_STREAM and/or
    /// STOP_SENDING with the H3_WEBTRANSPORT_BUFFERED_STREAM_REJECTED error code. When the
    /// number of buffered datagrams is exceeded, a datagram SHALL be dropped. It is up to an
    /// implementation to choose what stream or datagram to discard.
    accepted_streams: AcceptedStreams<C, B>,

    pending_recv_streams: Vec<AcceptRecvStream<C::RecvStream, B>>,

    got_peer_settings: bool,
    pub send_grease_frame: bool,
    pub config: Config,
}

impl<B, C> ConnectionInner<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    pub async fn send_settings(&mut self) -> Result<(), Error> {
        #[cfg(test)]
        if !self.config.send_settings {
            return Ok(());
        }

        let settings = frame::Settings::try_from(self.config)
            .map_err(|e| Code::H3_INTERNAL_ERROR.with_cause(e))?;

        tracing::debug!("Sending server settings: {:#x?}", settings);

        //= https://www.rfc-editor.org/rfc/rfc9114#section-3.2
        //# After the QUIC connection is
        //# established, a SETTINGS frame MUST be sent by each endpoint as the
        //# initial frame of their respective HTTP control stream.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.1
        //# Each side MUST initiate a single control stream at the beginning of
        //# the connection and send its SETTINGS frame as the first frame on this
        //# stream.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4
        //# A SETTINGS frame MUST be sent as the first frame of
        //# each control stream (see Section 6.2.1) by each peer, and it MUST NOT
        //# be sent subsequently.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4
        //= type=implication
        //# SETTINGS frames MUST NOT be sent on any stream other than the control
        //# stream.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.2
        //= type=implication
        //# Endpoints MUST NOT require any data to be received from
        //# the peer prior to sending the SETTINGS frame; settings MUST be sent
        //# as soon as the transport is ready to send data.
        trace!("Sending Settings frame: {:#x?}", settings);
        stream::write(
            &mut self.control_send,
            WriteBuf::from(UniStreamHeader::Control(settings)),
        )
        .await?;

        Ok(())
    }

    /// Initiates the connection and opens a control stream
    pub async fn new(mut conn: C, shared: SharedStateRef, config: Config) -> Result<Self, Error> {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2
        //# Endpoints SHOULD create the HTTP control stream as well as the
        //# unidirectional streams required by mandatory extensions (such as the
        //# QPACK encoder and decoder streams) first, and then create additional
        //# streams as allowed by their peer.
        let control_send = future::poll_fn(|cx| conn.poll_open_send(cx))
            .await
            .map_err(|e| Code::H3_STREAM_CREATION_ERROR.with_transport(e))?;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.1
        //= type=implication
        //# The
        //# sender MUST NOT close the control stream, and the receiver MUST NOT
        //# request that the sender close the control stream.
        let mut conn_inner = Self {
            shared,
            conn,
            control_send,
            control_recv: None,
            decoder_recv: None,
            encoder_recv: None,
            pending_recv_streams: Vec::with_capacity(3),
            got_peer_settings: false,
            send_grease_frame: config.send_grease,
            config,
            accepted_streams: Default::default(),
        };

        conn_inner.send_settings().await?;

        // start a grease stream
        if config.send_grease {
            //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.8
            //= type=implication
            //# Frame types of the format 0x1f * N + 0x21 for non-negative integer
            //# values of N are reserved to exercise the requirement that unknown
            //# types be ignored (Section 9).  These frames have no semantics, and
            //# they MAY be sent on any stream where frames are allowed to be sent.
            conn_inner.start_grease_stream().await;
        }

        Ok(conn_inner)
    }

    /// Send GOAWAY with specified max_id, iff max_id is smaller than the previous one.

    pub async fn shutdown<T>(
        &mut self,
        sent_closing: &mut Option<T>,
        max_id: T,
    ) -> Result<(), Error>
    where
        T: From<VarInt> + PartialOrd<T> + Copy,
        VarInt: From<T>,
    {
        if let Some(sent_id) = sent_closing {
            if *sent_id <= max_id {
                return Ok(());
            }
        }

        *sent_closing = Some(max_id);
        self.shared.write("shutdown").closing = true;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-3.3
        //# When either endpoint chooses to close the HTTP/3
        //# connection, the terminating endpoint SHOULD first send a GOAWAY frame
        //# (Section 5.2) so that both endpoints can reliably determine whether
        //# previously sent frames have been processed and gracefully complete or
        //# terminate any necessary remaining tasks.
        stream::write(&mut self.control_send, Frame::Goaway(max_id.into())).await
    }

    #[allow(missing_docs)]
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

        // Accept the request by accepting the next bidirectional stream
        // .into().into() converts the impl QuicError into crate::error::Error.
        // The `?` operator doesn't work here for some reason.
        self.conn.poll_accept_bidi(cx).map_err(|e| e.into().into())
    }

    /// Polls incoming streams
    ///
    /// Accepted streams which are not control, decoder, or encoder streams are buffer in `accepted_recv_streams`
    pub fn poll_accept_recv(&mut self, cx: &mut Context<'_>) -> Result<(), Error> {
        if let Some(ref e) = self.shared.read("poll_accept_request").error {
            return Err(e.clone());
        }

        // Get all currently pending streams
        loop {
            match self.conn.poll_accept_recv(cx)? {
                Poll::Ready(Some(stream)) => self
                    .pending_recv_streams
                    .push(AcceptRecvStream::new(stream)),
                Poll::Ready(None) => {
                    return Err(Code::H3_GENERAL_PROTOCOL_ERROR.with_reason(
                        "Connection closed unexpected",
                        crate::error::ErrorLevel::ConnectionError,
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
            //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2
            //= type=implication
            //# As certain stream types can affect connection state, a recipient
            //# SHOULD NOT discard data from incoming unidirectional streams prior to
            //# reading the stream type.
            let stream = self
                .pending_recv_streams
                .remove(index - removed)
                .into_stream()?;

            match stream {
                //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.1
                //# Only one control stream per peer is permitted;
                //# receipt of a second stream claiming to be a control stream MUST be
                //# treated as a connection error of type H3_STREAM_CREATION_ERROR.
                AcceptedRecvStream::Control(s) => {
                    if self.control_recv.is_some() {
                        return Err(
                            self.close(Code::H3_STREAM_CREATION_ERROR, "got two control streams")
                        );
                    }
                    self.control_recv = Some(s);
                }
                enc @ AcceptedRecvStream::Encoder(_) => {
                    if let Some(_prev) = self.encoder_recv.replace(enc) {
                        return Err(
                            self.close(Code::H3_STREAM_CREATION_ERROR, "got two encoder streams")
                        );
                    }
                }
                dec @ AcceptedRecvStream::Decoder(_) => {
                    if let Some(_prev) = self.decoder_recv.replace(dec) {
                        return Err(
                            self.close(Code::H3_STREAM_CREATION_ERROR, "got two decoder streams")
                        );
                    }
                }
                AcceptedRecvStream::WebTransportUni(id, s)
                    if self.config.settings.enable_webtransport =>
                {
                    // Store until someone else picks it up, like a webtransport session which is
                    // not yet established.
                    self.accepted_streams.wt_uni_streams.push((id, s))
                }

                //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.3
                //= type=implication
                //# Endpoints MUST NOT consider these streams to have any meaning upon
                //# receipt.
                _ => (),
            }
        }

        Ok(())
    }

    /// Waits for the control stream to be received and reads subsequent frames.
    pub fn poll_control(&mut self, cx: &mut Context<'_>) -> Poll<Result<Frame<PayloadLen>, Error>> {
        if let Some(ref e) = self.shared.read("poll_accept_request").error {
            return Poll::Ready(Err(e.clone()));
        }

        let recv = {
            // TODO
            self.poll_accept_recv(cx)?;
            if let Some(v) = &mut self.control_recv {
                v
            } else {
                // Try later
                return Poll::Pending;
            }
        };

        let recvd = ready!(recv.poll_next(cx))?;

        let res = match recvd {
            //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.1
            //# If either control
            //# stream is closed at any point, this MUST be treated as a connection
            //# error of type H3_CLOSED_CRITICAL_STREAM.
            None => Err(self.close(Code::H3_CLOSED_CRITICAL_STREAM, "control stream closed")),
            Some(frame) => {
                match frame {
                    Frame::Settings(settings) if !self.got_peer_settings => {
                        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4
                        //= type=TODO
                        //# A receiver MAY treat the presence of duplicate
                        //# setting identifiers as a connection error of type H3_SETTINGS_ERROR.

                        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.1
                        //= type=TODO
                        //# Setting identifiers that were defined in [HTTP/2] where there is no
                        //# corresponding HTTP/3 setting have also been reserved
                        //# (Section 11.2.2).  These reserved settings MUST NOT be sent, and
                        //# their receipt MUST be treated as a connection error of type
                        //# H3_SETTINGS_ERROR.

                        self.got_peer_settings = true;

                        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4
                        //= type=implication
                        //# An implementation MUST ignore any parameter with an identifier it
                        //# does not understand.

                        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.1
                        //= type=implication
                        //# Endpoints MUST NOT consider such settings to have
                        //# any meaning upon receipt.
                        let mut shared = self.shared.write("connection settings write");
                        shared.peer_config = (&settings).into();

                        Ok(Frame::Settings(settings))
                    }
                    f @ Frame::Goaway(_) => Ok(f),
                    f @ Frame::CancelPush(_) | f @ Frame::MaxPushId(_) => {
                        if self.got_peer_settings {
                            //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.3
                            //= type=TODO
                            //# If a CANCEL_PUSH frame is received that
                            //# references a push ID greater than currently allowed on the
                            //# connection, this MUST be treated as a connection error of type
                            //# H3_ID_ERROR.

                            Ok(f)
                        } else {
                            //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.1
                            //# If the first frame of the control stream is any other frame
                            //# type, this MUST be treated as a connection error of type
                            //# H3_MISSING_SETTINGS.
                            Err(self.close(
                                Code::H3_MISSING_SETTINGS,
                                format!("received {:?} before settings on control stream", f),
                            ))
                        }
                    }

                    //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
                    //# Receipt of an invalid sequence of frames MUST be treated as a
                    //# connection error of type H3_FRAME_UNEXPECTED.

                    //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.1
                    //= type=implication
                    //# DATA frames MUST be associated with an HTTP request or response.

                    //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.1
                    //# If
                    //# a DATA frame is received on a control stream, the recipient MUST
                    //# respond with a connection error of type H3_FRAME_UNEXPECTED.

                    //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.2
                    //# If a HEADERS frame is received on a control stream, the recipient
                    //# MUST respond with a connection error of type H3_FRAME_UNEXPECTED.

                    //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4
                    //# If an endpoint receives a second SETTINGS
                    //# frame on the control stream, the endpoint MUST respond with a
                    //# connection error of type H3_FRAME_UNEXPECTED.
                    frame => Err(self.close(
                        Code::H3_FRAME_UNEXPECTED,
                        format!("on control stream: {:?}", frame),
                    )),
                }
            }
        };
        Poll::Ready(res)
    }

    pub(crate) fn process_goaway<T>(
        &mut self,
        recv_closing: &mut Option<T>,
        id: VarInt,
    ) -> Result<(), Error>
    where
        T: From<VarInt> + Copy,
        VarInt: From<T>,
    {
        {
            //= https://www.rfc-editor.org/rfc/rfc9114#section-5.2
            //# An endpoint MAY send multiple GOAWAY frames indicating different
            //# identifiers, but the identifier in each frame MUST NOT be greater
            //# than the identifier in any previous frame, since clients might
            //# already have retried unprocessed requests on another HTTP connection.

            //= https://www.rfc-editor.org/rfc/rfc9114#section-5.2
            //# Like the server,
            //# the client MAY send subsequent GOAWAY frames so long as the specified
            //# push ID is no greater than any previously sent value.
            if let Some(prev_id) = recv_closing.map(VarInt::from) {
                if prev_id < id {
                    //= https://www.rfc-editor.org/rfc/rfc9114#section-5.2
                    //# Receiving a GOAWAY containing a larger identifier than previously
                    //# received MUST be treated as a connection error of type H3_ID_ERROR.
                    return Err(self.close(
                        Code::H3_ID_ERROR,
                        format!(
                            "received a GoAway({}) greater than the former one ({})",
                            id, prev_id
                        ),
                    ));
                }
            }
            *recv_closing = Some(id.into());
            if !self.shared.read("connection goaway read").closing {
                self.shared.write("connection goaway overwrite").closing = true;
            }
            Ok(())
        }
    }

    /// Closes a Connection with code and reason.
    /// It returns an [`Error`] which can be returned.
    pub fn close<T: AsRef<str>>(&mut self, code: Code, reason: T) -> Error {
        self.shared.write("connection close err").error =
            Some(code.with_reason(reason.as_ref(), crate::error::ErrorLevel::ConnectionError));
        self.conn.close(code, reason.as_ref().as_bytes());
        code.with_reason(reason.as_ref(), crate::error::ErrorLevel::ConnectionError)
    }

    /// starts an grease stream
    /// https://www.rfc-editor.org/rfc/rfc9114.html#stream-grease
    async fn start_grease_stream(&mut self) {
        // start the stream
        let mut grease_stream = match future::poll_fn(|cx| self.conn.poll_open_send(cx))
            .await
            .map_err(|e| Code::H3_STREAM_CREATION_ERROR.with_transport(e))
        {
            Err(err) => {
                warn!("grease stream creation failed with {}", err);
                return;
            }
            Ok(grease) => grease,
        };

        //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.3
        //# Stream types of the format 0x1f * N + 0x21 for non-negative integer
        //# values of N are reserved to exercise the requirement that unknown
        //# types be ignored.  These streams have no semantics, and they can be
        //# sent when application-layer padding is desired.  They MAY also be
        //# sent on connections where no data is currently being transferred.
        match stream::write(&mut grease_stream, (StreamType::grease(), Frame::Grease)).await {
            Ok(()) => (),
            Err(err) => {
                warn!("write data on grease stream failed with {}", err);
                return;
            }
        }

        //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.3
        //# When sending a reserved stream type,
        //# the implementation MAY either terminate the stream cleanly or reset
        //# it.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.3
        //# When resetting the stream, either the H3_NO_ERROR error code or
        //# a reserved error code (Section 8.1) SHOULD be used.
        match future::poll_fn(|cx| grease_stream.poll_finish(cx))
            .await
            .map_err(|e| Code::H3_NO_ERROR.with_transport(e))
        {
            Ok(()) => (),
            Err(err) => warn!("grease stream error on close {}", err),
        };
    }

    #[allow(missing_docs)]
    pub fn accepted_streams_mut(&mut self) -> &mut AcceptedStreams<C, B> {
        &mut self.accepted_streams
    }
}

#[allow(missing_docs)]
pub struct RequestStream<S, B> {
    pub(super) stream: FrameStream<S, B>,
    pub(super) trailers: Option<Bytes>,
    pub(super) conn_state: SharedStateRef,
    pub(super) max_field_section_size: u64,
    send_grease_frame: bool,
}

impl<S, B> RequestStream<S, B> {
    #[allow(missing_docs)]
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

                //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
                //# Receipt of an invalid sequence of frames MUST be treated as a
                //# connection error of type H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.3
                //# Receiving a
                //# CANCEL_PUSH frame on a stream other than the control stream MUST be
                //# treated as a connection error of type H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4
                //# If an endpoint receives a SETTINGS frame on a different
                //# stream, the endpoint MUST respond with a connection error of type
                //# H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.6
                //# A client MUST treat a GOAWAY frame on a stream other than
                //# the control stream as a connection error of type H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.7
                //# The MAX_PUSH_ID frame is always sent on the control stream.  Receipt
                //# of a MAX_PUSH_ID frame on any other stream MUST be treated as a
                //# connection error of type H3_FRAME_UNEXPECTED.
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

                //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
                //# Receipt of an invalid sequence of frames MUST be treated as a
                //# connection error of type H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.3
                //# Receiving a
                //# CANCEL_PUSH frame on a stream other than the control stream MUST be
                //# treated as a connection error of type H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4
                //# If an endpoint receives a SETTINGS frame on a different
                //# stream, the endpoint MUST respond with a connection error of type
                //# H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.6
                //# A client MUST treat a GOAWAY frame on a stream other than
                //# the control stream as a connection error of type H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.7
                //# The MAX_PUSH_ID frame is always sent on the control stream.  Receipt
                //# of a MAX_PUSH_ID frame on any other stream MUST be treated as a
                //# connection error of type H3_FRAME_UNEXPECTED.
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
                //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
                //# An HTTP/3 implementation MAY impose a limit on the maximum size of
                //# the message header it will accept on an individual HTTP message.
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

    #[allow(missing_docs)]
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
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //= type=TODO
        //# Characters in field names MUST be
        //# converted to lowercase prior to their encoding.
        let mut block = BytesMut::new();

        let mem_size = qpack::encode_stateless(&mut block, Header::trailer(trailers))?;
        let max_mem_size = self
            .conn_state
            .read("send_trailers shared state read")
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
        stream::write(&mut self.stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        Ok(())
    }

    /// Stops an stream with an error code
    pub fn stop_stream(&mut self, code: Code) {
        self.stream.reset(code.into());
    }

    #[allow(missing_docs)]
    pub async fn finish(&mut self) -> Result<(), Error> {
        if self.send_grease_frame {
            // send a grease frame once per Connection
            stream::write(&mut self.stream, Frame::Grease)
                .await
                .map_err(|e| self.maybe_conn_err(e))?;
            self.send_grease_frame = false;
        }

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
