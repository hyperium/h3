use bytes::{Buf, Bytes, BytesMut};
use futures::future;
use http::{response, HeaderMap, Request, Response, StatusCode};
use std::{
    convert::TryFrom,
    task::{Context, Poll},
};

use crate::{
    connection::{self, ConnectionInner, ConnectionState, SharedStateRef},
    error::{Code, Error},
    frame::FrameStream,
    proto::{frame::Frame, headers::Header, varint::VarInt},
    qpack, quic, stream,
};
use tracing::{trace, warn};

pub fn builder() -> Builder {
    Builder::new()
}

pub struct Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    inner: ConnectionInner<C, B>,
    max_field_section_size: u64,
}

impl<C, B> ConnectionState for Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    fn shared_state(&self) -> &SharedStateRef {
        &self.inner.shared
    }
}

impl<C> Connection<C, Bytes>
where
    C: quic::Connection<Bytes>,
{
    pub async fn new(conn: C) -> Result<Self, Error> {
        Ok(builder().build(conn).await?)
    }
}

impl<C, B> Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    pub async fn accept(
        &mut self,
    ) -> Result<Option<(Request<()>, RequestStream<C::BidiStream, B>)>, Error> {
        let mut stream = match future::poll_fn(|cx| self.poll_accept_request(cx)).await {
            Ok(Some(s)) => FrameStream::new(s),
            Ok(None) => return Ok(None),
            Err(e) => {
                if e.is_closed() {
                    return Ok(None);
                }
                return Err(e);
            }
        };

        let frame = future::poll_fn(|cx| stream.poll_next(cx)).await;

        let mut encoded = match frame {
            Ok(Some(Frame::Headers(h))) => h,
            Ok(None) => {
                return Err(
                    Code::H3_REQUEST_INCOMPLETE.with_reason("request stream closed before headers")
                )
            }
            Ok(Some(_)) => {
                return Err(
                    Code::H3_FRAME_UNEXPECTED.with_reason("first request frame is not headers")
                )
            }
            Err(e) => {
                let err: Error = e.into();
                if err.is_closed() {
                    return Ok(None);
                }
                return Err(err);
            }
        };

        let mut request_stream = RequestStream {
            inner: connection::RequestStream::new(
                stream,
                self.max_field_section_size,
                self.inner.shared.clone(),
            ),
        };

        let qpack::Decoded {
            fields, mem_size, ..
        } = qpack::decode_stateless(&mut encoded)?;
        if mem_size > self.max_field_section_size {
            request_stream
                .send_response(
                    http::Response::builder()
                        .status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
                        .body(())
                        .expect("header too big response"),
                )
                .await?;
            return Err(Error::header_too_big(mem_size, self.max_field_section_size));
        }

        let (method, uri, headers) = Header::try_from(fields)?.into_request_parts()?;

        let mut req = http::Request::new(());
        *req.method_mut() = method;
        *req.uri_mut() = uri;
        *req.headers_mut() = headers;
        *req.version_mut() = http::Version::HTTP_3;

        Ok(Some((req, request_stream)))
    }

    pub fn poll_accept_request(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<C::BidiStream>, Error>> {
        let _ = self.poll_control(cx)?;
        self.inner.poll_accept_request(cx)
    }

    fn poll_control(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        while let Poll::Ready(frame) = self.inner.poll_control(cx)? {
            match frame {
                Frame::Settings(_) => trace!("Got settings"),
                Frame::Goaway(id) => {
                    if !id.is_push() {
                        return Poll::Ready(Err(Code::H3_ID_ERROR
                            .with_reason(format!("non-push StreamId in a GoAway frame: {}", id))));
                    }
                }
                f @ Frame::MaxPushId(_) | f @ Frame::CancelPush(_) => {
                    warn!("Control frame ignored {:?}", f);
                }
                frame => {
                    return Poll::Ready(Err(Code::H3_FRAME_UNEXPECTED
                        .with_reason(format!("on server control stream: {:?}", frame))))
                }
            }
        }
        Poll::Pending
    }
}

impl<C, B> Drop for Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    fn drop(&mut self) {
        self.inner.close(Code::H3_NO_ERROR, "");
    }
}

pub struct Builder {
    pub(super) max_field_section_size: u64,
}

impl Builder {
    pub(super) fn new() -> Self {
        Builder {
            max_field_section_size: VarInt::MAX.0,
        }
    }

    pub fn max_field_section_size(&mut self, value: u64) -> &mut Self {
        self.max_field_section_size = value;
        self
    }
}

impl Builder {
    pub async fn build<C, B>(&self, conn: C) -> Result<Connection<C, B>, Error>
    where
        C: quic::Connection<B>,
        B: Buf,
    {
        Ok(Connection {
            inner: ConnectionInner::new(
                conn,
                self.max_field_section_size,
                SharedStateRef::default(),
            )
            .await?,
            max_field_section_size: self.max_field_section_size,
        })
    }
}

pub struct RequestStream<S, B>
where
    S: quic::RecvStream,
{
    inner: connection::RequestStream<FrameStream<S>, B>,
}

impl<S, B> ConnectionState for RequestStream<S, B>
where
    S: quic::RecvStream,
{
    fn shared_state(&self) -> &SharedStateRef {
        &self.inner.conn_state
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::RecvStream,
{
    pub async fn recv_data(&mut self) -> Result<Option<impl Buf>, Error> {
        self.inner.recv_data().await
    }

    pub fn stop_sending(&mut self, error_code: crate::error::Code) {
        self.inner.stream.stop_sending(error_code)
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::RecvStream + quic::SendStream<B>,
    B: Buf,
{
    pub async fn send_response(&mut self, resp: Response<()>) -> Result<(), Error> {
        let (parts, _) = resp.into_parts();
        let response::Parts {
            status, headers, ..
        } = parts;
        let headers = Header::response(status, headers);

        let mut block = BytesMut::new();
        let mem_size = qpack::encode_stateless(&mut block, headers)?;

        let max_mem_size = self
            .inner
            .conn_state
            .read("send_response")
            .peer_max_field_section_size;
        if mem_size > max_mem_size {
            return Err(Error::header_too_big(mem_size, max_mem_size));
        }

        stream::write(&mut self.inner.stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        Ok(())
    }

    pub async fn send_data(&mut self, buf: B) -> Result<(), Error> {
        self.inner.send_data(buf).await
    }

    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        self.inner.send_trailers(trailers).await
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        self.inner.finish().await
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::RecvStream + quic::SendStream<B>,
    B: Buf,
{
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        let res = self.inner.recv_trailers().await;
        if let Err(ref e) = res {
            if e.is_header_too_big() {
                self.send_response(
                    http::Response::builder()
                        .status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)
                        .body(())
                        .expect("header too big response"),
                )
                .await?;
            }
        }
        res
    }
}
