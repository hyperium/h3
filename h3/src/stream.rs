use std::{
    io,
    task::{Context, Poll},
};

use bytes::{Bytes, BytesMut};
use futures::{future, ready};
use quic::RecvStream;

use crate::{
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

    stream.send_data(buf.freeze()).map_err(Error::transport)?;
    future::poll_fn(|cx| stream.poll_ready(cx))
        .await
        .map_err(Error::transport)?;

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
    buf: [u8; VarInt::MAX_SIZE],
    expected: usize,
    len: usize,
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
            buf: [0; VarInt::MAX_SIZE],
            expected: 1,
            len: 0,
        }
    }

    pub fn into_stream(self) -> Result<AcceptedRecvStream<S>, Error> {
        Ok(match self.ty.expect("Stream type not resolved yet") {
            StreamType::CONTROL => AcceptedRecvStream::Control(FrameStream::new(self.stream)),
            StreamType::PUSH => AcceptedRecvStream::Push(
                self.push_id.expect("Push ID not resolved yet"),
                FrameStream::new(self.stream),
            ),
            StreamType::ENCODER => AcceptedRecvStream::Encoder(self.stream),
            StreamType::DECODER => AcceptedRecvStream::Decoder(self.stream),
            t if t.0 > 0x21 && (t.0 - 0x21) % 0x1f == 0 => AcceptedRecvStream::Reserved,
            t => {
                return Err(Code::H3_STREAM_CREATION_ERROR
                    .with_reason(format!("unknown stream type 0x{:x}", t.0)))
            }
        })
    }

    pub fn poll_type(&mut self, cx: &mut Context) -> Poll<Result<(), Error>> {
        loop {
            match (self.ty.as_ref(), self.push_id) {
                (Some(&StreamType::PUSH), Some(_)) | (Some(_), _) => return Poll::Ready(Ok(())),
                _ => (),
            }

            match ready!(self
                .stream
                .poll_read(&mut self.buf[self.len..self.expected], cx))
            .map_err(Error::transport)?
            {
                Some(s) => self.len += s,
                None => {
                    return Poll::Ready(Err(Code::H3_STREAM_CREATION_ERROR
                        .with_reason("Stream closed before type received")))
                }
            };

            if self.len == 1 {
                self.expected = VarInt::encoded_size(self.buf[0]);
            }
            if self.len < self.expected {
                continue;
            }

            let mut cur = io::Cursor::new(&self.buf);
            if self.ty.is_none() {
                self.ty = Some(StreamType::decode(&mut cur).map_err(|_| {
                    Code::H3_INTERNAL_ERROR.with_reason("Unexpected end parsing stream type")
                })?);
                // Get the next VarInt for PUSH_ID on the next iteration
                self.len = 0;
                self.expected = 1;
            } else {
                self.push_id = Some(cur.get_var().map_err(|_| {
                    Code::H3_INTERNAL_ERROR.with_reason("Unexpected end parsing stream type")
                })?);
            }
        }
    }
}
