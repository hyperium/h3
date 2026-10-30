use std::{
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll, Waker},
};

use bytes::{Buf, Bytes};
use h3::{
    quic::{self, StreamErrorIncoming},
    stream::BufRecvStream,
};
use tokio::io::ReadBuf;

// Protocol error for streams whose WebTransport session has ended.
pub(crate) const WT_SESSION_GONE: u64 = 0x170d7b68;

pub(crate) struct StreamState<S, B> {
    stream: BufRecvStream<S, B>,
    closed: bool,
    reader: Option<Waker>,
    writer: Option<Waker>,
}

pub(crate) type SharedStream<S, B> = Arc<Mutex<StreamState<S, B>>>;
pub(crate) type WeakStream<S, B> = Weak<Mutex<StreamState<S, B>>>;

fn terminated() -> StreamErrorIncoming {
    StreamErrorIncoming::StreamTerminated {
        error_code: WT_SESSION_GONE,
    }
}

fn io_terminated() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::ConnectionAborted,
        "WebTransport session closed",
    )
}

fn close<S, B>(stream: &SharedStream<S, B>, reset: impl FnOnce(&mut BufRecvStream<S, B>)) {
    let (reader, writer) = {
        let mut state = stream.lock().unwrap();
        if state.closed {
            return;
        }
        state.closed = true;
        reset(&mut state.stream);
        (state.reader.take(), state.writer.take())
    };
    if let Some(reader) = reader {
        reader.wake();
    }
    if let Some(writer) = writer {
        writer.wake();
    }
}

pub(crate) fn close_recv<S: quic::RecvStream, B>(stream: &SharedStream<S, B>) {
    close(stream, |s| {
        quic::RecvStream::stop_sending(s, WT_SESSION_GONE)
    });
}

pub(crate) fn close_send<S: quic::SendStream<B>, B: Buf>(stream: &SharedStream<S, B>) {
    close(stream, |s| quic::SendStream::reset(s, WT_SESSION_GONE));
}

pub(crate) fn close_bidi<S: quic::RecvStream + quic::SendStream<B>, B: Buf>(
    stream: &SharedStream<S, B>,
) {
    close(stream, |s| {
        quic::SendStream::reset(s, WT_SESSION_GONE);
        quic::RecvStream::stop_sending(s, WT_SESSION_GONE);
    });
}

/// WebTransport receive stream.
///
/// Session termination resets the transport even when this handle is idle.
pub struct RecvStream<S, B> {
    pub(crate) stream: SharedStream<S, B>,
}

impl<S, B> RecvStream<S, B> {
    pub(crate) fn new(stream: BufRecvStream<S, B>) -> Self {
        Self {
            stream: Arc::new(Mutex::new(StreamState {
                stream,
                closed: false,
                reader: None,
                writer: None,
            })),
        }
    }
}

impl<S, B> std::fmt::Debug for RecvStream<S, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecvStream").finish_non_exhaustive()
    }
}

impl<S: quic::RecvStream, B> quic::RecvStream for RecvStream<S, B> {
    type Buf = Bytes;
    fn poll_data(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, StreamErrorIncoming>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(terminated()));
        }
        state.reader = Some(cx.waker().clone());
        state.stream.poll_data(cx)
    }
    fn stop_sending(&mut self, code: u64) {
        self.stream.lock().unwrap().stream.stop_sending(code);
    }
    fn recv_id(&self) -> quic::StreamId {
        self.stream.lock().unwrap().stream.recv_id()
    }
}

impl<S: quic::RecvStream, B: Buf> tokio::io::AsyncRead for RecvStream<S, B> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(io_terminated()));
        }
        state.reader = Some(cx.waker().clone());
        tokio::io::AsyncRead::poll_read(Pin::new(&mut state.stream), cx, buf)
    }
}

impl<S: quic::RecvStream, B: Buf> futures_util::io::AsyncRead for RecvStream<S, B> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(io_terminated()));
        }
        state.reader = Some(cx.waker().clone());
        futures_util::io::AsyncRead::poll_read(Pin::new(&mut state.stream), cx, buf)
    }
}

/// WebTransport send stream.
///
/// Session termination resets the transport even when this handle is idle.
pub struct SendStream<S, B> {
    pub(crate) stream: SharedStream<S, B>,
}

impl<S, B> SendStream<S, B> {
    pub(crate) fn new(stream: BufRecvStream<S, B>) -> Self {
        Self {
            stream: Arc::new(Mutex::new(StreamState {
                stream,
                closed: false,
                reader: None,
                writer: None,
            })),
        }
    }
}

impl<S, B> std::fmt::Debug for SendStream<S, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendStream").finish_non_exhaustive()
    }
}

impl<S: quic::SendStream<B>, B: Buf> quic::SendStream<B> for SendStream<S, B> {
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(terminated()));
        }
        state.writer = Some(cx.waker().clone());
        state.stream.poll_ready(cx)
    }
    fn send_data<T: Into<h3::stream::WriteBuf<B>>>(
        &mut self,
        data: T,
    ) -> Result<(), StreamErrorIncoming> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Err(terminated());
        }
        state.stream.send_data(data)
    }
    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(terminated()));
        }
        state.writer = Some(cx.waker().clone());
        state.stream.poll_finish(cx)
    }
    fn reset(&mut self, code: u64) {
        self.stream.lock().unwrap().stream.reset(code);
    }
    fn send_id(&self) -> quic::StreamId {
        self.stream.lock().unwrap().stream.send_id()
    }
}

impl<S: quic::SendStreamUnframed<B>, B: Buf> quic::SendStreamUnframed<B> for SendStream<S, B> {
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, StreamErrorIncoming>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(terminated()));
        }
        state.writer = Some(cx.waker().clone());
        state.stream.poll_send(cx, buf)
    }
}

impl<S: quic::SendStreamUnframed<B>, B: Buf> tokio::io::AsyncWrite for SendStream<S, B> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        quic::SendStreamUnframed::poll_send(&mut *self, cx, &mut buf).map_err(std::io::Error::other)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        quic::SendStream::poll_ready(&mut *self, cx).map_err(std::io::Error::other)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        quic::SendStream::poll_finish(&mut *self, cx).map_err(std::io::Error::other)
    }
}

impl<S: quic::SendStreamUnframed<B>, B: Buf> futures_util::io::AsyncWrite for SendStream<S, B> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        quic::SendStreamUnframed::poll_send(&mut *self, cx, &mut buf).map_err(std::io::Error::other)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        quic::SendStream::poll_ready(&mut *self, cx).map_err(std::io::Error::other)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        quic::SendStream::poll_finish(&mut *self, cx).map_err(std::io::Error::other)
    }
}

/// WebTransport bidirectional stream.
///
/// Session termination resets the transport even when this handle is idle.
/// Split halves retain a shared transport. Explicitly finish the sending half
/// to send FIN while retaining the receiver.
pub struct BidiStream<S, B> {
    pub(crate) stream: SharedStream<S, B>,
}

impl<S, B> BidiStream<S, B> {
    pub(crate) fn new(stream: BufRecvStream<S, B>) -> Self {
        Self {
            stream: Arc::new(Mutex::new(StreamState {
                stream,
                closed: false,
                reader: None,
                writer: None,
            })),
        }
    }
}

impl<S, B> std::fmt::Debug for BidiStream<S, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BidiStream").finish_non_exhaustive()
    }
}

impl<S: quic::RecvStream, B> quic::RecvStream for BidiStream<S, B> {
    type Buf = Bytes;
    fn poll_data(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, StreamErrorIncoming>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(terminated()));
        }
        state.reader = Some(cx.waker().clone());
        state.stream.poll_data(cx)
    }
    fn stop_sending(&mut self, code: u64) {
        self.stream.lock().unwrap().stream.stop_sending(code);
    }
    fn recv_id(&self) -> quic::StreamId {
        self.stream.lock().unwrap().stream.recv_id()
    }
}

impl<S: quic::RecvStream, B: Buf> tokio::io::AsyncRead for BidiStream<S, B> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(io_terminated()));
        }
        state.reader = Some(cx.waker().clone());
        tokio::io::AsyncRead::poll_read(Pin::new(&mut state.stream), cx, buf)
    }
}

impl<S: quic::RecvStream, B: Buf> futures_util::io::AsyncRead for BidiStream<S, B> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(io_terminated()));
        }
        state.reader = Some(cx.waker().clone());
        futures_util::io::AsyncRead::poll_read(Pin::new(&mut state.stream), cx, buf)
    }
}

impl<S: quic::SendStream<B>, B: Buf> quic::SendStream<B> for BidiStream<S, B> {
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(terminated()));
        }
        state.writer = Some(cx.waker().clone());
        state.stream.poll_ready(cx)
    }
    fn send_data<T: Into<h3::stream::WriteBuf<B>>>(
        &mut self,
        data: T,
    ) -> Result<(), StreamErrorIncoming> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Err(terminated());
        }
        state.stream.send_data(data)
    }
    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(terminated()));
        }
        state.writer = Some(cx.waker().clone());
        state.stream.poll_finish(cx)
    }
    fn reset(&mut self, code: u64) {
        self.stream.lock().unwrap().stream.reset(code);
    }
    fn send_id(&self) -> quic::StreamId {
        self.stream.lock().unwrap().stream.send_id()
    }
}

impl<S: quic::SendStreamUnframed<B>, B: Buf> quic::SendStreamUnframed<B> for BidiStream<S, B> {
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, StreamErrorIncoming>> {
        let mut state = self.stream.lock().unwrap();
        if state.closed {
            return Poll::Ready(Err(terminated()));
        }
        state.writer = Some(cx.waker().clone());
        state.stream.poll_send(cx, buf)
    }
}

impl<S: quic::SendStreamUnframed<B>, B: Buf> tokio::io::AsyncWrite for BidiStream<S, B> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        quic::SendStreamUnframed::poll_send(&mut *self, cx, &mut buf).map_err(std::io::Error::other)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        quic::SendStream::poll_ready(&mut *self, cx).map_err(std::io::Error::other)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        quic::SendStream::poll_finish(&mut *self, cx).map_err(std::io::Error::other)
    }
}

impl<S: quic::SendStreamUnframed<B>, B: Buf> futures_util::io::AsyncWrite for BidiStream<S, B> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        quic::SendStreamUnframed::poll_send(&mut *self, cx, &mut buf).map_err(std::io::Error::other)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        quic::SendStream::poll_ready(&mut *self, cx).map_err(std::io::Error::other)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        quic::SendStream::poll_finish(&mut *self, cx).map_err(std::io::Error::other)
    }
}

impl<S: quic::SendStream<B> + quic::RecvStream, B: Buf> quic::BidiStream<B> for BidiStream<S, B> {
    type SendStream = SendStream<S, B>;
    type RecvStream = RecvStream<S, B>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        // Keep the reset capability shared with the session after splitting.
        (
            SendStream {
                stream: self.stream.clone(),
            },
            RecvStream {
                stream: self.stream,
            },
        )
    }
}
