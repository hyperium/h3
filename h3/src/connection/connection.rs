use std::marker::PhantomData;

use bytes::Buf;
use futures_util::future;
use tracing::warn;

use crate::{
    error::{Code, Error},
    frame::FrameStream,
    proto::{
        frame::{Frame, PayloadLen, SettingId, Settings},
        stream::StreamType,
        varint::VarInt,
    },
    quic::{self, CloseCon, SendStream as _},
    stream::{self, AcceptRecvStream, AcceptedRecvStream},
};

use super::{
    connection_state::{ConnectionState, SharedStateRef},
    control_stream_send_handler::ControlStreamSendHandler,
};

pub trait HasQuicConnection<B: Buf> {
    type Conn: quic::CloseCon;
    fn get_conn(&mut self) -> &mut Self::Conn;
}

/// Starts a http/3 connection from a quic Connection.
pub(crate) async fn start_connection<C: quic::Connection<B> + quic::CloseCon, B: Buf>(
    mut conn: C,
    max_field_section_size: u64,
    shared: SharedStateRef,
    grease: bool,
) -> Result<
    (
        UnidirectionalStreamAcceptHandler<C, B>,
        ControlStreamSendHandler<C::SendStream, B>,
    ),
    Error,
> {
    //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2
    //# Endpoints SHOULD create the HTTP control stream as well as the
    //# unidirectional streams required by mandatory extensions (such as the
    //# QPACK encoder and decoder streams) first, and then create additional
    //# streams as allowed by their peer.
    let mut control_send = conn
        .open_send()
        .await
        .map_err(|e| Code::H3_STREAM_CREATION_ERROR.with_transport(e))?;

    let mut settings = Settings::default();
    settings
        .insert(SettingId::MAX_HEADER_LIST_SIZE, max_field_section_size)
        .map_err(|e| Code::H3_INTERNAL_ERROR.with_cause(e))?;

    if grease {
        //  Grease Settings (https://www.rfc-editor.org/rfc/rfc9114.html#name-defined-settings-parameters)
        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.1
        //# Setting identifiers of the format 0x1f * N + 0x21 for non-negative
        //# integer values of N are reserved to exercise the requirement that
        //# unknown identifiers be ignored.  Such settings have no defined
        //# meaning.  Endpoints SHOULD include at least one such setting in their
        //# SETTINGS frame.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.1
        //# Setting identifiers that were defined in [HTTP/2] where there is no
        //# corresponding HTTP/3 setting have also been reserved
        //# (Section 11.2.2).  These reserved settings MUST NOT be sent, and
        //# their receipt MUST be treated as a connection error of type
        //# H3_SETTINGS_ERROR.
        match settings.insert(SettingId::grease(), 0) {
            Ok(_) => (),
            Err(err) => warn!("Error when adding the grease Setting. Reason {}", err),
        }
    }

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
    stream::write(
        &mut control_send,
        (StreamType::CONTROL, Frame::Settings(settings)),
    )
    .await?;

    //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.1
    //= type=implication
    //# The
    //# sender MUST NOT close the control stream, and the receiver MUST NOT
    //# request that the sender close the control stream.
    let mut conn_inner = UnidirectionalStreamAcceptHandler {
        shared: shared.clone(),
        conn,
        control_recv: None,
        decoder_recv: None,
        encoder_recv: None,
        got_peer_settings: false,
    };
    // start a grease stream
    if grease {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.8
        //= type=implication
        //# Frame types of the format 0x1f * N + 0x21 for non-negative integer
        //# values of N are reserved to exercise the requirement that unknown
        //# types be ignored (Section 9).  These frames have no semantics, and
        //# they MAY be sent on any stream where frames are allowed to be sent.
        conn_inner.start_grease_stream().await;
    }

    let control_send_handler = ControlStreamSendHandler {
        shared,
        control_send,
        buf: PhantomData,
    };

    Ok((conn_inner, control_send_handler))
}

/// Accepts Unidirectional streams like the
pub struct UnidirectionalStreamAcceptHandler<C, B>
where
    C: quic::Connection<B> + quic::CloseCon,
    B: Buf,
{
    pub(crate) shared: SharedStateRef,
    conn: C,
    control_recv: Option<FrameStream<C::RecvStream, B>>,
    decoder_recv: Option<AcceptedRecvStream<C::RecvStream, B>>,
    encoder_recv: Option<AcceptedRecvStream<C::RecvStream, B>>,
    // The id of the last stream received by this connection:
    // request and push stream for server and clients respectively.
    got_peer_settings: bool,
}

impl<C, B> UnidirectionalStreamAcceptHandler<C, B>
where
    C: quic::Connection<B> + quic::CloseCon,
    B: Buf,
{
    pub async fn accept_recv(&mut self) -> Result<(), Error> {
        // TODO: put this somewhere else.
        if let Some(ref e) = self.shared.read("poll_accept_request").error {
            return Err(e.clone());
        }

        let mut accept_stream = AcceptRecvStream::new(self.conn.accept_recv().await?);
        accept_stream.receive_type().await?;
        let stream = accept_stream.into_stream()?;

        let _ = match stream {
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
                };
            }
            dec @ AcceptedRecvStream::Decoder(_) => {
                if let Some(_prev) = self.decoder_recv.replace(dec) {
                    return Err(
                        self.close(Code::H3_STREAM_CREATION_ERROR, "got two decoder streams")
                    );
                };
            }
            //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2.3
            //= type=implication
            //# Endpoints MUST NOT consider these streams to have any meaning upon
            //# receipt.
            _ => (),
        };
        Ok(())
    }

    pub async fn handle_connection_state(&mut self) -> Result<Frame<PayloadLen>, Error> {
        // TODO: put this somewhere else.
        if let Some(ref e) = self.shared.read("poll_accept_request").error {
            return Err(e.clone());
        }
        let stream = loop {
            match &mut self.control_recv {
                Some(stream) => break stream,
                None => {
                    self.accept_recv().await?;
                    continue;
                }
            }
        };
        let recv = future::poll_fn(|cx| stream.poll_next(cx)).await?;
        match recv {
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
                                //= https://www.rfc-editor.org/rfc/rfc9114#section-5.2
                                //# An endpoint MAY send multiple GOAWAY frames indicating different
                                //# identifiers, but the identifier in each frame MUST NOT be greater
                                //# than the identifier in any previous frame, since clients might
                                //# already have retried unprocessed requests on another HTTP connection.

                                //= https://www.rfc-editor.org/rfc/rfc9114#section-5.2
                                //# Like the server,
                                //# the client MAY send subsequent GOAWAY frames so long as the specified
                                //# push ID is no greater than any previously sent value.
                                if id <= closing_id {
                                    self.shared.write("connection goaway overwrite").closing =
                                        Some(id);
                                    Ok(Frame::Goaway(id))
                                } else {
                                    //= https://www.rfc-editor.org/rfc/rfc9114#section-5.2
                                    //# Receiving a GOAWAY containing a larger identifier than previously
                                    //# received MUST be treated as a connection error of type H3_ID_ERROR.
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
        }
    }

    /*    pub fn start_stream(&mut self, id: StreamId) {
        self.shared
            .write("cannot write the last stream")
            .last_accepted_stream = Some(id);
    }*/

    /// Closes a Connection with code and reason.
    /// It returns an [`Error`] which can be returned.
    pub fn close<T: AsRef<str>>(&mut self, code: Code, reason: T) -> Error {
        close_con(code, reason, self)
    }

    /// starts an grease stream
    /// https://www.rfc-editor.org/rfc/rfc9114.html#stream-grease
    async fn start_grease_stream(&mut self) {
        // start the stream
        let mut grease_stream = match self
            .conn
            .open_send()
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
}

/// Closes a Connection with code and reason.
/// It returns an [`Error`] which can be returned.
pub fn close_con<T, C, B>(code: Code, reason: T, conn: &mut C) -> Error
where
    T: AsRef<str>,
    C: HasQuicConnection<B> + ConnectionState,
    B: Buf,
{
    conn.shared_state().write("connection close err").error =
        Some(code.with_reason(reason.as_ref(), crate::error::ErrorLevel::ConnectionError));
    conn.get_conn().close(code, reason.as_ref().as_bytes());
    code.with_reason(reason.as_ref(), crate::error::ErrorLevel::ConnectionError)
}

impl<C, B> ConnectionState for UnidirectionalStreamAcceptHandler<C, B>
where
    B: Buf,
    C: quic::Connection<B> + quic::CloseCon,
{
    fn shared_state(&self) -> &SharedStateRef {
        &self.shared
    }
}

impl<C, B> HasQuicConnection<B> for UnidirectionalStreamAcceptHandler<C, B>
where
    B: Buf,
    C: quic::CloseCon + quic::Connection<B>,
{
    type Conn = C;

    fn get_conn(&mut self) -> &mut Self::Conn {
        &mut self.conn
    }
}
