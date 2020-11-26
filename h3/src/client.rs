use bytes::{Bytes, BytesMut};
use futures::future;
use http::{request, Response};
use std::convert::TryFrom;

use crate::connection::RequestStream;
use crate::{
    connection::{Builder, ConnectionInner},
    frame::{self, FrameStream},
    proto::{frame::Frame, headers::Header},
    qpack, quic, Error,
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
    ) -> Result<RequestStream<FrameStream<C::BidiStream>, Bytes, Connection<C>>, Error> {
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
            .map_err(|e| Error::Io(e.into()))?;

        let mut block = BytesMut::new();
        qpack::encode_stateless(&mut block, headers)?;

        frame::write(&mut stream, Frame::Headers(block.freeze())).await?;

        Ok(RequestStream::new(FrameStream::new(stream)))
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

impl<S, C> RequestStream<FrameStream<S>, Bytes, Connection<C>>
where
    S: quic::RecvStream,
    C: quic::Connection<Bytes>,
{
    /// Receive response headers
    pub async fn recv_response(&mut self) -> Result<Response<()>, Error> {
        let mut frame = future::poll_fn(|cx| self.stream.poll_next(cx))
            .await?
            .ok_or(Error::Peer("Did not receive response headers"))?;

        let fields = if let Frame::Headers(ref mut encoded) = frame {
            qpack::decode_stateless(encoded)?
        } else {
            return Err(Error::Peer("First response frame is not headers"));
        };

        let (status, headers) = Header::try_from(fields)?.into_response_parts()?;
        let mut resp = Response::new(());
        *resp.status_mut() = status;
        *resp.headers_mut() = headers;
        *resp.version_mut() = http::Version::HTTP_3;

        Ok(resp)
    }
}
