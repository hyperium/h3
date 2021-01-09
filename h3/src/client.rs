use bytes::{Bytes, BytesMut};
use futures::future;
use http::{request, HeaderMap, Response};
use std::convert::TryFrom;

use crate::{
    connection::{self, Builder, ConnectionInner},
    error::{Code, Error},
    frame::{self, FrameStream},
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

    pub async fn send_request(
        &mut self,
        req: http::Request<()>,
    ) -> Result<RequestStream<FrameStream<C::BidiStream>>, Error> {
        let (parts, _) = req.into_parts();
        let request::Parts {
            method,
            uri,
            headers,
            ..
        } = parts;
        let headers = Header::request(method, uri, headers)?;

        let mut stream = future::poll_fn(|mut cx| self.inner.quic.poll_open_bidi_stream(&mut cx))
            .await
            .map_err(|e| Error::transport(e))?;

        let mut block = BytesMut::new();
        qpack::encode_stateless(&mut block, headers)?;

        frame::write(&mut stream, Frame::Headers(block.freeze())).await?;

        Ok(RequestStream {
            inner: connection::RequestStream::new(FrameStream::new(stream)),
        })
    }
}

impl<C> Builder<Connection<C>>
where
    C: quic::Connection<Bytes>,
{
    pub async fn build(self, conn: C) -> Result<Connection<C>, Error> {
        Ok(Connection {
            inner: ConnectionInner::new(conn, self.max_field_section_size).await?,
        })
    }
}

pub struct RequestStream<S> {
    inner: connection::RequestStream<S, Bytes>,
}

impl<S> RequestStream<FrameStream<S>>
where
    S: quic::RecvStream,
{
    pub async fn recv_response(&mut self) -> Result<Response<()>, Error> {
        let mut frame = future::poll_fn(|cx| self.inner.stream.poll_next(cx))
            .await?
            .ok_or_else(|| {
                Code::H3_GENERAL_PROTOCOL_ERROR.with_reason("Did not receive response headers")
            })?;

        let fields = if let Frame::Headers(ref mut encoded) = frame {
            qpack::decode_stateless(encoded)?
        } else {
            return Err(
                Code::H3_FRAME_UNEXPECTED.with_reason("First response frame is not headers")
            );
        };

        let (status, headers) = Header::try_from(fields)?.into_response_parts()?;
        let mut resp = Response::new(());
        *resp.status_mut() = status;
        *resp.headers_mut() = headers;
        *resp.version_mut() = http::Version::HTTP_3;

        Ok(resp)
    }

    pub async fn recv_data(&mut self) -> Result<Option<Bytes>, Error> {
        self.inner.recv_data().await
    }

    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        self.inner.recv_trailers().await
    }
}

impl<S> RequestStream<S>
where
    S: quic::SendStream<Bytes>,
{
    pub async fn send_data(&mut self, buf: Bytes) -> Result<(), Error> {
        self.inner.send_data(buf).await
    }

    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        self.inner.send_trailers(trailers).await
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        self.inner.finish().await
    }
}
