use std::task::{Context, Poll};

use bytes::Buf;
use futures_util::future::poll_fn;
use h3::{
    error::{ConnectionError, StreamError},
    frame::FrameStream,
    quic,
    server::{Connection, RequestStream},
    stream::BufRecvStream,
    webtransport::SessionId,
};
use h3_datagram::{
    datagram_handler::{DatagramReader, HandleDatagramsExt},
    quic_traits::DatagramConnectionExt,
};
use http::Request;

use super::{BiStreamResolver, WebTransportSession};
use crate::stream::RecvStream;

/// The single owner of HTTP/3 control processing and incoming streams.
///
/// Accepting a WebTransport session does not transfer this driver to the
/// session. Continue calling [`Self::accept`] while sessions are running and
/// route WebTransport streams by their session ID. Resolve bidirectional
/// streams independently so a stalled first frame cannot block acceptance.
/// No tasks, queues, or per-session receivers are created by this type.
pub struct WebTransportConnection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    conn: Connection<C, B>,
}

/// A stream accepted by the connection driver.
pub enum IncomingStream<C: quic::Connection<B>, B: Buf> {
    /// Resolve independently to distinguish HTTP requests from WT streams.
    Bidirectional(BiStreamResolver<C, B>),
    /// An incoming WT unidirectional stream and its destination session.
    Unidirectional(SessionId, RecvStream<C::RecvStream, B>),
}

impl<C, B> WebTransportConnection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// Wrap an HTTP/3 connection configured for WebTransport.
    pub fn new(conn: Connection<C, B>) -> Self {
        Self { conn }
    }

    /// Accept the next stream while processing HTTP/3 control traffic.
    ///
    /// Dropping this future does not discard an accepted stream. The returned
    /// resolver is owned by the caller and survives subsequent session accepts.
    /// `None` has the same graceful completion meaning as
    /// [`h3::server::Connection::accept`].
    pub async fn accept(&mut self) -> Result<Option<IncomingStream<C, B>>, ConnectionError> {
        let incoming = poll_fn(|cx| self.poll_accept(cx)).await?;
        if incoming.is_none() {
            self.conn.shutdown(0).await?;
        }
        Ok(incoming)
    }

    fn take_uni(&mut self) -> Option<IncomingStream<C, B>> {
        self.conn
            .inner
            .accepted_streams_mut()
            .wt_uni_streams
            .pop()
            .map(|(id, stream)| IncomingStream::Unidirectional(id, RecvStream::new(stream)))
    }

    fn poll_accept(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<IncomingStream<C, B>>, ConnectionError>> {
        if let Some(stream) = self.take_uni() {
            return Poll::Ready(Ok(Some(stream)));
        }
        // This also processes control streams and discovers WT uni streams.
        let bidi = self.conn.poll_accept_request_stream(cx)?;
        if let Poll::Ready(Some(stream)) = bidi {
            let resolver = self
                .conn
                .create_resolver(FrameStream::new(BufRecvStream::new(stream)));
            return Poll::Ready(Ok(Some(IncomingStream::Bidirectional(BiStreamResolver {
                resolver,
            }))));
        }
        if let Some(stream) = self.take_uni() {
            return Poll::Ready(Ok(Some(stream)));
        }
        bidi.map(|_| Ok(None))
    }

    /// Initiate HTTP/3 graceful shutdown without taking ownership from sessions.
    ///
    /// Continue driving acceptance and retain session tasks until their work
    /// completes. This delegates the grace interval to the HTTP/3 connection.
    pub async fn shutdown(&mut self, max_requests: usize) -> Result<(), ConnectionError> {
        self.conn.shutdown(max_requests).await
    }
}

impl<C, B> WebTransportConnection<C, B>
where
    C: quic::Connection<B> + DatagramConnectionExt<B>,
    Connection<C, B>: HandleDatagramsExt<C, B>,
    B: Buf,
{
    /// Prepare acceptance of one CONNECT request without borrowing the driver
    /// across response I/O. The future can be polled alongside [`Self::accept`].
    pub fn accept_session(
        &mut self,
        request: Request<()>,
        stream: RequestStream<C::BidiStream, B>,
    ) -> impl std::future::Future<Output = Result<WebTransportSession<C, B>, StreamError>> {
        WebTransportSession::accept(request, stream, &mut self.conn)
    }

    /// Obtain a connection-wide datagram reader.
    ///
    /// Use one reader and dispatch by [`h3_datagram::datagram::Datagram::stream_id`].
    /// Competing per-session readers could consume another session's datagrams.
    pub fn datagram_reader(&self) -> DatagramReader<C::RecvDatagramHandler> {
        self.conn.get_datagram_reader()
    }
}
