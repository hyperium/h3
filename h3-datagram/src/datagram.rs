use bytes::Buf;
use h3::{
    error::{internal_error::InternalConnectionError, Code},
    proto::varint::VarInt,
    quic::StreamId,
};

/// HTTP datagram frames
/// See: <https://www.rfc-editor.org/rfc/rfc9297#section-2.1>
#[derive(Debug, Clone)]
pub struct Datagram<B> {
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
    // TODO: remove for MSRV >= 1.87 https://github.com/rust-lang/rust/issues/128101
    #[allow(unknown_lints, clippy::manual_is_multiple_of)]
    pub fn new(stream_id: StreamId, payload: B) -> Self {
        assert!(
            stream_id.into_inner() % 4 == 0,
            "StreamId is not divisible by 4"
        );
        // StreamId will be divided by 4 when encoding the Datagram header
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
    pub fn encode(self) -> EncodedDatagram<B> {
        let mut buffer = [0; VarInt::MAX_SIZE];
        let varint = VarInt::from(self.stream_id) / 4;
        varint.encode(&mut buffer.as_mut_slice());
        EncodedDatagram {
            stream_id: [0; VarInt::MAX_SIZE],
            len: varint.size(),
            pos: 0,
            payload: self.payload,
        }
    }

    /// Returns the datagram payload
    pub fn into_payload(self) -> B {
        self.payload
    }
}

#[derive(Debug)]
pub struct EncodedDatagram<B: Buf> {
    /// Encoded datagram stream ID as Varint
    stream_id: [u8; VarInt::MAX_SIZE],
    /// Length of the varint
    len: usize,
    /// Position of the stream_id buffer
    pos: usize,
    /// Datagram Payload
    payload: B,
}

/// Implementation of [`Buf`] for [`Datagram`]
impl<B> Buf for EncodedDatagram<B>
where
    B: Buf,
{
    fn remaining(&self) -> usize {
        self.len - self.pos + self.payload.remaining()
    }

    fn chunk(&self) -> &[u8] {
        if self.len - self.pos > 0 {
            &self.stream_id[self.pos..self.len]
        } else {
            self.payload.chunk()
        }
    }

    fn advance(&mut self, mut cnt: usize) {
        let remaining_header = self.len - self.pos;
        if remaining_header > 0 {
            let advanced = usize::min(cnt, remaining_header);
            self.pos += advanced;
            cnt -= advanced;
        }
        self.payload.advance(cnt);
    }
}
