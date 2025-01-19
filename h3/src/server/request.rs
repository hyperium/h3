use std::{convert::TryFrom, sync::Arc};

use bytes::Buf;
use http::{Request, StatusCode};

use tokio::sync::mpsc::UnboundedSender;
#[cfg(feature = "tracing")]
use tracing::instrument;

use crate::{
    connection::{self, ConnectionState, SharedStateRef},
    error::{Code, ErrorLevel},
    frame::{FrameStream, FrameStreamError},
    proto::{
        frame::{Frame, PayloadLen},
        headers::Header,
    },
    qpack,
    quic::{self, SendStream, StreamId},
    Error,
};

use super::{connection::RequestEnd, stream::RequestStream};

/// Helper struct to await the request headers and return a `Request` object
pub struct RequestResolver<C, B>
where
    C: quic::Connection<B>,
    C::BidiStream: quic::SendStream<B>,
    B: Buf,
{
    pub(super) frame_stream: FrameStream<C::BidiStream, B>,
    pub(super) error_sender: UnboundedSender<(Code, &'static str)>,
    pub(super) request_end_send: UnboundedSender<StreamId>,
    pub(super) send_grease_frame: bool,
    pub(super) max_field_section_size: u64,
    pub(super) shared: SharedStateRef,
}

impl<C, B> RequestResolver<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// Returns a future to await the request headers and return a `Request` object
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub async fn resolve_request(
        mut self,
    ) -> Result<Option<(Request<()>, RequestStream<C::BidiStream, B>)>, Error> {
        let frame = std::future::poll_fn(|cx| self.frame_stream.poll_next(cx)).await;
        let req = self.accept_with_frame(frame)?;
        if let Some(req) = req {
            Ok(Some(req.resolve().await?))
        } else {
            Ok(None)
        }
    }

    /// Accepts a http request where the first frame has already been read and decoded.
    ///
    /// This is needed as a bidirectional stream may be read as part of incoming webtransport
    /// bi-streams. If it turns out that the stream is *not* a `WEBTRANSPORT_STREAM` the request
    /// may still want to be handled and passed to the user.
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub fn accept_with_frame(
        mut self,
        frame: Result<Option<Frame<PayloadLen>>, FrameStreamError>,
    ) -> Result<Option<ResolvedRequest<C, B>>, Error> {
        let mut encoded = match frame {
            Ok(Some(Frame::Headers(h))) => h,

            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
            //# If a client-initiated
            //# stream terminates without enough of the HTTP message to provide a
            //# complete response, the server SHOULD abort its response stream with
            //# the error code H3_REQUEST_INCOMPLETE.
            Ok(None) => {
                self.frame_stream.reset(Code::H3_REQUEST_INCOMPLETE.into());
                return Err(Code::H3_REQUEST_INCOMPLETE
                    .with_reason("stream terminated without headers", ErrorLevel::StreamError));
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

                let _ = self.error_sender.send((
                    Code::H3_FRAME_UNEXPECTED,
                    "first request frame is not headers",
                ));

                return Err(Code::H3_FRAME_UNEXPECTED.with_reason(
                    "first request frame is not headers",
                    ErrorLevel::ConnectionError,
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
                        // TODO: give the provided reason
                        let _ = self
                            .error_sender
                            .send((code, "error reading first request frame"));

                        return Err(code.with_reason("", ErrorLevel::ConnectionError));
                    }
                    crate::error::Kind::Application {
                        code,
                        reason: _,
                        level: ErrorLevel::StreamError,
                    } => {
                        self.frame_stream.reset(code.into());
                        return Err(err);
                    }
                    _ => return Err(err),
                };
            }
        };

        let mut request_stream = RequestStream {
            request_end: Arc::new(RequestEnd {
                request_end: self.request_end_send.clone(),
                stream_id: self.frame_stream.send_id(),
            }),
            inner: connection::RequestStream::new(
                self.frame_stream,
                self.max_field_section_size,
                self.shared.clone(),
                self.send_grease_frame,
                self.error_sender.clone(),
            ),
        };

        let decoded = match qpack::decode_stateless(&mut encoded, self.max_field_section_size) {
            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
            //# An HTTP/3 implementation MAY impose a limit on the maximum size of
            //# the message header it will accept on an individual HTTP message.
            Err(qpack::DecoderError::HeaderTooLong(cancel_size)) => Err(cancel_size),
            Ok(decoded) => Ok(decoded),
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
                        let _ = self
                            .error_sender
                            .send((code, "error decoding first request frame"));

                        return Err(code.with_reason("", ErrorLevel::ConnectionError));
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

        Ok(Some(ResolvedRequest::new(
            request_stream,
            decoded,
            self.max_field_section_size,
        )))
    }
}

pub struct ResolvedRequest<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    request_stream: RequestStream<C::BidiStream, B>,
    // Ok or `REQUEST_HEADER_FIELDS_TO_LARGE` which needs to be sent
    decoded: Result<qpack::Decoded, u64>,
    max_field_section_size: u64,
}

impl<B, C> ResolvedRequest<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    pub fn new(
        request_stream: RequestStream<C::BidiStream, B>,
        decoded: Result<qpack::Decoded, u64>,
        max_field_section_size: u64,
    ) -> Self {
        Self {
            request_stream,
            decoded,
            max_field_section_size,
        }
    }

    /// Finishes the resolution of the request
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub async fn resolve(
        mut self,
    ) -> Result<(Request<()>, RequestStream<C::BidiStream, B>), Error> {
        let fields = match self.decoded {
            Ok(v) => v.fields,
            Err(cancel_size) => {
                // Send and await the error response
                self.request_stream
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
        };

        // Parse the request headers
        let (method, uri, protocol, headers) = match Header::try_from(fields) {
            Ok(header) => match header.into_request_parts() {
                Ok(parts) => parts,
                Err(err) => {
                    //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1.2
                    //# Malformed requests or responses that are
                    //# detected MUST be treated as a stream error of type H3_MESSAGE_ERROR.
                    let error: Error = err.into();
                    self.request_stream
                        .stop_stream(error.try_get_code().unwrap_or(Code::H3_MESSAGE_ERROR));
                    return Err(error);
                }
            },
            Err(err) => {
                //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1.2
                //# Malformed requests or responses that are
                //# detected MUST be treated as a stream error of type H3_MESSAGE_ERROR.
                let error: Error = err.into();
                self.request_stream
                    .stop_stream(error.try_get_code().unwrap_or(Code::H3_MESSAGE_ERROR));
                return Err(error);
            }
        };

        //  request_stream.stop_stream(Code::H3_MESSAGE_ERROR).await;
        let mut req = http::Request::new(());
        *req.method_mut() = method;
        *req.uri_mut() = uri;
        *req.headers_mut() = headers;
        // NOTE: insert `Protocol` and not `Option<Protocol>`
        if let Some(protocol) = protocol {
            req.extensions_mut().insert(protocol);
        }
        *req.version_mut() = http::Version::HTTP_3;
        // send the grease frame only once
        // self.inner.send_grease_frame = false;

        #[cfg(feature = "tracing")]
        tracing::trace!("replying with: {:?}", req);

        Ok((req, self.request_stream))
    }
}

impl<C, B> ConnectionState for RequestResolver<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    fn shared_state(&self) -> &SharedStateRef {
        &self.shared
    }
}
