use std::task::{Context, Poll};

use bytes::{Buf, BufMut as _, Bytes};
use futures_util::{future, ready};

use crate::{
    buf::BufList,
    error::{Code, ErrorLevel},
    frame::FrameStream,
    proto::{
        coding::{Decode as _, Encode},
        frame::Frame,
        stream::StreamType,
        varint::VarInt,
    },
    quic::{self, BidiStream, RecvStream, SendStream},
    webtransport::{self, SessionId},
    Error,
};

#[inline]
/// Transmits data by encoding in wire format.
pub(crate) async fn write<S, D, B>(stream: &mut S, data: D) -> Result<(), Error>
where
    S: SendStream,
    D: Into<WriteBuf<B>>,
    B: Buf,
{
    let mut write_buf = data.into();
    while write_buf.has_remaining() {
        future::poll_fn(|cx| stream.poll_send(cx, &mut write_buf)).await?;
    }

    Ok(())
}

const WRITE_BUF_ENCODE_SIZE: usize = StreamType::MAX_ENCODED_SIZE + Frame::MAX_ENCODED_SIZE;

/// Wrap frames to encode their header on the stack before sending them on the wire
///
/// Implements `Buf` so wire data is seamlessly available for transport layer transmits:
/// `Buf::chunk()` will yield the encoded header, then the payload. For unidirectional streams,
/// this type makes it possible to prefix wire data with the `StreamType`.
///
/// Conveying frames as `Into<WriteBuf>` makes it possible to encode only when generating wire-format
/// data is necessary (say, in `quic::SendStream::send_data`). It also has a public API ergonomy
/// advantage: `WriteBuf` doesn't have to appear in public associated types. On the other hand,
/// QUIC implementers have to call `into()`, which will encode the header in `Self::buf`.
pub struct WriteBuf<B>
where
    B: Buf,
{
    buf: [u8; WRITE_BUF_ENCODE_SIZE],
    len: usize,
    pos: usize,
    frame: Option<Frame<B>>,
}

impl<B> WriteBuf<B>
where
    B: Buf,
{
    fn encode_stream_type(&mut self, ty: StreamType) {
        let mut buf_mut = &mut self.buf[self.len..];
        ty.encode(&mut buf_mut);
        self.len = WRITE_BUF_ENCODE_SIZE - buf_mut.remaining_mut();
    }

    fn encode_frame_header(&mut self) {
        if let Some(frame) = self.frame.as_ref() {
            let mut buf_mut = &mut self.buf[self.len..];
            frame.encode(&mut buf_mut);
            self.len = WRITE_BUF_ENCODE_SIZE - buf_mut.remaining_mut();
        }
    }
}

impl<B> From<StreamType> for WriteBuf<B>
where
    B: Buf,
{
    fn from(ty: StreamType) -> Self {
        let mut me = Self {
            buf: [0; WRITE_BUF_ENCODE_SIZE],
            len: 0,
            pos: 0,
            frame: None,
        };
        me.encode_stream_type(ty);
        me
    }
}

impl<B> From<Frame<B>> for WriteBuf<B>
where
    B: Buf,
{
    fn from(frame: Frame<B>) -> Self {
        let mut me = Self {
            buf: [0; WRITE_BUF_ENCODE_SIZE],
            len: 0,
            pos: 0,
            frame: Some(frame),
        };
        me.encode_frame_header();
        me
    }
}

impl<B> From<(StreamType, Frame<B>)> for WriteBuf<B>
where
    B: Buf,
{
    fn from(ty_stream: (StreamType, Frame<B>)) -> Self {
        let (ty, frame) = ty_stream;
        let mut me = Self {
            buf: [0; WRITE_BUF_ENCODE_SIZE],
            len: 0,
            pos: 0,
            frame: Some(frame),
        };
        me.encode_stream_type(ty);
        me.encode_frame_header();
        me
    }
}

impl<B> Buf for WriteBuf<B>
where
    B: Buf,
{
    fn remaining(&self) -> usize {
        self.len - self.pos
            + self
                .frame
                .as_ref()
                .and_then(|f| f.payload())
                .map_or(0, |x| x.remaining())
    }

    fn chunk(&self) -> &[u8] {
        if self.len - self.pos > 0 {
            &self.buf[self.pos..self.len]
        } else if let Some(payload) = self.frame.as_ref().and_then(|f| f.payload()) {
            payload.chunk()
        } else {
            &[]
        }
    }

    fn advance(&mut self, mut cnt: usize) {
        let remaining_header = self.len - self.pos;
        if remaining_header > 0 {
            let advanced = usize::min(cnt, remaining_header);
            self.pos += advanced;
            cnt -= advanced;
        }

        if let Some(payload) = self.frame.as_mut().and_then(|f| f.payload_mut()) {
            payload.advance(cnt);
        }
    }
}

pub(super) enum AcceptedRecvStream<S>
where
    S: quic::RecvStream,
{
    Control(FrameStream<S>),
    Push(u64, FrameStream<S>),
    Encoder(BufRecvStream<S>),
    Decoder(BufRecvStream<S>),
    WebTransportUni(SessionId, webtransport::stream::RecvStream<S>),
    Reserved,
}

impl<S> AcceptedRecvStream<S>
where
    S: quic::RecvStream,
{
    /// Returns `true` if the accepted recv stream is [`WebTransportUni`].
    ///
    /// [`WebTransportUni`]: AcceptedRecvStream::WebTransportUni
    #[must_use]
    pub fn is_web_transport_uni(&self) -> bool {
        matches!(self, Self::WebTransportUni(..))
    }
}

/// Resolves an incoming streams type as well as `PUSH_ID`s and `SESSION_ID`s
pub(super) struct AcceptRecvStream<S> {
    stream: BufRecvStream<S>,
    ty: Option<StreamType>,
    /// push_id or session_id
    id: Option<VarInt>,
    expected: Option<usize>,
}

impl<S> AcceptRecvStream<S>
where
    S: RecvStream,
{
    pub fn new(stream: S) -> Self {
        Self {
            stream: BufRecvStream::new(stream),
            ty: None,
            id: None,
            expected: None,
        }
    }

    pub fn into_stream(self) -> Result<AcceptedRecvStream<S>, Error> {
        Ok(match self.ty.expect("Stream type not resolved yet") {
            StreamType::CONTROL => AcceptedRecvStream::Control(FrameStream::new(self.stream)),
            StreamType::PUSH => AcceptedRecvStream::Push(
                self.id.expect("Push ID not resolved yet").into_inner(),
                FrameStream::new(self.stream),
            ),
            StreamType::ENCODER => AcceptedRecvStream::Encoder(self.stream),
            StreamType::DECODER => AcceptedRecvStream::Decoder(self.stream),
            StreamType::WEBTRANSPORT_UNI => AcceptedRecvStream::WebTransportUni(
                SessionId::from_varint(self.id.expect("Session ID not resolved yet")),
                webtransport::stream::RecvStream::new(self.stream),
            ),
            t if t.value() > 0x21 && (t.value() - 0x21) % 0x1f == 0 => AcceptedRecvStream::Reserved,

            //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2
            //# Recipients of unknown stream types MUST
            //# either abort reading of the stream or discard incoming data without
            //# further processing.

            //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2
            //# If reading is aborted, the recipient SHOULD use
            //# the H3_STREAM_CREATION_ERROR error code or a reserved error code
            //# (Section 8.1).

            //= https://www.rfc-editor.org/rfc/rfc9114#section-6.2
            //= type=implication
            //# The recipient MUST NOT consider unknown stream types
            //# to be a connection error of any kind.
            t => {
                return Err(Code::H3_STREAM_CREATION_ERROR.with_reason(
                    format!("unknown stream type 0x{:x}", t.value()),
                    crate::error::ErrorLevel::ConnectionError,
                ))
            }
        })
    }

    pub fn poll_type(&mut self, cx: &mut Context) -> Poll<Result<(), Error>> {
        loop {
            // Return if all identification data is met
            match self.ty {
                Some(StreamType::PUSH | StreamType::WEBTRANSPORT_UNI) => {
                    if self.id.is_some() {
                        return Poll::Ready(Ok(()));
                    }
                }
                Some(_) => return Poll::Ready(Ok(())),
                None => (),
            };

            if ready!(self.stream.poll_read(cx))? {
                return Poll::Ready(Err(Code::H3_STREAM_CREATION_ERROR.with_reason(
                    "Stream closed before type received",
                    ErrorLevel::ConnectionError,
                )));
            };

            let mut buf = self.stream.buf_mut();
            if self.expected.is_none() && buf.remaining() >= 1 {
                self.expected = Some(VarInt::encoded_size(buf.chunk()[0]));
            }

            if let Some(expected) = self.expected {
                // Poll for more data
                if buf.remaining() < expected {
                    continue;
                }
            } else {
                continue;
            }

            // Parse ty and then id
            if self.ty.is_none() {
                // Parse StreamType
                self.ty = Some(StreamType::decode(&mut buf).map_err(|_| {
                    Code::H3_INTERNAL_ERROR.with_reason(
                        "Unexpected end parsing stream type",
                        ErrorLevel::ConnectionError,
                    )
                })?);
                // Get the next VarInt for PUSH_ID on the next iteration
                self.expected = None;
            } else {
                // Parse PUSH_ID
                self.id = Some(VarInt::decode(&mut buf).map_err(|_| {
                    Code::H3_INTERNAL_ERROR.with_reason(
                        "Unexpected end parsing push or session id",
                        ErrorLevel::ConnectionError,
                    )
                })?);
            }
        }
    }
}

/// A stream which allows partial reading of the data without data loss.
///
/// This fixes the problem where `poll_data` returns more than the needed amount of bytes,
/// requiring correct implementations to hold on to that extra data and return it later.
///
/// # Usage
///
/// Implements `quic::RecvStream` which will first return buffered data, and then read from the
/// stream
pub(crate) struct BufRecvStream<S> {
    buf: BufList<Bytes>,
    /// Indicates that the end of the stream has been reached
    ///
    /// Data may still be available as buffered
    eos: bool,
    stream: S,
}

impl<S: RecvStream> BufRecvStream<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            buf: BufList::new(),
            eos: false,
            stream,
        }
    }

    pub(crate) fn with_bufs(stream: S, bufs: BufList<Bytes>) -> Self {
        Self {
            buf: bufs,
            eos: false,
            stream,
        }
    }

    /// Reads more data into the buffer, returning the number of bytes read.
    ///
    /// Returns `true` if the end of the stream is reached.
    pub fn poll_read(&mut self, cx: &mut Context<'_>) -> Poll<Result<bool, S::Error>> {
        let data = ready!(self.stream.poll_data(cx))?;

        if let Some(mut data) = data {
            self.buf.push_bytes(&mut data);
            Poll::Ready(Ok(false))
        } else {
            self.eos = true;
            Poll::Ready(Ok(true))
        }
    }

    /// Returns the currently buffered data, allowing it to be partially read
    #[inline]
    pub fn buf_mut(&mut self) -> &mut BufList<Bytes> {
        &mut self.buf
    }

    #[inline]
    pub(crate) fn buf(&self) -> &BufList<Bytes> {
        &self.buf
    }

    pub fn is_eos(&self) -> bool {
        self.eos
    }
}

impl<S: RecvStream> RecvStream for BufRecvStream<S> {
    type Buf = Bytes;

    type Error = S::Error;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        // There is data buffered, return that immediately
        if let Some(chunk) = self.buf.take_first_chunk() {
            return Poll::Ready(Ok(Some(chunk)));
        }

        if let Some(mut data) = ready!(self.stream.poll_data(cx))? {
            Poll::Ready(Ok(Some(data.copy_to_bytes(data.remaining()))))
        } else {
            self.eos = true;
            Poll::Ready(Ok(None))
        }
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.stream.stop_sending(error_code)
    }

    fn recv_id(&self) -> quic::StreamId {
        self.stream.recv_id()
    }
}

impl<S: SendStream> SendStream for BufRecvStream<S> {
    type Error = S::Error;

    #[inline]
    fn poll_send<D: Buf>(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut D,
    ) -> Poll<Result<usize, Self::Error>> {
        self.stream.poll_send(cx, buf)
    }

    fn poll_finish(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.stream.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.stream.reset(reset_code)
    }

    fn send_id(&self) -> quic::StreamId {
        self.stream.send_id()
    }
}

impl<S: BidiStream> BidiStream for BufRecvStream<S> {
    type SendStream = BufRecvStream<S::SendStream>;

    type RecvStream = BufRecvStream<S::RecvStream>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        let (send, recv) = self.stream.split();
        (
            BufRecvStream {
                // Sending is not buffered
                buf: BufList::new(),
                eos: self.eos,
                stream: send,
            },
            BufRecvStream {
                buf: self.buf,
                eos: self.eos,
                stream: recv,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_buf_encode_streamtype() {
        let wbuf = WriteBuf::<Bytes>::from(StreamType::ENCODER);

        assert_eq!(wbuf.chunk(), b"\x02");
        assert_eq!(wbuf.len, 1);
    }

    #[test]
    fn write_buf_encode_frame() {
        let wbuf = WriteBuf::<Bytes>::from(Frame::Goaway(VarInt(2)));

        assert_eq!(wbuf.chunk(), b"\x07\x01\x02");
        assert_eq!(wbuf.len, 3);
    }

    #[test]
    fn write_buf_encode_streamtype_then_frame() {
        let wbuf = WriteBuf::<Bytes>::from((StreamType::ENCODER, Frame::Goaway(VarInt(2))));

        assert_eq!(wbuf.chunk(), b"\x02\x07\x01\x02");
    }

    #[test]
    fn write_buf_advances() {
        let mut wbuf =
            WriteBuf::<Bytes>::from((StreamType::ENCODER, Frame::Data(Bytes::from("hey"))));

        assert_eq!(wbuf.chunk(), b"\x02\x00\x03");
        wbuf.advance(3);
        assert_eq!(wbuf.remaining(), 3);
        assert_eq!(wbuf.chunk(), b"hey");
        wbuf.advance(2);
        assert_eq!(wbuf.chunk(), b"y");
        wbuf.advance(1);
        assert_eq!(wbuf.remaining(), 0);
    }

    #[test]
    fn write_buf_advance_jumps_header_and_payload_start() {
        let mut wbuf =
            WriteBuf::<Bytes>::from((StreamType::ENCODER, Frame::Data(Bytes::from("hey"))));

        wbuf.advance(4);
        assert_eq!(wbuf.chunk(), b"ey");
    }
}
