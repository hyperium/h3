use bytes::{Bytes, BytesMut};
use futures::future;
use http::{response, Request, Response};
use std::convert::TryFrom;

use crate::connection::RequestStream;
use crate::frame;
use crate::{
    connection::{Builder, ConnectionInner},
    error::{Code, Error},
    frame::FrameStream,
    proto::{frame::Frame, headers::Header},
    qpack, quic,
};

pub struct Connection<C: quic::Connection<Bytes>> {
    inner: ConnectionInner<C>,
}

impl<C> Connection<C>
where
    C: quic::Connection<Bytes>,
{
    pub async fn new(conn: C) -> Result<Self, Error> {
        Ok(Self::builder().build(conn).await?)
    }

    pub fn builder() -> Builder<Connection<C>> {
        Builder::new()
    }

    pub async fn accept(
        &mut self,
    ) -> Result<
        Option<(
            Request<()>,
            RequestStream<FrameStream<C::BidiStream>, Bytes, Connection<C>>,
        )>,
        Error,
    > {
        let stream = future::poll_fn(|cx| self.inner.quic.poll_accept_bidi_stream(cx))
            .await
            .map_err(|e| Error::transport(e))?;

        let mut stream = match stream {
            None => return Ok(None),
            Some(s) => FrameStream::new(s),
        };

        let frame = future::poll_fn(|cx| stream.poll_next(cx)).await?;

        let mut encoded = match frame {
            Some(Frame::Headers(h)) => h,
            None => {
                return Err(
                    Code::H3_REQUEST_INCOMPLETE.with_reason("request stream closed before headers")
                )
            }
            Some(_) => {
                return Err(
                    Code::H3_FRAME_UNEXPECTED.with_reason("first request frame is not headers")
                )
            }
        };

        let fields = qpack::decode_stateless(&mut encoded)?;
        let (method, uri, headers) = Header::try_from(fields)?.into_request_parts()?;

        let mut req = http::Request::new(());
        *req.method_mut() = method;
        *req.uri_mut() = uri;
        *req.headers_mut() = headers;
        *req.version_mut() = http::Version::HTTP_3;

        Ok(Some((req, RequestStream::new(stream))))
    }
}

impl<C> Builder<Connection<C>>
where
    C: quic::Connection<Bytes>,
{
    pub async fn build(&self, conn: C) -> Result<Connection<C>, Error> {
        Ok(Connection {
            inner: ConnectionInner::new(conn, self.max_field_section_size).await?,
        })
    }
}

impl<S, C> RequestStream<S, Bytes, Connection<C>>
where
    S: quic::SendStream<Bytes>,
    C: quic::Connection<Bytes>,
{
    /// Send response headers
    pub async fn send_response(&mut self, resp: Response<()>) -> Result<(), Error> {
        let (parts, _) = resp.into_parts();
        let response::Parts {
            status, headers, ..
        } = parts;
        let headers = Header::response(status, headers);

        let mut block = BytesMut::new();
        qpack::encode_stateless(&mut block, headers)?;

        frame::write(&mut self.stream, Frame::Headers(block.freeze())).await?;

        Ok(())
    }
}
