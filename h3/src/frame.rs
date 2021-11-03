use std::task::{Context, Poll};

use bytes::{Buf, Bytes};

use tracing::trace;

use crate::{
    buf::BufList,
    error::TransportError,
    proto::frame::{self, Frame},
    quic::{RecvStream, SendStream},
};

pub struct FrameStream<S>
where
    S: RecvStream,
{
    stream: S,
    bufs: BufList<S::Buf>,
    decoder: FrameDecoder,
    remaining_data: u64,
    /// Set to true when `stream` reaches the end.
    is_eos: bool,
}

impl<S: RecvStream> FrameStream<S> {
    pub fn new(stream: S) -> Self {
        Self::with_bufs(stream, BufList::new())
    }

    pub(crate) fn with_bufs(stream: S, bufs: BufList<S::Buf>) -> Self {
        Self {
            stream,
            bufs,
            decoder: FrameDecoder::default(),
            remaining_data: 0,
            is_eos: false,
        }
    }

    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Frame>, Error>> {
        assert!(
            self.remaining_data == 0,
            "There is still data to read, please call poll_data() until it returns None."
        );

        loop {
            let end = self.try_recv(cx)?;

            return match self.decoder.decode(&mut self.bufs)? {
                Some(Frame::Data { len }) => {
                    self.remaining_data = len;
                    Poll::Ready(Ok(Some(Frame::Data { len })))
                }
                Some(frame) => Poll::Ready(Ok(Some(frame))),
                None => match end {
                    // Recieved a chunk but frame is incomplete, poll until we get `Pending`.
                    Poll::Ready(false) => continue,
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(true) => {
                        if self.bufs.has_remaining() {
                            // Reached the end of recieve stream, but there is still some data:
                            // The frame is incomplete.
                            Poll::Ready(Err(Error::UnexpectedEnd))
                        } else {
                            Poll::Ready(Ok(None))
                        }
                    }
                },
            };
        }
    }

    pub fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Bytes>, Error>> {
        if self.remaining_data == 0 {
            return Poll::Ready(Ok(None));
        };

        let end = match self.try_recv(cx)? {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(end) => end,
        };

        let bufs_len = self.bufs.remaining();

        let data = if (bufs_len as u64) <= self.remaining_data {
            self.bufs.copy_to_bytes(bufs_len)
        } else {
            self.bufs.copy_to_bytes(self.remaining_data as usize)
        };

        match (data.len(), end) {
            (0, true) => return Poll::Ready(Ok(None)),
            (0, false) => return Poll::Pending,
            (x, true) if (x as u64) < self.remaining_data => {
                return Poll::Ready(Err(Error::UnexpectedEnd));
            }
            (x, _) => self.remaining_data -= x as u64,
        };

        Poll::Ready(Ok(Some(data)))
    }

    pub(crate) fn stop_sending(&mut self, error_code: crate::error::Code) {
        let _ = self.stream.stop_sending(error_code.into());
    }

    pub(crate) fn has_data(&self) -> bool {
        self.remaining_data != 0
    }

    pub(crate) fn is_eos(&self) -> bool {
        self.is_eos && !self.bufs.has_remaining()
    }

    fn try_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<bool, Error>> {
        if self.is_eos {
            return Poll::Ready(Ok(true));
        }
        match self.stream.poll_data(cx) {
            Poll::Ready(Err(e)) => Poll::Ready(Err(Error::Quic(e.into()))),
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(None)) => {
                self.is_eos = true;
                Poll::Ready(Ok(true))
            }
            Poll::Ready(Ok(Some(d))) => {
                self.bufs.push(d);
                Poll::Ready(Ok(false))
            }
        }
    }
}

impl<T, B> SendStream<B> for FrameStream<T>
where
    T: SendStream<B> + RecvStream,
    B: Buf,
{
    type Error = <T as SendStream<B>>::Error;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.stream.poll_ready(cx)
    }

    fn send_data(&mut self, data: B) -> Result<(), Self::Error> {
        self.stream.send_data(data)
    }

    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.stream.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.stream.reset(reset_code)
    }

    fn id(&self) -> u64 {
        self.stream.id()
    }
}

#[derive(Default)]
pub struct FrameDecoder {
    expected: Option<usize>,
}

macro_rules! decode {
    ($buf:ident, $dec:expr) => {{
        let mut cur = $buf.cursor();
        let decoded = $dec(&mut cur);
        (cur.position() as usize, decoded)
    }};
}

impl FrameDecoder {
    fn decode<B: Buf>(&mut self, src: &mut BufList<B>) -> Result<Option<Frame>, Error> {
        // Decode in a loop since we ignore unknown frames, and there may be
        // other frames already in our BufList.
        loop {
            if !src.has_remaining() {
                return Ok(None);
            }

            if let Some(min) = self.expected {
                if src.remaining() < min {
                    return Ok(None);
                }
            }

            let (pos, decoded) = decode!(src, |cur| Frame::decode(cur));

            match decoded {
                Err(frame::Error::UnknownFrame(ty)) => {
                    trace!("ignore unknown frame type {:#x}", ty);
                    src.advance(pos);
                    self.expected = None;
                    continue;
                }
                Err(frame::Error::Incomplete(min)) => {
                    self.expected = Some(min);
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
                Ok(frame) => {
                    src.advance(pos);
                    self.expected = None;
                    return Ok(Some(frame));
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Proto(frame::Error),
    Quic(TransportError),
    UnexpectedEnd,
}

impl From<frame::Error> for Error {
    fn from(err: frame::Error) -> Self {
        Error::Proto(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use bytes::{BufMut, BytesMut};
    use futures::future::poll_fn;
    use std::{collections::VecDeque, fmt, sync::Arc};
    use tokio;

    use crate::{proto::coding::Encode, quic};

    // Decoder

    #[test]
    fn one_frame() {
        let frame = Frame::Headers(b"salut"[..].into());

        let mut buf = BytesMut::with_capacity(16);
        frame.encode(&mut buf);
        let mut buf = BufList::from(buf);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf), Ok(Some(Frame::Headers(_))));
    }

    #[test]
    fn incomplete_frame() {
        let frame = Frame::Headers(b"salut"[..].into());

        let mut buf = BytesMut::with_capacity(16);
        frame.encode(&mut buf);
        buf.truncate(buf.len() - 1);
        let mut buf = BufList::from(buf);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf), Ok(None));
    }

    #[test]
    fn header_spread_multiple_buf() {
        let frame = Frame::Headers(b"salut"[..].into());

        let mut buf = BytesMut::with_capacity(16);
        frame.encode(&mut buf);
        let mut buf_list = BufList::new();
        // Cut buffer between type and length
        buf_list.push(&buf[..1]);
        buf_list.push(&buf[1..]);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf_list), Ok(Some(Frame::Headers(_))));
    }

    #[test]
    fn varint_spread_multiple_buf() {
        let payload = "salut".repeat(1024);
        let frame = Frame::Headers(payload.into());

        let mut buf = BytesMut::with_capacity(16);
        frame.encode(&mut buf);
        let mut buf_list = BufList::new();
        // Cut buffer in the middle of length's varint
        buf_list.push(&buf[..2]);
        buf_list.push(&buf[2..]);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf_list), Ok(Some(Frame::Headers(_))));
    }

    #[test]
    fn two_frames_then_incomplete() {
        let mut buf = BytesMut::with_capacity(64);
        Frame::Headers(b"header"[..].into()).encode(&mut buf);
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(&b"body"[..]);
        Frame::Headers(b"trailer"[..].into()).encode(&mut buf);

        buf.truncate(buf.len() - 1);
        let mut buf = BufList::from(buf);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf), Ok(Some(Frame::Headers(_))));
        assert_matches!(decoder.decode(&mut buf), Ok(Some(Frame::Data { len: 4 })));
        assert_matches!(decoder.decode(&mut buf), Ok(None));
    }

    // FrameStream

    macro_rules! assert_poll_matches {
        ($poll_fn:expr, $match:pat) => {
            assert_matches!(
                poll_fn($poll_fn).await,
                $match
            );
        };
        ($poll_fn:expr, $match:pat if $cond:expr ) => {
            assert_matches!(
                poll_fn($poll_fn).await,
                $match if $cond
            );
        }
    }

    #[tokio::test]
    async fn poll_full_request() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        Frame::Headers(b"header"[..].into()).encode(&mut buf);
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(&b"body"[..]);
        Frame::Headers(b"trailer"[..].into()).encode(&mut buf);
        recv.chunk(buf.freeze());
        let mut stream = FrameStream::new(recv);

        assert_poll_matches!(
            |mut cx| stream.poll_next(&mut cx),
            Ok(Some(Frame::Headers(_)))
        );
        assert_poll_matches!(
            |mut cx| stream.poll_next(&mut cx),
            Ok(Some(Frame::Data { len: 4 }))
        );
        assert_poll_matches!(
            |mut cx| stream.poll_data(&mut cx),
            Ok(Some(b)) if b.remaining() == 4
        );
        assert_poll_matches!(
            |mut cx| stream.poll_next(&mut cx),
            Ok(Some(Frame::Headers(_)))
        );
    }

    #[tokio::test]
    async fn poll_next_incomplete_frame() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        Frame::Headers(b"header"[..].into()).encode(&mut buf);
        let mut buf = buf.freeze();
        recv.chunk(buf.split_to(buf.len() - 1));
        let mut stream = FrameStream::new(recv);

        assert_poll_matches!(
            |mut cx| stream.poll_next(&mut cx),
            Err(Error::UnexpectedEnd)
        );
    }

    #[tokio::test]
    #[should_panic(
        expected = "There is still data to read, please call poll_data() until it returns None"
    )]
    async fn poll_next_reamining_data() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        Frame::Data { len: 4 }.encode(&mut buf);
        recv.chunk(buf.freeze());
        let mut stream = FrameStream::new(recv);

        assert_poll_matches!(
            |mut cx| stream.poll_next(&mut cx),
            Ok(Some(Frame::Data { len: 4 }))
        );

        // There is still data to consume, poll_next should panic
        let _ = poll_fn(|mut cx| stream.poll_next(&mut cx)).await;
    }

    #[tokio::test]
    async fn poll_data_split() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        // Body is split into two bufs
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(&b"body"[..]);
        let mut buf = buf.freeze();
        recv.chunk(buf.split_to(buf.len() - 2));
        recv.chunk(buf);
        let mut stream = FrameStream::new(recv);

        assert_poll_matches!(
            |mut cx| stream.poll_next(&mut cx),
            Ok(Some(Frame::Data { len: 4 }))
        );
        assert_poll_matches!(
            |mut cx| stream.poll_data(&mut cx),
            Ok(Some(b)) if b.remaining() == 4
        );
    }

    #[tokio::test]
    async fn poll_data_unexpected_end() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        // Truncated body
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(&b"b"[..]);
        recv.chunk(buf.freeze());
        let mut stream = FrameStream::new(recv);

        assert_poll_matches!(
            |mut cx| stream.poll_next(&mut cx),
            Ok(Some(Frame::Data { len: 4 }))
        );
        assert_poll_matches!(
            |mut cx| stream.poll_data(&mut cx),
            Err(Error::UnexpectedEnd)
        );
    }

    #[tokio::test]
    async fn poll_data_ignores_unknown_frames() {
        use crate::proto::varint::BufMutExt as _;

        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        // grease a lil
        crate::proto::frame::FrameType::RESERVED.encode(&mut buf);
        buf.write_var(0);

        // grease with some data
        crate::proto::frame::FrameType::RESERVED.encode(&mut buf);
        buf.write_var(6);
        buf.put_slice(b"grease");

        // Body
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"body");
        recv.chunk(buf.freeze());
        let mut stream = FrameStream::new(recv);

        assert_poll_matches!(
            |mut cx| stream.poll_next(&mut cx),
            Ok(Some(Frame::Data { len: 4 }))
        );
        assert_poll_matches!(
            |mut cx| stream.poll_data(&mut cx),
            Ok(Some(b)) if &*b == b"body"
        );
    }

    // Helpers

    #[derive(Default)]
    struct FakeRecv {
        chunks: VecDeque<Bytes>,
    }

    impl FakeRecv {
        fn chunk(&mut self, buf: Bytes) -> &mut Self {
            self.chunks.push_back(buf.into());
            self
        }
    }

    impl RecvStream for FakeRecv {
        type Buf = Bytes;
        type Error = FakeError;

        fn poll_data(
            &mut self,
            _: &mut Context<'_>,
        ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
            Poll::Ready(Ok(self.chunks.pop_front()))
        }

        fn stop_sending(&mut self, _: u64) {
            unimplemented!()
        }
    }

    #[derive(Debug)]
    struct FakeError;

    impl quic::Error for FakeError {
        fn is_timeout(&self) -> bool {
            unimplemented!()
        }

        fn err_code(&self) -> Option<u64> {
            unimplemented!()
        }
    }

    impl std::error::Error for FakeError {}
    impl fmt::Display for FakeError {
        fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
            unimplemented!()
        }
    }

    impl Into<Arc<dyn quic::Error>> for FakeError {
        fn into(self) -> Arc<dyn quic::Error> {
            unimplemented!()
        }
    }
}
