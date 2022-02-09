use async_trait::async_trait;
use bytes::{Buf, Bytes};
use tracing::trace;

use crate::{
    buf::BufList,
    error::TransportError,
    proto::{
        frame::{self, Frame, PayloadLen},
        stream::StreamId,
    },
    quic::{RecvStream, SendStream},
    stream::WriteBuf,
};

pub struct FrameStream<S>
where
    S: RecvStream,
{
    stream: S,
    bufs: BufList<Bytes>,
    decoder: FrameDecoder,
    remaining_data: usize,
    /// Set to true when `stream` reaches the end.
    is_eos: bool,
}

impl<S> FrameStream<S>
where
    S: RecvStream,
{
    pub fn new(stream: S) -> Self {
        Self::with_bufs(stream, BufList::new())
    }

    pub(crate) fn with_bufs(stream: S, bufs: BufList<Bytes>) -> Self {
        Self {
            stream,
            bufs,
            decoder: FrameDecoder::default(),
            remaining_data: 0,
            is_eos: false,
        }
    }

    pub async fn next(&mut self) -> Result<Option<Frame<PayloadLen>>, Error> {
        assert!(
            self.remaining_data == 0,
            "There is still data to read, please call poll_data() until it returns None."
        );

        loop {
            let end = self.try_recv().await?;

            return match self.decoder.decode(&mut self.bufs)? {
                Some(Frame::Data(PayloadLen(len))) => {
                    self.remaining_data = len;
                    Ok(Some(Frame::Data(PayloadLen(len))))
                }
                Some(frame) => Ok(Some(frame)),
                None => match end {
                    // Received a chunk but frame is incomplete, poll until we get `Pending`.
                    false => continue,
                    true => {
                        if self.bufs.has_remaining() {
                            // Reached the end of receive stream, but there is still some data:
                            // The frame is incomplete.
                            Err(Error::UnexpectedEnd)
                        } else {
                            Ok(None)
                        }
                    }
                },
            };
        }
    }

    pub async fn get_data(&mut self) -> Result<Option<impl Buf>, Error> {
        if self.remaining_data == 0 {
            return Ok(None);
        };

        while !self.try_recv().await? {}
        let data = self.bufs.take_chunk(self.remaining_data as usize);

        match data {
            None => Ok(None),
            Some(d) if d.remaining() < self.remaining_data && !self.bufs.has_remaining() => {
                Err(Error::UnexpectedEnd)
            }
            Some(d) => {
                self.remaining_data -= d.remaining();
                Ok(Some(d))
            }
        }
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

    async fn try_recv(&mut self) -> Result<bool, Error> {
        if self.is_eos {
            return Ok(true);
        }
        match self.stream.get_data().await {
            Err(e) => Err(Error::Quic(e.into())),
            Ok(None) => {
                self.is_eos = true;
                Ok(true)
            }
            Ok(Some(mut d)) => {
                self.bufs.push_bytes(&mut d);
                Ok(false)
            }
        }
    }
}

#[async_trait]
impl<T, B> SendStream<B> for FrameStream<T>
where
    T: SendStream<B> + RecvStream + Send,
    B: Buf,
{
    type Error = <T as SendStream<B>>::Error;

    async fn ready(&mut self) -> Result<(), Self::Error> {
        self.stream.ready().await
    }

    fn send_data<D: Into<WriteBuf<B>>>(&mut self, data: D) -> Result<(), Self::Error> {
        self.stream.send_data(data)
    }

    async fn finish(&mut self) -> Result<(), Self::Error> {
        self.stream.finish().await
    }

    fn reset(&mut self, reset_code: u64) {
        self.stream.reset(reset_code)
    }

    fn id(&self) -> StreamId {
        self.stream.id()
    }
}

#[derive(Default)]
pub struct FrameDecoder {
    expected: Option<usize>,
}

impl FrameDecoder {
    fn decode<B: Buf>(&mut self, src: &mut BufList<B>) -> Result<Option<Frame<PayloadLen>>, Error> {
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

            let (pos, decoded) = {
                let mut cur = src.cursor();
                let decoded = Frame::decode(&mut cur);
                (cur.position() as usize, decoded)
            };

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
    use std::{collections::VecDeque, fmt, sync::Arc};

    use assert_matches::assert_matches;
    use bytes::{BufMut, BytesMut};

    use crate::{
        proto::{coding::Encode, frame::FrameType, varint::VarInt},
        quic,
    };

    use super::*;

    // Decoder

    #[test]
    fn one_frame() {
        let mut buf = BytesMut::with_capacity(16);
        Frame::headers(&b"salut"[..]).encode_with_payload(&mut buf);
        let mut buf = BufList::from(buf);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf), Ok(Some(Frame::Headers(_))));
    }

    #[test]
    fn incomplete_frame() {
        let frame = Frame::headers(&b"salut"[..]);

        let mut buf = BytesMut::with_capacity(16);
        frame.encode(&mut buf);
        buf.truncate(buf.len() - 1);
        let mut buf = BufList::from(buf);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf), Ok(None));
    }

    #[test]
    fn header_spread_multiple_buf() {
        let mut buf = BytesMut::with_capacity(16);
        Frame::headers(&b"salut"[..]).encode_with_payload(&mut buf);
        let mut buf_list = BufList::new();
        // Cut buffer between type and length
        buf_list.push(&buf[..1]);
        buf_list.push(&buf[1..]);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf_list), Ok(Some(Frame::Headers(_))));
    }

    #[test]
    fn varint_spread_multiple_buf() {
        let mut buf = BytesMut::with_capacity(16);
        Frame::headers("salut".repeat(1024)).encode_with_payload(&mut buf);

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
        Frame::headers(&b"header"[..]).encode_with_payload(&mut buf);
        Frame::Data(&b"body"[..]).encode_with_payload(&mut buf);
        Frame::headers(&b"trailer"[..]).encode_with_payload(&mut buf);

        buf.truncate(buf.len() - 1);
        let mut buf = BufList::from(buf);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf), Ok(Some(Frame::Headers(_))));
        assert_matches!(
            decoder.decode(&mut buf),
            Ok(Some(Frame::Data(PayloadLen(4))))
        );
        assert_matches!(decoder.decode(&mut buf), Ok(None));
    }

    // FrameStream

    #[tokio::test]
    async fn poll_full_request() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        Frame::headers(&b"header"[..]).encode_with_payload(&mut buf);
        Frame::Data(&b"body"[..]).encode_with_payload(&mut buf);
        Frame::headers(&b"trailer"[..]).encode_with_payload(&mut buf);
        recv.chunk(buf.freeze());

        let mut stream = FrameStream::new(recv);

        assert_matches!(stream.next().await, Ok(Some(Frame::Headers(_))));
        assert_matches!(stream.next().await, Ok(Some(Frame::Data(PayloadLen(4)))));
        assert_matches!(
            to_bytes(stream.get_data().await),
            Ok(Some(b)) if b.remaining() == 4
        );
        assert_matches!(stream.next().await, Ok(Some(Frame::Headers(_))));
    }

    #[tokio::test]
    async fn poll_next_incomplete_frame() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        Frame::headers(&b"header"[..]).encode_with_payload(&mut buf);
        let mut buf = buf.freeze();
        recv.chunk(buf.split_to(buf.len() - 1));
        let mut stream = FrameStream::new(recv);

        assert_matches!(stream.next().await, Err(Error::UnexpectedEnd));
    }

    #[tokio::test]
    #[should_panic(
        expected = "There is still data to read, please call poll_data() until it returns None"
    )]
    async fn poll_next_reamining_data() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        FrameType::DATA.encode(&mut buf);
        VarInt::from(4u32).encode(&mut buf);
        recv.chunk(buf.freeze());
        let mut stream = FrameStream::new(recv);

        assert_matches!(stream.next().await, Ok(Some(Frame::Data(PayloadLen(4)))));

        // There is still data to consume, poll_next should panic
        let _ = stream.next().await;
    }

    #[tokio::test]
    async fn poll_data_split() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        // Body is split into two bufs
        Frame::Data(Bytes::from("body")).encode_with_payload(&mut buf);

        let mut buf = buf.freeze();
        recv.chunk(buf.split_to(buf.len() - 2));
        recv.chunk(buf);
        let mut stream = FrameStream::new(recv);

        // We get the total size of data about to be received
        assert_matches!(stream.next().await, Ok(Some(Frame::Data(PayloadLen(4)))));

        // Then we get parts of body, chunked as they arrived
        assert_matches!(
            to_bytes(stream.get_data().await),
            Ok(Some(b)) if b.remaining() == 2
        );
        assert_matches!(
            to_bytes(stream.get_data().await),
            Ok(Some(b)) if b.remaining() == 2
        );
    }

    #[tokio::test]
    async fn poll_data_unexpected_end() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        // Truncated body
        FrameType::DATA.encode(&mut buf);
        VarInt::from(4u32).encode(&mut buf);
        buf.put_slice(&b"b"[..]);
        recv.chunk(buf.freeze());
        let mut stream = FrameStream::new(recv);

        assert_matches!(stream.next().await, Ok(Some(Frame::Data(PayloadLen(4)))));
        assert_matches!(to_bytes(stream.get_data().await), Err(Error::UnexpectedEnd));
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
        Frame::Data(Bytes::from("body")).encode_with_payload(&mut buf);

        recv.chunk(buf.freeze());
        let mut stream = FrameStream::new(recv);

        assert_matches!(stream.next().await, Ok(Some(Frame::Data(PayloadLen(4)))));
        assert_matches!(
            to_bytes(stream.get_data().await),
            Ok(Some(b)) if &*b == b"body"
        );
    }

    #[tokio::test]
    async fn poll_data_eos_but_buffered_data() {
        let mut recv = FakeRecv::default();
        let mut buf = BytesMut::with_capacity(64);

        FrameType::DATA.encode(&mut buf);
        VarInt::from(4u32).encode(&mut buf);
        buf.put_slice(&b"bo"[..]);
        recv.chunk(buf.clone().freeze());

        let mut stream = FrameStream::new(recv);

        assert_matches!(stream.next().await, Ok(Some(Frame::Data(PayloadLen(4)))));

        buf.truncate(0);
        buf.put_slice(&b"dy"[..]);
        stream.bufs.push_bytes(&mut buf.freeze());

        assert_matches!(
            to_bytes(stream.get_data().await),
            Ok(Some(b)) if &*b == b"bo"
        );

        assert_matches!(
            to_bytes(stream.get_data().await),
            Ok(Some(b)) if &*b == b"dy"
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

    #[async_trait]
    impl RecvStream for FakeRecv {
        type Buf = Bytes;
        type Error = FakeError;

        async fn get_data(&mut self) -> Result<Option<Self::Buf>, Self::Error> {
            Ok(self.chunks.pop_front())
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

    fn to_bytes(x: Result<Option<impl Buf>, Error>) -> Result<Option<Bytes>, Error> {
        x.map(|b| b.map(|mut b| b.copy_to_bytes(b.remaining())))
    }
}
