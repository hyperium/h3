//! Client implementation of the HTTP/3 protocol

use std::{
    marker::PhantomData,
    sync::{atomic::AtomicUsize, Arc},
    task::{Context, Poll, Waker},
};

use bytes::{Buf, BytesMut};
use futures_util::future;
use http::request;

#[cfg(feature = "tracing")]
use tracing::{info, instrument, trace};

use crate::{
    connection::{self, ConnectionInner, ConnectionState, SharedStateRef},
    error::{Code, Error, ErrorLevel},
    frame::FrameStream,
    proto::{frame::Frame, headers::Header, push::PushId},
    qpack,
    quic::{self, StreamId},
    stream::{self, BufRecvStream},
};

use super::stream::RequestStream;

/// HTTP/3 request sender
///
/// [`send_request()`] initiates a new request and will resolve when it is ready to be sent
/// to the server. Then a [`RequestStream`] will be returned to send a request body (for
/// POST, PUT methods) and receive a response. After the whole body is sent, it is necessary
/// to call [`RequestStream::finish()`] to let the server know the request transfer is complete.
/// This includes the cases where no body is sent at all.
///
/// This struct is cloneable so multiple requests can be sent concurrently.
///
/// Existing instances are atomically counted internally, so whenever all of them have been
/// dropped, the connection will be automatically closed with HTTP/3 connection error code
/// `HTTP_NO_ERROR = 0`.
///
/// # Examples
///
/// ## Sending a request with no body
///
/// ```rust
/// # use h3::{quic, client::*};
/// # use http::{Request, Response};
/// # use bytes::Buf;
/// # async fn doc<T,B>(mut send_request: SendRequest<T, B>) -> Result<(), Box<dyn std::error::Error>>
/// # where
/// #     T: quic::OpenStreams<B>,
/// #     B: Buf,
/// # {
/// // Prepare the HTTP request to send to the server
/// let request = Request::get("https://www.example.com/").body(())?;
///
/// // Send the request to the server
/// let mut req_stream: RequestStream<_, _> = send_request.send_request(request).await?;
/// // Don't forget to end up the request by finishing the send stream.
/// req_stream.finish().await?;
/// // Receive the response
/// let response: Response<()> = req_stream.recv_response().await?;
/// // Process the response...
/// # Ok(())
/// # }
/// # pub fn main() {}
/// ```
///
/// ## Sending a request with a body and trailers
///
/// ```rust
/// # use h3::{quic, client::*};
/// # use http::{Request, Response, HeaderMap};
/// # use bytes::{Buf, Bytes};
/// # async fn doc<T,B>(mut send_request: SendRequest<T, Bytes>) -> Result<(), Box<dyn std::error::Error>>
/// # where
/// #     T: quic::OpenStreams<Bytes>,
/// # {
/// // Prepare the HTTP request to send to the server
/// let request = Request::get("https://www.example.com/").body(())?;
///
/// // Send the request to the server
/// let mut req_stream = send_request.send_request(request).await?;
/// // Send some data
/// req_stream.send_data("body".into()).await?;
/// // Prepare the trailers
/// let mut trailers = HeaderMap::new();
/// trailers.insert("trailer", "value".parse()?);
/// // Send them and finish the send stream
/// req_stream.send_trailers(trailers).await?;
/// // We don't need to finish the send stream, as `send_trailers()` did it for us
///
/// // Receive the response.
/// let response = req_stream.recv_response().await?;
/// // Process the response...
/// # Ok(())
/// # }
/// # pub fn main() {}
/// ```
///
/// [`send_request()`]: struct.SendRequest.html#method.send_request
/// [`RequestStream`]: struct.RequestStream.html
/// [`RequestStream::finish()`]: struct.RequestStream.html#method.finish
pub struct SendRequest<T, B>
where
    T: quic::OpenStreams<B>,
    B: Buf,
{
    pub(super) open: T,
    pub(super) conn_state: SharedStateRef,
    pub(super) max_field_section_size: u64, // maximum size for a header we receive
    // counts instances of SendRequest to close the connection when the last is dropped.
    pub(super) sender_count: Arc<AtomicUsize>,
    pub(super) conn_waker: Option<Waker>,
    pub(super) _buf: PhantomData<fn(B)>,
    pub(super) send_grease_frame: bool,
}

impl<T, B> SendRequest<T, B>
where
    T: quic::OpenStreams<B>,
    B: Buf,
{
    /// Send an HTTP/3 request to the server
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub async fn send_request(
        &mut self,
        req: http::Request<()>,
    ) -> Result<RequestStream<T::BidiStream, B>, Error> {
        let (peer_max_field_section_size, closing) = {
            let state = self.conn_state.read("send request lock state");
            (state.peer_config.max_field_section_size, state.closing)
        };

        if closing {
            return Err(Error::closing());
        }

        let (parts, _) = req.into_parts();
        let request::Parts {
            method,
            uri,
            headers,
            extensions,
            ..
        } = parts;
        let headers = Header::request(method, uri, headers, extensions)?;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
        //= type=implication
        //# A
        //# client MUST send only a single request on a given stream.
        let mut stream = future::poll_fn(|cx| self.open.poll_open_bidi(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //= type=TODO
        //# Characters in field names MUST be
        //# converted to lowercase prior to their encoding.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.1
        //= type=TODO
        //# To allow for better compression efficiency, the Cookie header field
        //# ([COOKIES]) MAY be split into separate field lines, each with one or
        //# more cookie-pairs, before compression.

        let mut block = BytesMut::new();
        let mem_size = qpack::encode_stateless(&mut block, headers)?;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2
        //# An implementation that
        //# has received this parameter SHOULD NOT send an HTTP message header
        //# that exceeds the indicated size, as the peer will likely refuse to
        //# process it.
        if mem_size > peer_max_field_section_size {
            return Err(Error::header_too_big(mem_size, peer_max_field_section_size));
        }

        stream::write(&mut stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        let request_stream = RequestStream {
            inner: connection::RequestStream::new(
                FrameStream::new(BufRecvStream::new(stream)),
                self.max_field_section_size,
                self.conn_state.clone(),
                self.send_grease_frame,
            ),
        };
        // send the grease frame only once
        self.send_grease_frame = false;
        Ok(request_stream)
    }
}

impl<T, B> ConnectionState for SendRequest<T, B>
where
    T: quic::OpenStreams<B>,
    B: Buf,
{
    fn shared_state(&self) -> &SharedStateRef {
        &self.conn_state
    }
}

impl<T, B> Clone for SendRequest<T, B>
where
    T: quic::OpenStreams<B> + Clone,
    B: Buf,
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
            _buf: PhantomData,
            send_grease_frame: self.send_grease_frame,
        }
    }
}

impl<T, B> Drop for SendRequest<T, B>
where
    T: quic::OpenStreams<B>,
    B: Buf,
{
    fn drop(&mut self) {
        if self
            .sender_count
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
            == 1
        {
            if let Some(w) = Option::take(&mut self.conn_waker) {
                w.wake()
            }
            self.shared_state().write("SendRequest drop").error = Some(Error::closed());
            self.open.close(Code::H3_NO_ERROR, b"");
        }
    }
}

/// Client connection driver
///
/// Maintains the internal state of an HTTP/3 connection, including control and QPACK.
/// It needs to be polled continuously via [`poll_close()`]. On connection closure, this
/// will resolve to `Ok(())` if the peer sent `HTTP_NO_ERROR`, or `Err()` if a connection-level
/// error occurred.
///
/// [`shutdown()`] initiates a graceful shutdown of this connection. After calling it, no request
/// initiation will be further allowed. Then [`poll_close()`] will resolve when all ongoing requests
/// and push streams complete. Finally, a connection closure with `HTTP_NO_ERROR` code will be
/// sent to the server.
///
/// # Examples
///
/// ## Drive a connection concurrently
///
/// ```rust
/// # use bytes::Buf;
/// # use futures_util::future;
/// # use h3::{client::*, quic};
/// # use tokio::task::JoinHandle;
/// # async fn doc<C, B>(mut connection: Connection<C, B>)
/// #    -> JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>
/// # where
/// #    C: quic::Connection<B> + Send + 'static,
/// #    C::SendStream: Send + 'static,
/// #    C::RecvStream: Send + 'static,
/// #    B: Buf + Send + 'static,
/// # {
/// // Run the driver on a different task
/// tokio::spawn(async move {
///     future::poll_fn(|cx| connection.poll_close(cx)).await?;
///     Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
/// })
/// # }
/// ```
///
/// ## Shutdown a connection gracefully
///
/// ```rust
/// # use bytes::Buf;
/// # use futures_util::future;
/// # use h3::quic;
/// # use h3::client::Connection;
/// # use h3::client::SendRequest;
/// # use tokio::{self, sync::oneshot, task::JoinHandle};
/// # async fn doc<C, B>(mut connection: Connection<C, B>)
/// #    -> Result<(), Box<dyn std::error::Error + Send + Sync>>
/// # where
/// #    C: quic::Connection<B> + Send + 'static,
/// #    C::SendStream: Send + 'static,
/// #    C::RecvStream: Send + 'static,
/// #    B: Buf + Send + 'static,
/// # {
/// // Prepare a channel to stop the driver thread
/// let (shutdown_tx, shutdown_rx) = oneshot::channel();
///
/// // Run the driver on a different task
/// let driver = tokio::spawn(async move {
///     tokio::select! {
///         // Drive the connection
///         closed = future::poll_fn(|cx| connection.poll_close(cx)) => closed?,
///         // Listen for shutdown condition
///         max_streams = shutdown_rx => {
///             // Initiate shutdown
///             connection.shutdown(max_streams?);
///             // Wait for ongoing work to complete
///             future::poll_fn(|cx| connection.poll_close(cx)).await?;
///         }
///     };
///
///     Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
/// });
///
/// // Do client things, wait for close condition...
///
/// // Initiate shutdown
/// shutdown_tx.send(2);
/// // Wait for the connection to be closed
/// driver.await?
/// # }
/// ```
/// [`poll_close()`]: struct.Connection.html#method.poll_close
/// [`shutdown()`]: struct.Connection.html#method.shutdown
pub struct Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    pub(super) inner: ConnectionInner<C, B>,
    // Has a GOAWAY frame been sent? If so, this PushId is the last we are willing to accept.
    pub(super) sent_closing: Option<PushId>,
    // Has a GOAWAY frame been received? If so, this is StreamId the last the remote will accept.
    pub(super) recv_closing: Option<StreamId>,
}

impl<C, B> Connection<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    /// Initiate a graceful shutdown, accepting `max_push` potentially in-flight server pushes
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub async fn shutdown(&mut self, _max_push: usize) -> Result<(), Error> {
        // TODO: Calculate remaining pushes once server push is implemented.
        self.inner.shutdown(&mut self.sent_closing, PushId(0)).await
    }

    /// Wait until the connection is closed
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub async fn wait_idle(&mut self) -> Result<(), Error> {
        future::poll_fn(|cx| self.poll_close(cx)).await
    }

    /// Maintain the connection state until it is closed
    #[cfg_attr(feature = "tracing", instrument(skip_all, level = "trace"))]
    pub fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        while let Poll::Ready(result) = self.inner.poll_control(cx) {
            match result {
                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.2
                //= type=TODO
                //# When a 0-RTT QUIC connection is being used, the initial value of each
                //# server setting is the value used in the previous session.  Clients
                //# SHOULD store the settings the server provided in the HTTP/3
                //# connection where resumption information was provided, but they MAY
                //# opt not to store settings in certain cases (e.g., if the session
                //# ticket is received before the SETTINGS frame).

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.2
                //= type=TODO
                //# A client MUST comply
                //# with stored settings -- or default values if no values are stored --
                //# when attempting 0-RTT.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.2
                //= type=TODO
                //# Once a server has provided new settings,
                //# clients MUST comply with those values.
                Ok(Frame::Settings(_)) => {
                    #[cfg(feature = "tracing")]
                    trace!("Got settings");
                    ()
                }

                Ok(Frame::Goaway(id)) => {
                    //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.6
                    //# The GOAWAY frame is always sent on the control stream.  In the
                    //# server-to-client direction, it carries a QUIC stream ID for a client-
                    //# initiated bidirectional stream encoded as a variable-length integer.
                    //# A client MUST treat receipt of a GOAWAY frame containing a stream ID
                    //# of any other type as a connection error of type H3_ID_ERROR.
                    if !StreamId::from(id).is_request() {
                        return Poll::Ready(Err(Code::H3_ID_ERROR.with_reason(
                            format!("non-request StreamId in a GoAway frame: {}", id),
                            ErrorLevel::ConnectionError,
                        )));
                    }
                    self.inner.process_goaway(&mut self.recv_closing, id)?;

                    #[cfg(feature = "tracing")]
                    info!("Server initiated graceful shutdown, last: StreamId({})", id);
                }

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.5
                //# If a PUSH_PROMISE frame is received on the control stream, the client
                //# MUST respond with a connection error of type H3_FRAME_UNEXPECTED.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.7
                //# A client MUST treat the
                //# receipt of a MAX_PUSH_ID frame as a connection error of type
                //# H3_FRAME_UNEXPECTED.
                Ok(frame) => {
                    return Poll::Ready(Err(Code::H3_FRAME_UNEXPECTED.with_reason(
                        format!("on client control stream: {:?}", frame),
                        ErrorLevel::ConnectionError,
                    )))
                }
                Err(e) => {
                    let connection_error = self.inner.shared.read("poll_close").error.clone();
                    let connection_error = match connection_error {
                        Some(e) => e,
                        None => {
                            self.inner.shared.write("poll_close error").error = Some(e.clone());
                            e
                        }
                    };
                    if connection_error.is_closed() {
                        return Poll::Ready(Ok(()));
                    }
                    return Poll::Ready(Err(connection_error));
                }
            }
        }

        //= https://www.rfc-editor.org/rfc/rfc9114#section-6.1
        //# Clients MUST treat
        //# receipt of a server-initiated bidirectional stream as a connection
        //# error of type H3_STREAM_CREATION_ERROR unless such an extension has
        //# been negotiated.
        if self.inner.poll_accept_request(cx).is_ready() {
            return Poll::Ready(Err(self.inner.close(
                Code::H3_STREAM_CREATION_ERROR,
                "client received a bidirectional stream",
            )));
        }

        Poll::Pending
    }
}
