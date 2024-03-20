//! Server-side HTTP/3 stream management

use bytes::Buf;

use crate::{
    connection::{ConnectionState, SharedStateRef},
    ext::Datagram,
    quic::{self, RecvDatagramExt},
    Error,
};
use pin_project_lite::pin_project;

use super::connection::{Connection, RequestEnd};
use std::{marker::PhantomData, sync::Arc};

use std::{
    option::Option,
    result::Result,
    task::{Context, Poll},
};

use bytes::BytesMut;
use futures_util::{future::Future, ready};
use http::{response, HeaderMap, Response};

use quic::StreamId;

use crate::{
    error::Code,
    proto::{frame::Frame, headers::Header},
    qpack,
    quic::SendStream as _,
    stream::{self},
};

use tracing::error;

/// Manage request and response transfer for an incoming request
///
/// The [`RequestStream`] struct is used to send and/or receive
/// information from the client.
pub struct RequestStream<S, B> {
    pub(super) inner: crate::connection::RequestStream<S, B>,
    pub(super) request_end: Arc<RequestEnd>,
}

impl<S, B> AsMut<crate::connection::RequestStream<S, B>> for RequestStream<S, B> {
    fn as_mut(&mut self) -> &mut crate::connection::RequestStream<S, B> {
        &mut self.inner
    }
}

impl<S, B> ConnectionState for RequestStream<S, B> {
    fn shared_state(&self) -> &SharedStateRef {
        &self.inner.conn_state
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::RecvStream,
    B: Buf,
{
    /// Receive data sent from the client
    pub async fn recv_data(&mut self) -> Result<Option<impl Buf>, Error> {
        self.inner.recv_data().await
    }

    /// Poll for data sent from the client
    pub fn poll_recv_data(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<impl Buf>, Error>> {
        self.inner.poll_recv_data(cx)
    }

    /// Receive an optional set of trailers for the request
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        self.inner.recv_trailers().await
    }

    /// Tell the peer to stop sending into the underlying QUIC stream
    pub fn stop_sending(&mut self, error_code: crate::error::Code) {
        self.inner.stream.stop_sending(error_code)
    }

    /// Returns the underlying stream id
    pub fn id(&self) -> StreamId {
        self.inner.stream.id()
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::SendStream<B>,
    B: Buf,
{
    /// Send the HTTP/3 response
    ///
    /// This should be called before trying to send any data with
    /// [`RequestStream::send_data`].
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
            .peer_config
            .max_field_section_size;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
        //# An implementation that
        //# has received this parameter SHOULD NOT send an HTTP message header
        //# that exceeds the indicated size, as the peer will likely refuse to
        //# process it.
        if mem_size > max_mem_size {
            return Err(Error::header_too_big(mem_size, max_mem_size));
        }

        stream::write(&mut self.inner.stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        Ok(())
    }

    /// Send some data on the response body.
    pub async fn send_data(&mut self, buf: B) -> Result<(), Error> {
        self.inner.send_data(buf).await
    }

    /// Stop a stream with an error code
    ///
    /// The code can be [`Code::H3_NO_ERROR`].
    pub fn stop_stream(&mut self, error_code: Code) {
        self.inner.stop_stream(error_code);
    }

    /// Send a set of trailers to end the response.
    ///
    /// Either [`RequestStream::finish`] or
    /// [`RequestStream::send_trailers`] must be called to finalize a
    /// request.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        self.inner.send_trailers(trailers).await
    }

    /// End the response without trailers.
    ///
    /// Either [`RequestStream::finish`] or
    /// [`RequestStream::send_trailers`] must be called to finalize a
    /// request.
    pub async fn finish(&mut self) -> Result<(), Error> {
        self.inner.finish().await
    }

    //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1.1
    //= type=TODO
    //# Implementations SHOULD cancel requests by abruptly terminating any
    //# directions of a stream that are still open.  To do so, an
    //# implementation resets the sending parts of streams and aborts reading
    //# on the receiving parts of streams; see Section 2.4 of
    //# [QUIC-TRANSPORT].

    /// Returns the underlying stream id
    pub fn send_id(&self) -> StreamId {
        self.inner.stream.send_id()
    }
}

impl<S, B> RequestStream<S, B>
where
    S: quic::BidiStream<B>,
    B: Buf,
{
    /// Splits the Request-Stream into send and receive.
    /// This can be used the send and receive data on different tasks.
    pub fn split(
        self,
    ) -> (
        RequestStream<S::SendStream, B>,
        RequestStream<S::RecvStream, B>,
    ) {
        let (send, recv) = self.inner.split();
        (
            RequestStream {
                inner: send,
                request_end: self.request_end.clone(),
            },
            RequestStream {
                inner: recv,
                request_end: self.request_end,
            },
        )
    }
}

impl Drop for RequestEnd {
    fn drop(&mut self) {
        if let Err(e) = self.request_end.send(self.stream_id) {
            error!(
                "failed to notify connection of request end: {} {}",
                self.stream_id, e
            );
        }
    }
}

pin_project! {
    /// Future for [`Connection::read_datagram`]
    pub struct ReadDatagram<'a, C, B>
    where
            C: quic::Connection<B>,
            B: Buf,
        {
            pub(super) conn: &'a mut Connection<C, B>,
            pub(super) _marker: PhantomData<B>,
        }
}

impl<'a, C, B> Future for ReadDatagram<'a, C, B>
where
    C: quic::Connection<B> + RecvDatagramExt,
    B: Buf,
{
    type Output = Result<Option<Datagram<C::Buf>>, Error>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        tracing::trace!("poll: read_datagram");
        match ready!(self.conn.inner.conn.poll_accept_datagram(cx))? {
            Some(v) => Poll::Ready(Ok(Some(Datagram::decode(v)?))),
            None => Poll::Ready(Ok(None)),
        }
    }
}
