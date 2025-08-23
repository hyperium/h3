use bytes::Buf;
use thiserror::Error;

use crate::{
    proto::coding::BufExt,
    qpack2::qpack_result::{ParseProgressResult, StatefulParser},
};

#[derive(Debug, PartialEq, Error)]
pub enum PrefixIntParseError {
    #[error("value overflow")]
    Overflow,
}

/// PrefixIntParser is a parser for prefix integers.
/// It is used to decode and encode prefix integers in the QPACK format.
#[derive(Debug)]
pub struct PrefixIntParser<const F: u8> {
    value: u64,
    position: u8,
    flags: Option<u8>,
}

/// Creates a new PrefixIntParser with the given size.
/// The size is the number of bits used to encode the prefix integer.
/// The size must be between 1 and 8.
fn new_prefix_int_parser<const F2: u8>() -> PrefixIntParser<F2> {
    // TODO: maybe add compile-time check for F2
    assert!(F2 <= 8);

    PrefixIntParser {
        value: 0,
        position: 0,
        flags: None,
    }
}

impl<const F: u8> StatefulParser<PrefixIntParseError, (u8, u64)> for PrefixIntParser<F> {
    fn parse_progress<B: Buf>(
        mut self,
        buf: &mut B,
    ) -> ParseProgressResult<Self, PrefixIntParseError, (u8, u64)> {
        if self.flags.is_none() {
            // Get the first byte from the buffer
            let first = match buf.get::<u8>() {
                Ok(value) => value,
                Err(_) => return ParseProgressResult::MoreData(self),
            };
            // Get the flags from the first byte
            let flags = (first as usize >> F) as u8;
            // Get the value from the first byte
            let value = first & Self::MASK;
            // Check if the value is less than the mask
            if value < Self::MASK {
                // The whole value is in the first byte
                return ParseProgressResult::Done((flags, value as u64));
            } else {
                // The value is larger than the mask, so we need to read more bytes
                self.flags = Some(flags);
                self.value = Self::MASK as u64;
            }
        }

        loop {
            // Get the next byte from the buffer
            let byte = match buf.get::<u8>() {
                Ok(value) => value,
                Err(_) => return ParseProgressResult::MoreData(self),
            } as u64;

            // Add the value of the byte to the prefix integer
            self.value += (byte & 127) << self.position;
            // Increment the position
            self.position += 7;

            // Check if the byte is the last byte
            if byte & 128 == 0 {
                // The whole value is in the buffer
                let flags = self.flags.take().expect("we must have flags here");
                return ParseProgressResult::Done((flags, self.value));
            }

            // Overflow check
            if self.position >= MAX_POWER {
                // The value is too large, so we need to return an error
                return ParseProgressResult::Error(PrefixIntParseError::Overflow);
            }
        }
    }
}

impl<const F: u8> PrefixIntParser<F> {
    const MASK: u8 = 0xFF >> (8 - F);
}

//= https://www.rfc-editor.org/rfc/rfc9204.html#section-4.1.1
//# QPACK implementations MUST be able to decode integers up to and
//# including 62 bits long.
const MAX_POWER: u8 = 9 * 7;

#[cfg(test)]
mod test {
    use std::{io::Cursor, panic};

    use crate::tests::test_all_chunking_combinations;

    use super::*;
    use assert_matches::assert_matches;

    fn check_codec<const F: u8>(flags: u8, value: u64, data: &[u8]) {
        test_all_chunking_combinations(
            &mut Cursor::new(data),
            || new_prefix_int_parser::<F>(),
            false,
            ParseProgressResult::Done((flags, value)),
        );
    }

    fn check_overflow<const F: u8>(data: &[u8]) {
        test_all_chunking_combinations(
            &mut Cursor::new(data),
            || new_prefix_int_parser::<F>(),
            false,
            ParseProgressResult::Error(PrefixIntParseError::Overflow),
        );
    }

    #[test]
    fn codec_5_bits() {
        check_codec::<5>(0b101, 10, &[0b1010_1010]);
        check_codec::<5>(0b101, 0, &[0b1010_0000]);
        check_codec::<5>(0b010, 1337, &[0b0101_1111, 154, 10]);
        check_codec::<5>(0b010, 31, &[0b0101_1111, 0]);
        check_codec::<5>(
            0b010,
            0x80_00_00_00_00_00_00_1E,
            &[95, 255, 255, 255, 255, 255, 255, 255, 255, 127],
        );
    }

    #[test]
    fn codec_8_bits() {
        check_codec::<8>(0, 42, &[0b0010_1010]);
        check_codec::<8>(0, 424_242, &[255, 179, 240, 25]);
        check_codec::<8>(
            0,
            0x80_00_00_00_00_00_00_FE,
            &[255, 255, 255, 255, 255, 255, 255, 255, 255, 127],
        );
    }

    #[test]
    #[should_panic]
    fn size_too_big_of_size() {
        let _ = new_prefix_int_parser::<9>();
    }

    #[test]
    fn overflow_1() {
        check_overflow::<1>(&[255, 128, 254, 255, 255, 255, 255, 255, 255, 255, 255, 1]);
    }
    #[test]
    fn overflow_2() {
        check_overflow::<2>(&[255, 128, 254, 255, 255, 255, 255, 255, 255, 255, 255, 1]);
    }
    #[test]
    fn overflow_3() {
        check_overflow::<3>(&[255, 128, 254, 255, 255, 255, 255, 255, 255, 255, 255, 1]);
    }
    #[test]
    fn overflow_4() {
        check_overflow::<4>(&[255, 128, 254, 255, 255, 255, 255, 255, 255, 255, 255, 1]);
    }
    #[test]
    fn overflow_5() {
        check_overflow::<5>(&[255, 128, 254, 255, 255, 255, 255, 255, 255, 255, 255, 1]);
    }
    #[test]
    fn overflow_6() {
        check_overflow::<6>(&[255, 128, 254, 255, 255, 255, 255, 255, 255, 255, 255, 1]);
    }
    #[test]
    fn overflow_7() {
        check_overflow::<7>(&[255, 128, 254, 255, 255, 255, 255, 255, 255, 255, 255, 1]);
    }
    #[test]
    fn overflow_8() {
        check_overflow::<8>(&[255, 128, 254, 255, 255, 255, 255, 255, 255, 255, 255, 1]);
    }

    #[test]
    fn number_never_ends_with_0x80() {
        check_codec::<4>(0b0001, 143, &[31, 128, 1]);
    }

    #[test]
    fn test_prefix_int_parser() {
        let mut buf = &[0b011_00010][..];
        let parser = new_prefix_int_parser::<5>();
        let result = parser.parse_progress(&mut buf);
        assert_matches!(result, ParseProgressResult::Done((0b011, 0b00010)));
    }

    #[test]
    fn test_prefix_int_two_buffer() {
        let mut buf = &[0b011_11111][..];
        let parser = new_prefix_int_parser::<5>();
        let parser = match parser.parse_progress(&mut buf) {
            ParseProgressResult::MoreData(parser) => parser,
            _ => panic!("Expected MoreData"),
        };
        let mut buffer2 = &[0b00000001][..];
        let result = parser.parse_progress(&mut buffer2);
        assert_matches!(result, ParseProgressResult::Done((0b011, 32)));
    }

    #[test]
    fn overflow_x() {
        check_overflow::<5>(&[95, 225, 255, 255, 255, 255, 255, 255, 255, 255, 1]);
    }

    #[test]
    fn allow_62_bit() {
        //= https://www.rfc-editor.org/rfc/rfc9204.html#section-4.1.1
        //= type=test
        //# QPACK implementations MUST be able to decode integers up to and
        //# including 62 bits long.

        // This is the maximum value that can be encoded in with a flag size of 7 bits
        // The value is requires more than 62 bits so the spec is fulfilled
        check_codec::<1>(
            1,
            9223372036854775808,
            &[3, 255, 255, 255, 255, 255, 255, 255, 255, 127],
        );
    }
}
