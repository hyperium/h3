mod buf;

use std::{
    io,
    task::{Context, Poll},
};

use bytes::{Buf, Bytes};

use crate::{
    proto::{
        frame::{self, Frame, FrameHeader, PartialData},
        ErrorCode,
    },
    quic::RecvStream,
};
use buf::BufList;

pub struct FrameStream<R: RecvStream> {
    recv: R,
    bufs: BufList<Bytes>,
    decoder: FrameDecoder,
}

impl<R> FrameStream<R>
where
    R: RecvStream<Buf = Bytes>,
{
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Frame>, Error>> {
        let end = match self.recv.poll_data(cx) {
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into().into())),
            Poll::Pending => false,
            Poll::Ready(Ok(None)) => true,
            Poll::Ready(Ok(Some(d))) => {
                self.bufs.push(d);
                false
            }
        };

        match self.decoder.decode(&mut self.bufs)? {
            Some(frame) => Poll::Ready(Ok(Some(frame))),
            None if end => Poll::Ready(Ok(None)),
            None => Poll::Pending,
        }
    }

    pub fn reset(&mut self, error_code: ErrorCode) {
        let _ = self.recv.stop_sending(error_code.0.into());
    }
}

#[derive(Default)]
pub struct FrameDecoder {
    partial: Option<PartialData>,
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
        if !src.has_remaining() {
            return Ok(None);
        }

        if let Some(ref mut partial) = self.partial {
            let (pos, frame) = decode!(src, |cur| Frame::Data(partial.decode_data(cur)));
            src.advance(pos);

            if partial.remaining() == 0 {
                self.partial = None;
            }

            return Ok(Some(frame));
        }

        if let Some(min) = self.expected {
            if src.remaining() < min {
                return Ok(None);
            }
        }

        let (pos, decoded) = decode!(src, |cur| Frame::decode(cur));

        match decoded {
            Err(frame::Error::IncompleteData) => {
                let (pos, decoded) = decode!(src, |cur| PartialData::decode(cur));
                let (partial, frame) = decoded?;
                src.advance(pos);
                self.expected = None;
                self.partial = Some(partial);
                if frame.len() > 0 {
                    Ok(Some(Frame::Data(frame)))
                } else {
                    Ok(None)
                }
            }
            Err(frame::Error::UnknownFrame(_)) => {
                src.advance(pos);
                self.expected = None;
                Ok(None)
            }
            Err(frame::Error::Incomplete(min)) => {
                self.expected = Some(min);
                Ok(None)
            }
            Err(e) => Err(e.into()),
            Ok(frame) => {
                src.advance(pos);
                self.expected = None;
                Ok(Some(frame))
            }
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Proto(frame::Error),
    Io(io::Error),
}

impl Error {
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Io(_) => ErrorCode::GENERAL_PROTOCOL_ERROR,
            Error::Proto(frame::Error::Settings(_)) => ErrorCode::SETTINGS_ERROR,
            Error::Proto(frame::Error::UnsupportedFrame(_)) => ErrorCode::FRAME_UNEXPECTED,
            Error::Proto(_) => ErrorCode::FRAME_ERROR,
        }
    }
}

impl From<frame::Error> for Error {
    fn from(err: frame::Error) -> Self {
        Error::Proto(err)
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::frame;
    use assert_matches::assert_matches;
    use bytes::BytesMut;

    #[test]
    fn one_frame() {
        let frame = frame::HeadersFrame {
            encoded: b"salut"[..].into(),
        };

        let mut buf = BytesMut::with_capacity(16);
        frame.encode(&mut buf);
        let mut buf = BufList::from(buf);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf), Ok(Some(Frame::Headers(_))));
    }

    #[test]
    fn incomplete_frame() {
        let frame = frame::HeadersFrame {
            encoded: b"salut"[..].into(),
        };

        let mut buf = BytesMut::with_capacity(16);
        frame.encode(&mut buf);
        buf.truncate(buf.len() - 1);
        let mut buf = BufList::from(buf);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf), Ok(None));
    }

    #[test]
    fn header_spread_multiple_buf() {
        let frame = frame::HeadersFrame {
            encoded: b"salut"[..].into(),
        };

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
        let frame = frame::HeadersFrame {
            encoded: payload.into(),
        };

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
        let frames = [
            Frame::Headers(frame::HeadersFrame {
                encoded: b"header"[..].into(),
            }),
            Frame::Data(frame::DataFrame {
                payload: b"body"[..].into(),
            }),
            Frame::Headers(frame::HeadersFrame {
                encoded: b"trailer"[..].into(),
            }),
        ];

        let mut buf = BytesMut::with_capacity(64);
        for frame in frames.iter() {
            frame.encode(&mut buf);
        }
        buf.truncate(buf.len() - 1);
        let mut buf = BufList::from(buf);

        let mut decoder = FrameDecoder::default();
        assert_matches!(decoder.decode(&mut buf), Ok(Some(Frame::Headers(_))));
        assert_matches!(decoder.decode(&mut buf), Ok(Some(Frame::Data(_))));
        assert_matches!(decoder.decode(&mut buf), Ok(None));
    }
}
