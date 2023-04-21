use std::convert::TryFrom;

use bytes::{Buf, Bytes};

use crate::{error::Code, Error};

use super::{stream::StreamId, varint::VarInt};

/// HTTP datagram frames
/// See: https://www.rfc-editor.org/rfc/rfc9297#section-2.1
pub struct Datagram<B = Bytes> {
    /// Stream id divided by 4
    stream_id: StreamId,
    /// The data contained in the datagram
    pub payload: B,
}

impl<B> Datagram<B>
where
    B: Buf,
{
    pub fn new(stream_id: StreamId, payload: B) -> Self {
        assert!(
            stream_id.into_inner() % 4 == 0,
            "StreamId is not divisible by 4"
        );
        Self { stream_id, payload }
    }

    /// Decodes a datagram frame from the QUIC datagram
    pub fn decode(mut buf: B) -> Result<Self, Error> {
        let q_stream_id = VarInt::decode(&mut buf)
            .map_err(|_| Code::H3_DATAGRAM_ERROR.with_cause("Malformed datagram frame"))?;

        //= https://www.rfc-editor.org/rfc/rfc9297#section-2.1
        // Quarter Stream ID: A variable-length integer that contains the value of the client-initiated bidirectional
        // stream that this datagram is associated with divided by four (the division by four stems
        // from the fact that HTTP requests are sent on client-initiated bidirectional streams,
        // which have stream IDs that are divisible by four). The largest legal QUIC stream ID
        // value is 262-1, so the largest legal value of the Quarter Stream ID field is 260-1.
        // Receipt of an HTTP/3 Datagram that includes a larger value MUST be treated as an HTTP/3
        // connection error of type H3_DATAGRAM_ERROR (0x33).
        let stream_id = StreamId::try_from(u64::from(q_stream_id) * 4)
            .map_err(|_| Code::H3_DATAGRAM_ERROR.with_cause("Invalid stream id"))?;

        let payload = buf;

        Ok(Self { stream_id, payload })
    }

    #[inline]
    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    #[inline]
    pub fn payload(&self) -> &B {
        &self.payload
    }

    pub fn encode<D: bytes::BufMut>(self, buf: &mut D) {
        (VarInt::from(self.stream_id) / 4).encode(buf);
        buf.put(self.payload);
    }
}
