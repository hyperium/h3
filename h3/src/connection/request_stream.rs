use std::convert::TryFrom;

use bytes::{Buf, Bytes, BytesMut};
use futures_util::future;
use http::HeaderMap;

use crate::{
    error::Code,
    frame::FrameStream,
    proto::{frame::Frame, headers::Header},
    qpack,
    quic::{self, SendStream},
    stream, Error,
};

use super::connection_state::{ConnectionState, SharedStateRef};

pub struct RequestStream<S, B> {
    pub(crate) stream: FrameStream<S, B>,
    pub(crate) trailers: Option<Bytes>,
    pub(crate) conn_state: SharedStateRef,
    pub(crate) max_field_section_size: u64,
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

                // Todo: Send response with status 431, see spec
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
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //= type=TODO
        //# Characters in field names MUST be
        //# converted to lowercase prior to their encoding.
        let mut block = BytesMut::new();

        let mem_size = qpack::encode_stateless(&mut block, Header::trailer(trailers))?;
        let max_mem_size = self
            .conn_state
            .read("send_trailers shared state read")
            .peer_max_field_section_size;

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
