use std::convert::TryFrom;
use std::marker::PhantomData;

use bytes::{Bytes, BytesMut};
use futures::future;
use http::{response, HeaderMap, Request, Response};

use crate::frame;
use crate::{
    connection::{Builder, ConnectionInner},
    frame::FrameStream,
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

    pub async fn accept(
        &mut self,
    ) -> Result<
        Option<(
            Request<()>,
            RequestStream<FrameStream<C::BidiStream>, Bytes>,
        )>,
        Error,
    > {
        let stream = future::poll_fn(|cx| self.inner.quic.poll_accept_bidi_stream(cx))
            .await
            .map_err(|e| Error::Io(e.into()))?;

        let mut stream = match stream {
            None => return Ok(None),
            Some(s) => FrameStream::new(s),
        };

        let frame = future::poll_fn(|cx| stream.poll_next(cx)).await?;

        let mut encoded = match frame {
            Some(Frame::Headers(h)) => h,
            None => return Err(Error::Peer("request stream closed before headers")),
            Some(_) => return Err(Error::Peer("first request frame is not headers")),
        };

        let fields = qpack::decode_stateless(&mut encoded)?;
        let (method, uri, mut headers) = Header::try_from(fields)?.into_request_parts()?;

        let mut request_builder = http::Request::builder().method(method).uri(uri);
        request_builder.headers_mut().replace(&mut headers);

        Ok(Some((
            request_builder
                .body(())
                .map_err(|_| Error::Peer("invalid headers"))?,
            RequestStream {
                stream,
                _phantom: PhantomData,
            },
        )))
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

pub struct RequestStream<S, B> {
    stream: S,
    _phantom: PhantomData<B>,
}

impl<S> RequestStream<S, Bytes>
where
    S: quic::SendStream<Bytes>,
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

    /// Send a set of trailers to end the response.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        let mut block = BytesMut::new();
        qpack::encode_stateless(&mut block, Header::trailer(trailers))?;

        frame::write(&mut self.stream, Frame::Headers(block.freeze())).await?;

        Ok(())
    }

    // Wait for all data to be sent and close the stream
    pub async fn finish(&mut self) -> Result<(), Error> {
        future::poll_fn(|cx| self.stream.poll_ready(cx))
            .await
            .map_err(|e| Error::Io(e.into()))?;
        future::poll_fn(|cx| self.stream.poll_finish(cx))
            .await
            .map_err(|e| Error::Io(e.into()))
    }
}

impl<S> RequestStream<FrameStream<S>, Bytes>
where
    S: quic::RecvStream,
{
    /// Receive some of the request body.
    pub async fn recv_data(&mut self) -> Result<Option<Bytes>, Error> {
        Ok(future::poll_fn(|cx| self.stream.poll_data(cx)).await?)
    }

    /// Receive an optional set of trailers for the request.
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        let mut frame = future::poll_fn(|cx| self.stream.poll_next(cx)).await?;

        let fields = match frame {
            Some(Frame::Headers(ref mut b)) => qpack::decode_stateless(b)?,
            Some(_) => return Err(Error::Peer("Unexpected frame types")),
            None => return Ok(None),
        };

        Ok(Some(Header::try_from(fields)?.into_fields()))
    }
}
