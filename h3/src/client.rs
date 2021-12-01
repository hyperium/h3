use bytes::{Bytes, BytesMut};
use futures::future;
use http::{request, HeaderMap, Response};
use std::{
    convert::TryFrom,
    marker::PhantomData,
    sync::{atomic::AtomicUsize, Arc},
    task::{Context, Poll, Waker},
};

use crate::{
    connection::{self, ConnectionInner, ConnectionState, SharedStateRef},
    error::{Code, Error},
    frame::FrameStream,
    proto::{frame::Frame, headers::Header, varint::VarInt},
    qpack, quic, stream,
};
use tracing::{trace, warn};

pub fn builder<C: quic::Connection<Bytes>>() -> Builder<C> {
    Builder::new()
}

pub async fn new<C, O>(conn: C) -> Result<(Connection<C>, SendRequest<C::OpenStreams>), Error>
where
    C: quic::Connection<Bytes, OpenStreams = O>,
    O: quic::OpenStreams<Bytes>,
{
    Ok(Builder::new().build(conn).await?)
}

pub struct SendRequest<T: quic::OpenStreams<Bytes>> {
    open: T,
    conn_state: SharedStateRef,
    max_field_section_size: u64, // maximum size for a header we receive
    // counts instances of SendRequest to close the connection when the last is dropped.
    sender_count: Arc<AtomicUsize>,
    conn_waker: Option<Waker>,
}

impl<T> SendRequest<T>
where
    T: quic::OpenStreams<Bytes>,
{
    pub async fn send_request(
        &mut self,
        req: http::Request<()>,
    ) -> Result<RequestStream<T::BidiStream>, Error> {
        let peer_max_field_section_size = self
            .conn_state
            .read("send request lock state")
            .peer_max_field_section_size;

        let (parts, _) = req.into_parts();
        let request::Parts {
            method,
            uri,
            headers,
            ..
        } = parts;
        let headers = Header::request(method, uri, headers)?;

        let mut stream = future::poll_fn(|cx| self.open.poll_open_bidi(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        let mut block = BytesMut::new();
        let mem_size = qpack::encode_stateless(&mut block, headers)?;
        if mem_size > peer_max_field_section_size {
            return Err(Error::header_too_big(mem_size, peer_max_field_section_size));
        }

        stream::write(&mut stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        Ok(RequestStream {
            inner: connection::RequestStream::new(
                FrameStream::new(stream),
                self.max_field_section_size,
                self.conn_state.clone(),
            ),
        })
    }
}

impl<T> ConnectionState for SendRequest<T>
where
    T: quic::OpenStreams<Bytes>,
{
    fn shared_state(&self) -> &SharedStateRef {
        &self.conn_state
    }
}

impl<T> Clone for SendRequest<T>
where
    T: quic::OpenStreams<Bytes> + Clone,
{
    fn clone(&self) -> Self {
        self.sender_count
            .fetch_add(1, std::sync::atomic::Ordering::Release);

        Self {
            open: self.open.clone(),
            conn_state: self.conn_state.clone(),
            max_field_section_size: self.max_field_section_size,
            sender_count: self.sender_count.clone(),
            conn_waker: self.conn_waker.clone(),
        }
    }
}

impl<T> Drop for SendRequest<T>
where
    T: quic::OpenStreams<Bytes>,
{
    fn drop(&mut self) {
        if self
            .sender_count
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
            == 1
        {
            if let Some(w) = self.conn_waker.take() {
                w.wake()
            }
            self.shared_state().write("SendRequest drop").error = Some(Error::closed());
            self.open.close(Code::H3_NO_ERROR, b"");
        }
    }
}

pub struct Connection<C>
where
    C: quic::Connection<Bytes>,
{
    inner: ConnectionInner<C>,
}

impl<C> Connection<C>
where
    C: quic::Connection<Bytes>,
{
    pub fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        while let Poll::Ready(result) = self.inner.poll_control(cx) {
            match result {
                Ok(Frame::Settings(_)) => trace!("Got settings"),
                Ok(f @ Frame::Goaway(_)) => {
                    warn!("Control frame ignored {:?}", f);
                }
                Ok(frame) => {
                    return Poll::Ready(Err(Code::H3_FRAME_UNEXPECTED
                        .with_reason(format!("on client control stream: {:?}", frame))))
                }
                Err(e) => {
                    let connection_error = self
                        .inner
                        .shared
                        .read("poll_close error read")
                        .error
                        .as_ref()
                        .map(|e| e.clone());

                    match connection_error {
                        Some(e) if e.is_closed() => return Poll::Ready(Ok(())),
                        Some(e) => return Poll::Ready(Err(e.clone())),
                        None => {
                            self.inner.shared.write("poll_close error").error = e.clone().into();
                            return Poll::Ready(Err(e));
                        }
                    }
                }
            }
        }

        if self.inner.poll_accept_request(cx).is_ready() {
            return Poll::Ready(Err(self.inner.close(
                Code::H3_STREAM_CREATION_ERROR,
                "client received a bidirectionnal stream",
            )));
        }

        Poll::Pending
    }
}

pub struct Builder<C>
where
    C: quic::Connection<Bytes>,
{
    pub(super) max_field_section_size: u64,
    _conn: PhantomData<C>,
}

impl<C, O> Builder<C>
where
    C: quic::Connection<Bytes, OpenStreams = O>,
    O: quic::OpenStreams<Bytes>,
{
    pub(super) fn new() -> Self {
        Builder {
            max_field_section_size: VarInt::MAX.0,
            _conn: PhantomData,
        }
    }

    pub fn max_field_section_size(&mut self, value: u64) -> &mut Self {
        self.max_field_section_size = value;
        self
    }

    pub async fn build(&mut self, quic: C) -> Result<(Connection<C>, SendRequest<O>), Error> {
        let open = quic.opener();
        let conn_state = SharedStateRef::default();

        let conn_waker = Some(future::poll_fn(|cx| Poll::Ready(cx.waker().clone())).await);

        Ok((
            Connection {
                inner: ConnectionInner::new(quic, self.max_field_section_size, conn_state.clone())
                    .await?,
            },
            SendRequest {
                open,
                conn_state,
                conn_waker,
                max_field_section_size: self.max_field_section_size,
                sender_count: Arc::new(AtomicUsize::new(1)),
            },
        ))
    }
}

pub struct RequestStream<S>
where
    S: quic::RecvStream,
{
    inner: connection::RequestStream<FrameStream<S>, Bytes>,
}

impl<S> ConnectionState for RequestStream<S>
where
    S: quic::RecvStream,
{
    fn shared_state(&self) -> &SharedStateRef {
        &self.inner.conn_state
    }
}

impl<S> RequestStream<S>
where
    S: quic::RecvStream,
{
    pub async fn recv_response(&mut self) -> Result<Response<()>, Error> {
        let mut frame = future::poll_fn(|cx| self.inner.stream.poll_next(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?
            .ok_or_else(|| {
                Code::H3_GENERAL_PROTOCOL_ERROR.with_reason("Did not receive response headers")
            })?;

        let decoded = if let Frame::Headers(ref mut encoded) = frame {
            qpack::decode_stateless(encoded)?
        } else {
            return Err(
                Code::H3_FRAME_UNEXPECTED.with_reason("First response frame is not headers")
            );
        };

        let qpack::Decoded {
            fields, mem_size, ..
        } = decoded;

        if mem_size > self.inner.max_field_section_size {
            self.inner.stop_sending(Code::H3_REQUEST_CANCELLED);
            return Err(Error::header_too_big(
                mem_size,
                self.inner.max_field_section_size,
            ));
        }

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
        let res = self.inner.recv_trailers().await;
        if let Err(ref e) = res {
            if let crate::error::Kind::HeaderTooBig { .. } = e.kind() {
                self.inner.stream.stop_sending(Code::H3_REQUEST_CANCELLED);
            }
        }
        res
    }

    pub fn stop_sending(&mut self, error_code: crate::error::Code) {
        self.inner.stream.stop_sending(error_code)
    }
}

impl<S> RequestStream<S>
where
    S: quic::RecvStream + quic::SendStream<Bytes>,
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
