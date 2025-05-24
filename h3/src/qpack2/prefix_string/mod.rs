mod bitwin;
mod decode;
//mod encode;
/*
use std::convert::TryInto;
use std::fmt;
use std::num::TryFromIntError;

use bytes::{Buf, BufMut};

pub use self::bitwin::BitWindow;

pub use self::{
    decode::{HuffmanDecodingError, HpackStringDecode},
    encode::{HuffmanEncodingError, HpackStringEncode},
};

use crate::proto::coding::BufMutExt;


#[derive(Debug, PartialEq)]
pub enum PrefixStringError {
    UnexpectedEnd,
    Integer(IntegerError),
    HuffmanDecoding(HuffmanDecodingError),
    HuffmanEncoding(HuffmanEncodingError),
    BufSize(TryFromIntError),
}

impl std::fmt::Display for PrefixStringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PrefixStringError::UnexpectedEnd => write!(f, "unexpected end"),
            PrefixStringError::Integer(e) => write!(f, "could not parse integer: {}", e),
            PrefixStringError::HuffmanDecoding(e) => write!(f, "Huffman decode failed: {:?}", e),
            PrefixStringError::HuffmanEncoding(e) => write!(f, "Huffman encode failed: {:?}", e),
            PrefixStringError::BufSize(_) => write!(f, "number in buffer wrong size"),
        }
    }
}


pub fn decode<B: Buf>(size: u8, buf: &mut B) -> Result<Option<Vec<u8>>, PrefixStringDecoderError> {
    let (flags, len) = match prefix_int::decode(size - 1, buf){
        Ok(Some((flags, len))) => (flags, len),
        Ok(None) => return Ok(None),
        Err(e) => return Err(PrefixStringError::Integer(e)),
    };

    let len: usize = len.try_into()?;
    if buf.remaining() < len {
        return Err(PrefixStringError::UnexpectedEnd);
    }

    let payload = buf.copy_to_bytes(len);
    let value = if flags & 1 == 0 {
        payload.into_iter().collect()
    } else {
        let mut decoded = Vec::new();
        for byte in payload.into_iter().collect::<Vec<u8>>().hpack_decode() {
            decoded.push(byte?);
        }
        decoded
    };
    Ok(value)
}*/

/*

pub fn encode<B: BufMut>(size: u8, flags: u8, value: &[u8], buf: &mut B) -> Result<(), PrefixStringError> {
    let encoded = Vec::from(value).hpack_encode()?;
    prefix_int::encode(size - 1, flags << 1 | 1, encoded.len().try_into()?, buf);
    for byte in encoded {
        buf.write(byte);
    }
    Ok(())
}

impl From<HuffmanEncodingError> for PrefixStringError {
    fn from(error: HuffmanEncodingError) -> Self {
        PrefixStringError::HuffmanEncoding(error)
    }
}

impl From<IntegerError> for PrefixStringError {
    fn from(error: IntegerError) -> Self {
        match error {
            IntegerError::UnexpectedEnd => PrefixStringError::UnexpectedEnd,
            e => PrefixStringError::Integer(e),
        }
    }
}

impl From<HuffmanDecodingError> for PrefixStringError {
    fn from(error: HuffmanDecodingError) -> Self {
        PrefixStringError::HuffmanDecoding(error)
    }
}

impl From<TryFromIntError> for PrefixStringError {
    fn from(error: TryFromIntError) -> Self {
        PrefixStringError::BufSize(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use std::io::Cursor;

    #[test]
    fn codec_6() {
        let mut buf = Vec::new();
        encode(6, 0b01, b"name without ref", &mut buf).unwrap();
        let mut read = Cursor::new(&buf);
        assert_eq!(
            &buf,
            &[
                0b0110_1100,
                168,
                116,
                149,
                79,
                6,
                76,
                231,
                181,
                42,
                88,
                89,
                127
            ]
        );
        assert_eq!(decode(6, &mut read).unwrap(), b"name without ref");
    }

    #[test]
    fn codec_8() {
        let mut buf = Vec::new();
        encode(8, 0b01, b"name with ref", &mut buf).unwrap();
        let mut read = Cursor::new(&buf);
        assert_eq!(
            &buf,
            &[0b1000_1010, 168, 116, 149, 79, 6, 76, 234, 88, 89, 127]
        );
        assert_eq!(decode(8, &mut read).unwrap(), b"name with ref");
    }

    #[test]
    fn codec_8_empty() {
        let mut buf = Vec::new();
        encode(8, 0b01, b"", &mut buf).unwrap();
        let mut read = Cursor::new(&buf);
        assert_eq!(&buf, &[0b1000_0000]);
        assert_eq!(decode(8, &mut read).unwrap(), b"");
    }

    #[test]
    fn decode_non_huffman() {
        let buf = vec![0b0100_0011, b'b', b'a', b'r'];
        let mut read = Cursor::new(&buf);
        assert_eq!(decode(6, &mut read).unwrap(), b"bar");
    }

    #[test]
    fn decode_too_short() {
        let buf = vec![0b0100_0011, b'b', b'a'];
        let mut read = Cursor::new(&buf);
        assert_matches!(decode(6, &mut read), Err(PrefixStringError::UnexpectedEnd));
    }
}
*/