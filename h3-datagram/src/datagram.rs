use bytes::{Buf, Bytes};
use h3::{
    error::{internal_error::InternalConnectionError, Code},
    proto::varint::VarInt,
    quic::StreamId,
};

/// HTTP datagram frames
/// See: <https://www.rfc-editor.org/rfc/rfc9297#section-2.1>
pub struct Datagram<B = Bytes> {
    /// Stream id divided by 4
    stream_id: StreamId,
    /// The data contained in the datagram
    payload: B,
}

impl<B> Datagram<B>
where
    B: Buf,
{
    /// Creates a new datagram frame
    pub fn new(stream_id: StreamId, payload: B) -> Self {
        assert!(
            stream_id.into_inner() % 4 == 0,
            "StreamId is not divisible by 4"
        );
        // TODO: does stream_id need to be devided by 4?
        Self { stream_id, payload }
    }

    /// Decodes a datagram frame from the QUIC datagram
    pub fn decode(mut buf: B) -> Result<Self, InternalConnectionError> {
        let q_stream_id = VarInt::decode(&mut buf).map_err(|_| {
            InternalConnectionError::new(Code::H3_DATAGRAM_ERROR, "invalid stream id".to_string())
        })?;

        //= https://www.rfc-editor.org/rfc/rfc9297#section-2.1
        // Quarter Stream ID: A variable-length integer that contains the value of the client-initiated bidirectional
        // stream that this datagram is associated with divided by four (the division by four stems
        // from the fact that HTTP requests are sent on client-initiated bidirectional streams,
        // which have stream IDs that are divisible by four). The largest legal QUIC stream ID
        // value is 262-1, so the largest legal value of the Quarter Stream ID field is 260-1.
        // Receipt of an HTTP/3 Datagram that includes a larger value MUST be treated as an HTTP/3
        // connection error of type H3_DATAGRAM_ERROR (0x33).
        let stream_id = StreamId::try_from(u64::from(q_stream_id) * 4).map_err(|_| {
            InternalConnectionError::new(Code::H3_DATAGRAM_ERROR, "invalid stream id".to_string())
        })?;

        let payload = buf;

        Ok(Self { stream_id, payload })
    }

    #[inline]
    /// Returns the associated stream id of the datagram
    pub fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    #[inline]
    /// Returns the datagram payload
    pub fn payload(&self) -> &B {
        &self.payload
    }

    /// Encode the datagram to wire format
    pub fn encode<D: bytes::BufMut>(self, buf: &mut D) {
        (VarInt::from(self.stream_id) / 4).encode(buf);
        buf.put(self.payload);
    }

    /// Returns the datagram payload
    pub fn into_payload(self) -> B {
        self.payload
    }
}
