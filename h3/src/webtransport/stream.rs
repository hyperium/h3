use std::task::Poll;

use bytes::{Buf, Bytes};
use futures_util::ready;

use crate::{buf::BufList, quic};

/// WebTransport receive stream
pub struct RecvStream<S> {
    buf: BufList<Bytes>,
    stream: S,
}

impl<S> RecvStream<S> {
    pub(crate) fn new(buf: BufList<Bytes>, stream: S) -> Self {
        Self { buf, stream }
    }
}

impl<S> quic::RecvStream for RecvStream<S>
where
    S: quic::RecvStream,
{
    type Buf = Bytes;

    type Error = S::Error;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        match ready!(self.stream.poll_data(cx)?) {
            Some(mut buf) => self.buf.push_bytes(&mut buf),
            None => return Poll::Ready(Ok(None)),
        }

        if let Some(chunk) = self.buf.take_first_chunk() {
            if chunk.remaining() > 0 {
                return Poll::Ready(Ok(Some(chunk)));
            }
        }

        Poll::Pending
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.stream.stop_sending(error_code)
    }

    fn recv_id(&self) -> quic::StreamId {
        self.stream.recv_id()
    }
}
