use std::convert::TryFrom;
use std::marker::PhantomData;

use bytes::{Bytes, BytesMut};
use futures::future;
use http::{request, HeaderMap, Response};

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
    ) -> Result<RequestStream<FrameStream<C::BidiStream>, Bytes>, Error> {
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

        Ok(RequestStream {
            stream: FrameStream::new(stream),
            _phantom: PhantomData,
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

pub struct RequestStream<S, B> {
    stream: S,
    _phantom: PhantomData<B>,
}

impl<S> RequestStream<FrameStream<S>, Bytes>
where
    S: quic::RecvStream,
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

        let (status, mut fields) = Header::try_from(fields)?.into_response_parts()?;
        let mut resp = Response::builder().status(status);
        resp.headers_mut().replace(&mut fields);

        Ok(resp.body(()).map_err(|_| Error::Peer("invalid headers"))?)
    }

    /// Receive some of the request body.
    pub async fn recv_data(&mut self) -> Result<Option<Bytes>, Error> {
        Ok(future::poll_fn(|cx| self.stream.poll_data(cx)).await?)
    }
}

impl<S> RequestStream<S, Bytes>
where
    S: quic::SendStream<Bytes>,
{
    /// Send some data on the response body.
    pub async fn send_data(&mut self, buf: Bytes) -> Result<(), Error> {
        frame::write(
            &mut self.stream,
            Frame::Data {
                len: buf.len() as u64,
            },
        )
        .await?;
        self.stream
            .send_data(buf)
            .map_err(|e| Error::Io(e.into()))?;
        future::poll_fn(|cx| self.stream.poll_ready(cx))
            .await
            .map_err(|e| Error::Io(e.into()))?;

        Ok(())
    }

    /// Send a set of trailers to end the request.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        let mut block = BytesMut::new();
        qpack::encode_stateless(&mut block, Header::trailer(trailers))?;

        frame::write(&mut self.stream, Frame::Headers(block.freeze())).await?;

        Ok(())
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        future::poll_fn(|cx| self.stream.poll_finish(cx))
            .await
            .map_err(|e| Error::Io(e.into()))
    }
}
