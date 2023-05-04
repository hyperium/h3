use std::convert::TryFrom;

use bytes::Buf;
use http::{Request, StatusCode};

use crate::{error::Code, proto::headers::Header, qpack, quic, server::RequestStream, Error};

pub struct ResolveRequest<C: quic::Connection<B>, B: Buf> {
    request_stream: RequestStream<C::BidiStream, B>,
    // Ok or `REQUEST_HEADER_FIELDS_TO_LARGE` which neeeds to be sent
    decoded: Result<qpack::Decoded, u64>,
    max_field_section_size: u64,
}

impl<B: Buf, C: quic::Connection<B>> ResolveRequest<C, B> {
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
        tracing::trace!("replying with: {:?}", req);
        Ok((req, self.request_stream))
    }
}
