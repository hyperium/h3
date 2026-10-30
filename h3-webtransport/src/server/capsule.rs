use bytes::{Buf, BufMut, Bytes, BytesMut};
use h3::proto::varint::VarInt;

use super::SessionClose;

const CLOSE_SESSION: u64 = 0x2843;
const MAX_REASON: usize = 1024;

pub(super) fn encode(code: u32, reason: &str) -> Result<Bytes, &'static str> {
    if reason.len() > MAX_REASON {
        return Err("WebTransport close reason exceeds 1024 bytes");
    }
    let mut wire = BytesMut::new();
    VarInt::from_u32(CLOSE_SESSION as u32).encode(&mut wire);
    VarInt::from_u32(4 + reason.len() as u32).encode(&mut wire);
    wire.put_u32(code);
    wire.extend_from_slice(reason.as_bytes());
    Ok(wire.freeze())
}

/// Incremental capsule framing. Unknown values are skipped without allocation.
#[derive(Default)]
pub(super) struct Decoder {
    integer: Vec<u8>,
    kind: Option<u64>,
    remaining: Option<u64>,
    payload: Vec<u8>,
    closed: bool,
}

impl Decoder {
    pub(super) fn feed(
        &mut self,
        mut data: impl Buf,
    ) -> Result<Option<SessionClose>, &'static str> {
        let mut close = None;
        while data.has_remaining() {
            if self.closed {
                return Err("data after WebTransport close capsule");
            }
            if self.remaining.is_none() {
                self.integer.push(data.get_u8());
                if self.integer.len() != VarInt::encoded_size(self.integer[0]) {
                    continue;
                }
                let value = VarInt::decode(&mut &self.integer[..]).unwrap().into_inner();
                self.integer.clear();
                if self.kind.is_none() {
                    self.kind = Some(value);
                    continue;
                }
                if self.kind == Some(CLOSE_SESSION) && !(4..=4 + MAX_REASON as u64).contains(&value)
                {
                    return Err("invalid WebTransport close capsule length");
                }
                self.remaining = Some(value);
            }
            let remaining = self.remaining.unwrap();
            let take = remaining.min(data.remaining() as u64) as usize;
            if self.kind == Some(CLOSE_SESSION) {
                let start = self.payload.len();
                self.payload.resize(start + take, 0);
                data.copy_to_slice(&mut self.payload[start..]);
            } else {
                data.advance(take);
            }
            self.remaining = Some(remaining - take as u64);
            if self.remaining == Some(0) {
                if self.kind == Some(CLOSE_SESSION) {
                    let code = u32::from_be_bytes(self.payload[..4].try_into().unwrap());
                    let reason = std::str::from_utf8(&self.payload[4..])
                        .map_err(|_| "WebTransport close reason is not UTF-8")?;
                    close = Some(SessionClose {
                        code,
                        reason: reason.to_owned(),
                    });
                    self.closed = true;
                }
                self.kind = None;
                self.remaining = None;
            }
        }
        Ok(close)
    }

    pub(super) fn finish(&self) -> Result<(), &'static str> {
        if self.integer.is_empty() && self.kind.is_none() && self.remaining.is_none() {
            Ok(())
        } else {
            Err("incomplete WebTransport capsule")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_capsules_are_skipped_and_close_is_decoded_byte_by_byte() {
        let mut decoder = Decoder::default();
        let mut wire = BytesMut::new();
        // Non-minimal varints are legal, including a zero-length unknown capsule.
        wire.extend_from_slice(b"\xc0\0\0\0\0\0\0\x17\x40\x00");
        wire.extend_from_slice(&encode(u32::MAX, "closed 🎭").unwrap());
        let mut close = None;
        for byte in wire.iter() {
            close = decoder.feed(&[*byte][..]).unwrap().or(close);
        }
        assert_eq!(
            close.unwrap(),
            SessionClose {
                code: u32::MAX,
                reason: "closed 🎭".into()
            }
        );
        decoder.finish().unwrap();
        assert!(decoder.feed(&b"x"[..]).is_err());
    }

    #[test]
    fn unknown_payload_does_not_allocate_and_truncation_is_an_error() {
        let mut decoder = Decoder::default();
        let mut wire = BytesMut::new();
        VarInt::from_u32(0x17).encode(&mut wire);
        VarInt::MAX.encode(&mut wire);
        wire.extend_from_slice(&[0; 128]);
        assert!(decoder.feed(wire.freeze()).unwrap().is_none());
        assert_eq!(decoder.payload.capacity(), 0);
        assert!(decoder.finish().is_err());
    }

    #[test]
    fn reason_length_and_utf8_are_checked() {
        assert!(encode(0, &"é".repeat(512)).is_ok());
        assert!(encode(0, &"é".repeat(513)).is_err());
        let mut decoder = Decoder::default();
        assert!(decoder.feed(&b"\x68\x43\x44\x05"[..]).is_err());
        let mut decoder = Decoder::default();
        assert!(decoder.feed(&b"\x68\x43\x05\0\0\0\0\xff"[..]).is_err());
    }
}
