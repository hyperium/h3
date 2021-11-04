use std::task::{Context, Poll};

use bytes::{Buf, Bytes, BytesMut};
use futures::{future, ready};
use quic::RecvStream;

use crate::{
    buf::BufList,
    error::Code,
    frame::FrameStream,
    proto::{
        coding::{BufExt, Decode as _, Encode},
        stream::StreamType,
        varint::VarInt,
    },
    quic::{self, SendStream},
    Error,
};

pub(crate) async fn write<S, T>(stream: &mut S, data: T) -> Result<(), Error>
where
    S: SendStream<Bytes>,
    T: Encode,
{
    let mut buf = BytesMut::new();
    data.encode(&mut buf);

    stream.send_data(buf.freeze())?;
    future::poll_fn(|cx| stream.poll_ready(cx)).await?;

    Ok(())
}

pub(super) enum AcceptedRecvStream<S>
where
    S: quic::RecvStream,
{
    Control(FrameStream<S>),
    Push(u64, FrameStream<S>),
    Encoder(S),
    Decoder(S),
    Reserved,
}

pub(super) struct AcceptRecvStream<S>
where
    S: quic::RecvStream,
{
    stream: S,
    ty: Option<StreamType>,
    push_id: Option<u64>,
    buf: BufList<S::Buf>,
    expected: Option<usize>,
}

impl<S> AcceptRecvStream<S>
where
    S: RecvStream,
{
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            ty: None,
            push_id: None,
            buf: BufList::new(),
            expected: None,
        }
    }

    pub fn into_stream(self) -> Result<AcceptedRecvStream<S>, Error> {
        Ok(match self.ty.expect("Stream type not resolved yet") {
            StreamType::CONTROL => {
                AcceptedRecvStream::Control(FrameStream::with_bufs(self.stream, self.buf))
            }
            StreamType::PUSH => AcceptedRecvStream::Push(
                self.push_id.expect("Push ID not resolved yet"),
                FrameStream::with_bufs(self.stream, self.buf),
            ),
            StreamType::ENCODER => AcceptedRecvStream::Encoder(self.stream),
            StreamType::DECODER => AcceptedRecvStream::Decoder(self.stream),
            t if t.value() > 0x21 && (t.value() - 0x21) % 0x1f == 0 => AcceptedRecvStream::Reserved,
            t => {
                return Err(Code::H3_STREAM_CREATION_ERROR
                    .with_reason(format!("unknown stream type 0x{:x}", t.value())))
            }
        })
    }

    pub fn poll_type(&mut self, cx: &mut Context) -> Poll<Result<(), Error>> {
        loop {
            match (self.ty.as_ref(), self.push_id) {
                // When accepting a Push stream, we want to parse two VarInts: [StreamType, PUSH_ID]
                (Some(&StreamType::PUSH), Some(_)) | (Some(_), _) => return Poll::Ready(Ok(())),
                _ => (),
            }

            match ready!(self.stream.poll_data(cx))? {
                Some(b) => self.buf.push(b),
                None => {
                    return Poll::Ready(Err(Code::H3_STREAM_CREATION_ERROR
                        .with_reason("Stream closed before type received")))
                }
            };

            if self.expected.is_none() && self.buf.remaining() >= 1 {
                self.expected = Some(VarInt::encoded_size(self.buf.chunk()[0]));
            }

            if let Some(expected) = self.expected {
                if self.buf.remaining() < expected {
                    continue;
                }
            } else {
                continue;
            }

            if self.ty.is_none() {
                // Parse StreamType
                self.ty = Some(StreamType::decode(&mut self.buf).map_err(|_| {
                    Code::H3_INTERNAL_ERROR.with_reason("Unexpected end parsing stream type")
                })?);
                // Get the next VarInt for PUSH_ID on the next iteration
                self.expected = None;
            } else {
                // Parse PUSH_ID
                self.push_id = Some(self.buf.get_var().map_err(|_| {
                    Code::H3_INTERNAL_ERROR.with_reason("Unexpected end parsing stream type")
                })?);
            }
        }
    }
}
