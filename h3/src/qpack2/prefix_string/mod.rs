mod bitwin;

/// Huffman decoding utilities
mod huffman_decode;

/// Huffman encoding utilities
mod huffman_encode;

/// Encoding prefix_strings
mod encode;

/// Decoding prefix_strings
mod decode;

/*
use std::convert::TryInto;
use std::fmt;
use std::num::TryFromIntError;

use bytes::{Buf, BufMut};

pub use self::bitwin::BitWindow;

pub use self::{
    huffman_decode::{HuffmanDecodingError, HpackStringDecode},
    huffman_encode::{HuffmanEncodingError, HpackStringEncode},
};

use crate::proto::coding::BufMutExt;

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