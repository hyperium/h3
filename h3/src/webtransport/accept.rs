use bytes::Buf;
use futures_util::ready;
use futures_util::Future;
use std::mem;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

use crate::frame::FrameStream;
use crate::frame::FrameStreamError;
use crate::proto::coding::BufExt;
use crate::proto::frame::Frame;
use crate::proto::frame::PayloadLen;
use crate::proto::stream::StreamType;
use crate::quic::BidiStream;
use crate::{
    buf::Cursor,
    error::{Code, ErrorLevel},
    proto::varint::VarInt,
    quic::Connection,
    stream::BufRecvStream,
    Error,
};

use super::stream::RecvStream;
use super::SessionId;

/// Resolves to a request or a webtransport bi stream
#[pin_project::pin_project]
pub(crate) struct AcceptBi<C: Connection> {
    stream: Option<FrameStream<C::BidiStream>>,
}

impl<C: Connection> AcceptBi<C> {
    pub(crate) fn new(stream: FrameStream<C::BidiStream>) -> Self {
        Self {
            stream: Some(stream),
        }
    }
}

const WEBTRANSPORT_BI: u64 = 0x41;

impl<C: Connection> Future for AcceptBi<C> {
    type Output = (
        Result<Option<Frame<PayloadLen>>, FrameStreamError>,
        FrameStream<C::BidiStream>,
    );

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let stream = self.stream.as_mut().unwrap();

        let frame = ready!(stream.poll_next(cx));

        let stream = Option::take(&mut self.stream).unwrap();

        match frame {
            Ok(Some(frame)) => Poll::Ready(((Ok(Some(frame))), stream)),
            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.1
            //# If a client-initiated
            //# stream terminates without enough of the HTTP message to provide a
            //# complete response, the server SHOULD abort its response stream with
            //# the error code H3_REQUEST_INCOMPLETE.
            Ok(None) => Poll::Ready((Ok(None), stream)),
            Err(err) => Poll::Ready(((Err(err)), stream)),
        }
    }
}

enum DecodedStreamType {
    WebTransport(SessionId),
    Framed,
}

/// Decodes the stream
fn decode_stream(mut buf: impl Buf) -> Result<DecodedStreamType, usize> {
    use crate::proto::coding::Decode;
    let rem = buf.remaining() + 1;

    // Get the type
    let ty = buf.get_var().map_err(|_| rem + 1)?;

    if ty == WEBTRANSPORT_BI {
        let session_id = SessionId::decode(&mut buf).map_err(|_| rem + 2)?;

        Ok(DecodedStreamType::WebTransport(session_id))
    } else {
        Ok(DecodedStreamType::Framed)
    }
}
