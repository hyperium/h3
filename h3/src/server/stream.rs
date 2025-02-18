//! Server-side HTTP/3 stream management

use bytes::Buf;

use crate::{
    error2::{
        traits::{CloseConnection, CloseStream},
        NewCode, StreamError,
    },
    proto::varint::VarInt,
    quic::{self},
    shared_state::{ConnectionState2, SharedState2},
};

use super::connection::RequestEnd;
use std::sync::Arc;

use std::{
    option::Option,
    result::Result,
    task::{Context, Poll},
};

use bytes::BytesMut;
use futures_util::future;
use http::{response, HeaderMap, Response};

use quic::StreamId;

use crate::{
    proto::{frame::Frame, headers::Header},
    qpack,
    quic::SendStream as _,
    stream::{self},
};

#[cfg(feature = "tracing")]
use tracing::{error, instrument};

/// Manage request and response transfer for an incoming request
///
/// The [`RequestStream`] struct is used to send and/or receive
/// information from the client.
/// After sending the final response, call [`RequestStream::finish`] to close the stream.
pub struct RequestStream<S, B> {
    pub(super) inner: crate::connection::RequestStream<S, B>,
    pub(super) request_end: Arc<RequestEnd>,
}

impl<S, B> AsMut<crate::connection::RequestStream<S, B>> for RequestStream<S, B> {
    fn as_mut(&mut self) -> &mut crate::connection::RequestStream<S, B> {
        &mut self.inner
    }
}

impl<S, B> ConnectionState2 for RequestStream<S, B> {
    fn shared_state(&self) -> &SharedState2 {
        &self.inner.conn_state
    }
}

impl<C, B> CloseConnection for RequestStream<C, B> {
    fn close_connection(&mut self, code: NewCode, reason: String) -> () {
        let _ = self.inner.error_sender.send((code, reason));
    }
}

impl<S, B> CloseStream for RequestStream<S, B> {}

impl<S, B> RequestStream<S, B>
where
    S: quic::RecvStream,
    B: Buf,
{
    /// Receive data sent from the client
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub async fn recv_data(&mut self) -> Result<Option<impl Buf>, StreamError> {
        future::poll_fn(|cx| self.poll_recv_data(cx)).await
    }

    /// Poll for data sent from the client
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub fn poll_recv_data(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<impl Buf>, StreamError>> {
        self.inner.poll_recv_data(cx)
    }

    /// Receive an optional set of trailers for the request
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, StreamError> {
        future::poll_fn(|cx| self.poll_recv_trailers(cx)).await
    }

    /// Poll for an optional set of trailers for the request
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub fn poll_recv_trailers(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap>, StreamError>> {
        self.inner.poll_recv_trailers(cx)
    }

    /// Tell the peer to stop sending into the underlying QUIC stream
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub fn stop_sending(&mut self, error_code: NewCode) {
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
    pub async fn send_response(&mut self, resp: Response<()>) -> Result<(), StreamError> {
        let (parts, _) = resp.into_parts();
        let response::Parts {
            status, headers, ..
        } = parts;
        let headers = Header::response(status, headers);

        let mut block = BytesMut::new();
        let mem_size = qpack::encode_stateless(&mut block, headers)
            .map_err(|_e| todo!("figure out qpack errors"))?;

        let max_mem_size = if let Some(settings) = self.inner.settings() {
            settings.max_field_section_size
        } else {
            VarInt::MAX.0
        };

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
        //# An implementation that
        //# has received this parameter SHOULD NOT send an HTTP message header
        //# that exceeds the indicated size, as the peer will likely refuse to
        //# process it.
        if mem_size > max_mem_size {
            return Err(StreamError::HeaderTooBig {
                actual_size: mem_size,
                max_size: max_mem_size,
            });
        }

        stream::write(&mut self.inner.stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.handle_quic_stream_error(e))?;

        Ok(())
    }

    /// Send some data on the response body.
    pub async fn send_data(&mut self, buf: B) -> Result<(), StreamError> {
        self.inner.send_data(buf).await
    }

    /// Stop a stream with an error code
    ///
    /// The code can be [`Code::H3_NO_ERROR`].
    pub fn stop_stream(&mut self, error_code: NewCode) {
        self.inner.stop_stream(error_code);
    }

    /// Send a set of trailers to end the response.
    ///
    /// [`RequestStream::finish`] must be called to finalize a request.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), StreamError> {
        self.inner.send_trailers(trailers).await
    }

    /// End the response without trailers.
    ///
    /// [`RequestStream::finish`] must be called to finalize a request.
    pub async fn finish(&mut self) -> Result<(), StreamError> {
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
        if let Err(_error) = self.request_end.send(self.stream_id) {
            #[cfg(feature = "tracing")]
            error!(
                "failed to notify connection of request end: {} {}",
                self.stream_id, _error
            );
        }
    }
}
